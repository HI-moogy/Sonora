//! Checks Windows' per-app output device (the "Output device" choice per
//! app in Settings > Sound > Volume mixer, which EarTrumpet also sets) on
//! this PC: a child plays clicks to "the default device", is pointed at
//! another device, and the clicks are timed there. Run once with the
//! choice made before the child opens its stream, and once while it's
//! already playing (does it follow?). The choice is cleared afterwards.
//!
//! Usage: direct_route_probe <part of a device name>

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use audio_engine::loopback::standard_format;
use audio_engine::{list_output_devices, ComGuard};
use windows::core::{HRESULT, HSTRING, PCWSTR};
use windows::Win32::Media::Audio::{
    eConsole, eMultimedia, eRender, EDataFlow, ERole, IAudioCaptureClient, IAudioClient, IAudioRenderClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::WinRT::RoGetActivationFactory;

const RATE: f64 = 48_000.0;
const CLICK_EVERY: Duration = Duration::from_millis(500);
const CLICK_FRAMES: usize = 240;
const AMPLITUDE: f32 = 0.02;

/// Windows' audio policy, as its own Settings app uses it (Windows 10 21H2
/// and later; undocumented). Only the last three methods are used. It's a
/// WinRT interface: the first three slots are IInspectable's.
#[windows_core::interface("ab3d4648-e242-459f-b02f-541c70306324")]
unsafe trait IAudioPolicyConfigFactory: windows_core::IUnknown {
    fn get_iids(&self) -> HRESULT;
    fn get_runtime_class_name(&self) -> HRESULT;
    fn get_trust_level(&self) -> HRESULT;
    fn add_ctx_volume_change(&self) -> HRESULT;
    fn remove_ctx_volume_changed(&self) -> HRESULT;
    fn add_ringer_vibrate_state_changed(&self) -> HRESULT;
    fn remove_ringer_vibrate_state_change(&self) -> HRESULT;
    fn set_volume_group_gain_for_id(&self) -> HRESULT;
    fn get_volume_group_gain_for_id(&self) -> HRESULT;
    fn get_active_volume_group_for_endpoint_id(&self) -> HRESULT;
    fn get_volume_groups_for_endpoint(&self) -> HRESULT;
    fn get_current_volume_context(&self) -> HRESULT;
    fn set_volume_group_mute_for_id(&self) -> HRESULT;
    fn get_volume_group_mute_for_id(&self) -> HRESULT;
    fn set_ringer_vibrate_state(&self) -> HRESULT;
    fn get_ringer_vibrate_state(&self) -> HRESULT;
    fn set_preferred_chat_application(&self) -> HRESULT;
    fn reset_preferred_chat_application(&self) -> HRESULT;
    fn get_preferred_chat_application(&self) -> HRESULT;
    fn get_current_chat_applications(&self) -> HRESULT;
    fn add_chat_context_changed(&self) -> HRESULT;
    fn remove_chat_context_changed(&self) -> HRESULT;
    fn set_persisted_default_audio_endpoint(&self, process: u32, flow: EDataFlow, role: ERole, device: *mut core::ffi::c_void) -> HRESULT;
    fn get_persisted_default_audio_endpoint(&self, process: u32, flow: EDataFlow, role: ERole, device: *mut *mut core::ffi::c_void) -> HRESULT;
    fn clear_all_persisted_application_default_endpoints(&self) -> HRESULT;
}

fn policy() -> windows::core::Result<IAudioPolicyConfigFactory> {
    unsafe { RoGetActivationFactory(&HSTRING::from("Windows.Media.Internal.AudioPolicyConfig")) }
}

/// Points process `pid`'s "default output" at `device_id` (an endpoint id),
/// or back at the real default with `None`.
fn set_app_output(policy: &IAudioPolicyConfigFactory, pid: u32, device_id: Option<&str>) -> windows::core::Result<()> {
    // The policy names endpoints by their device interface path.
    let device = device_id
        .map(|id| HSTRING::from(format!("\\\\?\\SWD#MMDEVAPI#{id}#{{e6327cad-dcec-4949-ae8a-991e976a79d2}}")))
        .unwrap_or_default();
    for role in [eConsole, eMultimedia] {
        unsafe {
            policy
                .set_persisted_default_audio_endpoint(pid, eRender, role, std::mem::transmute_copy(&device))
                .ok()?;
        }
    }
    Ok(())
}

fn app_output(policy: &IAudioPolicyConfigFactory, pid: u32) -> windows::core::Result<String> {
    let mut device: HSTRING = HSTRING::new();
    unsafe {
        policy
            .get_persisted_default_audio_endpoint(pid, eRender, eMultimedia, &mut device as *mut HSTRING as *mut _)
            .ok()?;
    }
    Ok(device.to_string())
}

fn now_hns() -> i64 {
    let (mut counter, mut frequency) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut counter);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    (counter as i128 * 10_000_000 / frequency as i128) as i64
}

/// Child: once told to (a line on stdin), plays clicks to the default
/// output device, printing each click's due time. Each further line closes
/// the stream and opens the default device again (as an app does when
/// Windows says its default device changed).
fn play_default() -> windows::core::Result<()> {
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    let (reopen_tx, reopen_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for _ in std::io::stdin().lines().map_while(Result::ok) {
            let _ = reopen_tx.send(());
        }
    });
    loop {
        play_until(&reopen_rx)?;
    }
}

fn play_until(reopen: &mpsc::Receiver<()>) -> windows::core::Result<()> {
    let (client, render): (IAudioClient, IAudioRenderClient) = unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let client: IAudioClient = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?.Activate(CLSCTX_ALL, None)?;
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, 2_000_000, 0, &standard_format(), None)?;
        let render = client.GetService()?;
        (client, render)
    };
    let size = unsafe { client.GetBufferSize()? };
    unsafe { client.Start()? };
    let (mut written, mut next_click) = (0u64, (RATE * 0.3) as u64);
    loop {
        if reopen.try_recv().is_ok() {
            unsafe { client.Stop()? };
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
        let padding = unsafe { client.GetCurrentPadding()? };
        let free = size - padding;
        if free == 0 {
            continue;
        }
        let samples = unsafe { std::slice::from_raw_parts_mut(render.GetBuffer(free)? as *mut f32, free as usize * 2) };
        let now = now_hns();
        for (i, frame) in samples.chunks_mut(2).enumerate() {
            let position = written + i as u64;
            let in_click = position >= next_click && position < next_click + CLICK_FRAMES as u64;
            if position == next_click {
                let due = now + ((padding as u64 + i as u64) as f64 / RATE * 1e7) as i64;
                println!("click {due}");
            }
            frame.fill(if in_click { AMPLITUDE } else { 0.0 });
            if position + 1 == next_click + CLICK_FRAMES as u64 {
                next_click += (CLICK_EVERY.as_secs_f64() * RATE) as u64;
            }
        }
        written += free as u64;
        unsafe { render.ReleaseBuffer(free, 0)? };
    }
}

/// When clicks start in what `device_id` plays over `seconds`.
fn click_onsets(device_id: &str, seconds: f64) -> windows::core::Result<Vec<i64>> {
    let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
    let client: IAudioClient = unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        enumerator.GetDevice(PCWSTR(wide.as_ptr()))?.Activate(CLSCTX_ALL, None)?
    };
    let mut onsets = Vec::new();
    let mut quiet_for = usize::MAX;
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            2_000_000,
            0,
            &standard_format(),
            None,
        )?;
        let service: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let start = Instant::now();
        while start.elapsed().as_secs_f64() < seconds {
            std::thread::sleep(Duration::from_millis(3));
            while service.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags, mut qpc) = (std::ptr::null_mut(), 0u32, 0u32, 0u64);
                service.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
                let samples = std::slice::from_raw_parts(data as *const f32, frames as usize * 2);
                for (i, s) in samples.iter().step_by(2).enumerate() {
                    if s.abs() > AMPLITUDE / 4.0 {
                        if quiet_for > 4_800 {
                            onsets.push(qpc as i64 + (i as f64 / RATE * 1e7) as i64);
                        }
                        quiet_for = 0;
                    } else {
                        quiet_for = quiet_for.saturating_add(1);
                    }
                }
                service.ReleaseBuffer(frames)?;
            }
        }
    }
    Ok(onsets)
}

/// The child, stopped however the probe ends.
struct Player(std::process::Child);

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One run: points a playing child at `device_id`; with `reopen`, the
/// child then opens its stream again.
fn run(policy: &IAudioPolicyConfigFactory, device_id: &str, reopen: bool) -> windows::core::Result<()> {
    let mut child = Player(
        Command::new(std::env::current_exe().unwrap())
            .arg("--play-default")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let pid = child.0.id();
    let stdout = child.0.stdout.take().unwrap();
    let (due_tx, due_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(due) = line.strip_prefix("click ").and_then(|d| d.parse::<i64>().ok()) {
                let _ = due_tx.send(due);
            }
        }
    });
    let mut control = child.0.stdin.take().unwrap();
    writeln!(control, "go").unwrap();
    std::thread::sleep(Duration::from_secs(1));
    set_app_output(policy, pid, Some(device_id))?;
    println!("  Windows now says the app plays on: {}", app_output(policy, pid)?);
    if reopen {
        writeln!(control, "reopen").unwrap();
    }
    std::thread::sleep(Duration::from_millis(500));
    let onsets = click_onsets(device_id, 5.0)?;
    set_app_output(policy, pid, None)?;
    let dues: Vec<i64> = due_rx.try_iter().collect();
    let mut delays: Vec<f64> = onsets
        .iter()
        .filter_map(|&onset| {
            let due = dues.iter().copied().filter(|&d| d <= onset).max()?;
            let delay = (onset - due) as f64 / 10_000.0;
            (delay < 1_000.0).then_some(delay)
        })
        .collect();
    delays.sort_by(f64::total_cmp);
    println!(
        "  {} clicks heard there, median {:.1} ms after due",
        delays.len(),
        delays.get(delays.len() / 2).copied().unwrap_or(f64::NAN)
    );
    Ok(())
}

fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--play-default") {
        return play_default();
    }
    let device = list_output_devices()?
        .into_iter()
        .find(|d| d.name.contains(args.first().expect("part of a device name").as_str()))
        .expect("no active output device with that name");
    let policy = policy()?;
    // A read-only look at apps playing now: what Windows has them set to.
    for pid in std::env::args().skip(2).filter_map(|pid| pid.parse::<u32>().ok()) {
        println!("process {pid} plays on: {:?}", app_output(&policy, pid));
    }
    println!("device: {}", device.name);
    println!("pointed there while playing, stream left open:");
    run(&policy, &device.id, false)?;
    println!("pointed there while playing, then the stream reopened:");
    run(&policy, &device.id, true)
}
