//! Checks Windows' per-app output device (the "Output device" choice per
//! app in Settings > Sound > Volume mixer, which EarTrumpet also sets) on
//! this PC: a child plays clicks to "the default device", is pointed at
//! another device, and the clicks are timed there. Run once with the
//! choice made while the child plays with its stream left open, and once
//! with it reopened. The choice is cleared afterwards. `mixer` instead
//! routes the child through Sonora's mixer to that device, then has it
//! reopen its audio: it should end up playing direct.
//!
//! The clicks are at -80 dB, inaudible on the default device the child
//! starts on, and still found in the captured output.
//!
//! `park` routes the child through the mixer with the EQ on, the device
//! named set as the silent output: once the child reopens its audio it
//! should play there at full volume, and a recording of the child (as a
//! Discord stream of its window makes) should hear it at full volume too.
//!
//! Usage: direct_route_probe <part of a device name> [mixer | park | recover <pid>]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use audio_engine::loopback::{activate_process_loopback_client, standard_format};
use audio_engine::{app_output, list_output_devices, ComGuard, EqBand, Mixer, DEFAULT_EQ_BANDS};
use common::Priority;
use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

const RATE: f64 = 48_000.0;
const CLICK_EVERY: Duration = Duration::from_millis(500);
const CLICK_FRAMES: usize = 240;
const AMPLITUDE: f32 = 1e-4;

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
                // Ignored once the probe is gone (a crash test kills it).
                let _ = writeln!(std::io::stdout(), "click {due}");
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

/// When clicks start in what `device_id` plays over `seconds`, and the
/// loudest sample.
fn click_onsets(device_id: &str, seconds: f64) -> windows::core::Result<(Vec<i64>, f32)> {
    let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
    let client: IAudioClient = unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        enumerator.GetDevice(PCWSTR(wide.as_ptr()))?.Activate(CLSCTX_ALL, None)?
    };
    let mut onsets = Vec::new();
    let mut peak = 0.0f32;
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
                    peak = peak.max(s.abs());
                    if s.abs() > AMPLITUDE / 8.0 {
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
    Ok((onsets, peak))
}

/// The child, stopped however the probe ends.
struct Player(std::process::Child);

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts a child playing clicks on the default device: the child, a
/// line on its stdin reopens its stream, and each click's due time.
fn start_player() -> (Player, std::process::ChildStdin, mpsc::Receiver<i64>) {
    let mut child = Player(
        Command::new(std::env::current_exe().unwrap())
            .arg("--play-default")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
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
    (child, control, due_rx)
}

/// How many clicks came out on `device_id` and how late (median).
fn report(device_id: &str, dues: &mpsc::Receiver<i64>) -> windows::core::Result<()> {
    let (onsets, peak) = click_onsets(device_id, 5.0)?;
    let dues: Vec<i64> = dues.try_iter().collect();
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
        "  {} clicks heard there, median {:.1} ms after due, at {:.0}% of the click's level",
        delays.len(),
        delays.get(delays.len() / 2).copied().unwrap_or(f64::NAN),
        peak / AMPLITUDE * 100.0
    );
    Ok(())
}

/// One run: points a playing child at `device_id`; with `reopen`, the
/// child then opens its stream again.
fn run(device_id: &str, reopen: bool) -> windows::core::Result<()> {
    let (child, mut control, dues) = start_player();
    let pid = child.0.id();
    std::thread::sleep(Duration::from_secs(1));
    app_output::set(pid, Some(device_id))?;
    println!("  Windows now says the app plays on: {:?}", app_output::get(pid)?);
    if reopen {
        writeln!(control, "reopen").unwrap();
    }
    std::thread::sleep(Duration::from_millis(500));
    let result = report(device_id, &dues);
    app_output::set(pid, None)?;
    result
}

/// Through the mixer: the child's channel plays on `device_id`, the child
/// on the default device. It's played through the mixer until it opens
/// its audio again on the device Sonora pointed it at, then direct.
fn run_mixer(device_id: &str) -> windows::core::Result<()> {
    let (child, mut control, dues) = start_player();
    let pid = child.0.id();
    std::thread::sleep(Duration::from_secs(1));
    let mixer = Mixer::start()?;
    mixer.ensure_bus("Game");
    mixer.set_bus_device("Game", Some(device_id.to_string()));
    mixer.set_bus_priority("Game", Priority::High);
    let id = mixer.add_source(pid, "probe", "Game")?;
    let direct = || mixer.sources().iter().any(|source| source.direct);
    std::thread::sleep(Duration::from_millis(1500));
    println!(
        "  routed: {}; Windows has it on {:?}",
        if direct() { "playing direct" } else { "through the mixer" },
        app_output::get(pid)?
    );
    report(device_id, &dues)?;
    writeln!(control, "reopen").unwrap();
    let reopened = Instant::now();
    while !direct() && reopened.elapsed() < Duration::from_secs(6) {
        std::thread::sleep(Duration::from_millis(50));
    }
    println!(
        "  reopened its audio: {} after {:.1} s",
        if direct() { "playing direct" } else { "still through the mixer" },
        reopened.elapsed().as_secs_f64()
    );
    let _ = dues.try_iter().count();
    report(device_id, &dues)?;
    let app_meter = mixer.sources().iter().map(|source| source.level).fold(0.0f32, f32::max);
    let channel_meter = mixer.buses().iter().find(|bus| bus.name == "Game").map_or(0.0, |bus| bus.level);
    println!(
        "  meters: app {:.0}%, channel {:.0}% of the click's level",
        app_meter / AMPLITUDE * 100.0,
        channel_meter / AMPLITUDE * 100.0
    );
    // Its fader and its channel's still set its volume.
    mixer.set_source_gain(id, 0.5);
    std::thread::sleep(Duration::from_millis(300));
    println!("  app at 50%:");
    report(device_id, &dues)?;
    mixer.set_bus_gain("Game", 0.5);
    std::thread::sleep(Duration::from_millis(300));
    println!("  and its channel at 50%:");
    report(device_id, &dues)?;
    drop(mixer);
    println!("  removed: Windows has it back on {:?}", app_output::get(pid)?);
    Ok(())
}

/// The loudest the child's own sound gets over `seconds`, recorded as
/// another app recording it would (Windows' per-app capture), relative to
/// its click.
fn heard_by_a_recording(pid: u32, seconds: f64) -> windows::core::Result<f32> {
    use windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    let client = activate_process_loopback_client(pid)?;
    let mut peak = 0.0f32;
    unsafe {
        let event = CreateEventW(None, false, false, None)?;
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            2_000_000,
            0,
            &standard_format(),
            None,
        )?;
        client.SetEventHandle(event)?;
        let service: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let start = Instant::now();
        while start.elapsed().as_secs_f64() < seconds {
            WaitForSingleObject(event, 100);
            while service.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                service.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let samples = std::slice::from_raw_parts(data as *const f32, frames as usize * 2);
                peak = samples.iter().fold(peak, |peak, s| peak.max(s.abs()));
                service.ReleaseBuffer(frames)?;
            }
        }
        client.Stop()?;
    }
    Ok(peak / AMPLITUDE)
}

/// Through the mixer with the EQ on and `device_id` as the silent output.
fn run_park(device_id: &str) -> windows::core::Result<()> {
    let (child, mut control, dues) = start_player();
    let pid = child.0.id();
    std::thread::sleep(Duration::from_secs(1));
    let mixer = Mixer::start()?;
    mixer.ensure_bus("Game");
    mixer.set_bus_eq_band("Game", 5, EqBand { gain_db: 3.0, ..DEFAULT_EQ_BANDS[5] });
    mixer.set_silent_output(Some(device_id.to_string()));
    mixer.add_source(pid, "probe", "Game")?;
    std::thread::sleep(Duration::from_millis(1500));
    println!("  routed with the EQ on; Windows has it on {:?}", app_output::get(pid)?);
    println!("  a recording of it hears {:.1}% of the click (playing as it is until it reopens its audio)", heard_by_a_recording(pid, 2.0)? * 100.0);
    writeln!(control, "reopen").unwrap();
    std::thread::sleep(Duration::from_millis(2500));
    let _ = dues.try_iter().count();
    println!("  reopened its audio:");
    report(device_id, &dues)?;
    println!("  a recording of it hears {:.1}% of the click", heard_by_a_recording(pid, 2.0)? * 100.0);
    let channel_meter = mixer.buses().iter().find(|bus| bus.name == "Game").map_or(0.0, |bus| bus.level);
    println!("  channel meter: {:.0}% of the click (through the mixer, at its own volume)", channel_meter / AMPLITUDE * 100.0);
    drop(mixer);
    println!("  removed: Windows has it back on {:?}", app_output::get(pid)?);
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
    // A read-only look at apps playing now: what Windows has them set to.
    for pid in std::env::args().skip(2).filter_map(|pid| pid.parse::<u32>().ok()) {
        println!("process {pid} plays on: {:?}", app_output::get(pid));
    }
    println!("device: {}", device.name);
    // After a crash: starting the mixer should give app `pid` back its own
    // output (`recover <pid>`).
    if let Some(pid) = args.iter().position(|arg| arg == "recover").and_then(|at| args.get(at + 1)?.parse::<u32>().ok()) {
        println!("before: Windows has {pid} on {:?}", app_output::get(pid));
        drop(Mixer::start()?);
        println!("after starting the mixer: {:?}", app_output::get(pid));
        return Ok(());
    }
    if args.iter().any(|arg| arg == "park") {
        println!("routed through the mixer, the silent output set to it:");
        return run_park(&device.id);
    }
    if args.iter().any(|arg| arg == "mixer") {
        println!("routed through the mixer to it:");
        return run_mixer(&device.id);
    }
    println!("pointed there while playing, stream left open:");
    run(&device.id, false)?;
    println!("pointed there while playing, then the stream reopened:");
    run(&device.id, true)
}
