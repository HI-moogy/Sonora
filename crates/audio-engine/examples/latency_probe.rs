//! Measures how late audio comes out through the mixer, and whether it
//! drops out, on a spare output device (so nothing plays where you listen).
//!
//! A child process plays a short click every half second to the device and
//! reports, for each click, the moment it was due to play (its write time
//! plus what was already queued ahead of it). This process captures the
//! device's output and times when each click actually comes out. Done once
//! with the child playing directly and once routed through a mixer bus; the
//! difference is the delay the mixer adds. In `tone` mode the child plays a
//! steady tone instead, and gaps in it are counted as dropouts.
//!
//! In `capture` mode it times only Windows' per-app capture (no mixer).
//! With `short`, the device's audio engine is held at its shortest cycle
//! while measuring (as a low-latency app would). `periods` lists each
//! output device's engine cycles.
//!
//! Usage: latency_probe <part of a device name> [priority: high|normal|low] [clicks|tone|capture] [short]
//!        latency_probe periods

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use audio_engine::loopback::standard_format;
use audio_engine::render::open_render_client_for_device;
use audio_engine::{list_output_devices, ComGuard, Mixer};
use common::Priority;
use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, IAudioClient3, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

const RATE: f64 = 48_000.0;
const CLICK_EVERY: Duration = Duration::from_millis(500);
const CLICK_FRAMES: usize = 240;
const AMPLITUDE: f32 = 0.02;

/// The playing child process, stopped however the probe ends.
struct Player(std::process::Child);

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Now, in 100 ns units of the system-wide performance counter (the time
/// base WASAPI capture timestamps use).
fn now_hns() -> i64 {
    let (mut counter, mut frequency) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut counter);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    (counter as i128 * 10_000_000 / frequency as i128) as i64
}

/// Child: plays clicks (or a tone) to `device_id`, printing each click's due time.
fn play(device_id: &str, tone: bool) -> windows::core::Result<()> {
    let (client, render) = open_render_client_for_device(Some(device_id), &standard_format())?;
    let size = unsafe { client.GetBufferSize()? };
    unsafe { client.Start()? };
    let (mut written, mut next_click, mut phase) = (0u64, 0u64, 0f64);
    loop {
        std::thread::sleep(Duration::from_millis(5));
        let padding = unsafe { client.GetCurrentPadding()? };
        let free = size - padding;
        if free == 0 {
            continue;
        }
        let samples = unsafe {
            std::slice::from_raw_parts_mut(render.GetBuffer(free)? as *mut f32, free as usize * 2)
        };
        let now = now_hns();
        for (i, frame) in samples.chunks_mut(2).enumerate() {
            let position = written + i as u64;
            let value = if tone {
                phase += std::f64::consts::TAU * 440.0 / RATE;
                (phase.sin() * AMPLITUDE as f64) as f32
            } else if position >= next_click && position < next_click + CLICK_FRAMES as u64 {
                if position == next_click {
                    // Plays after what's queued ahead of it.
                    let due = now + ((padding as u64 + i as u64) as f64 / RATE * 1e7) as i64;
                    println!("click {due}");
                }
                AMPLITUDE
            } else {
                0.0
            };
            frame.fill(value);
            if !tone && position + 1 == next_click + CLICK_FRAMES as u64 {
                next_click += (CLICK_EVERY.as_secs_f64() * RATE) as u64;
            }
        }
        if !tone && next_click == 0 {
            next_click = (RATE * 0.3) as u64;
        }
        written += free as u64;
        unsafe { render.ReleaseBuffer(free, 0)? };
    }
}

/// Captures what `device_id` plays for `seconds`, as (timestamp, left sample) packets.
fn capture(device_id: &str, seconds: f64) -> windows::core::Result<Vec<(i64, Vec<f32>)>> {
    let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
    let client: IAudioClient = unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        enumerator
            .GetDevice(PCWSTR(wide.as_ptr()))?
            .Activate(CLSCTX_ALL, None)?
    };
    let mut packets = Vec::new();
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
                let (mut data, mut frames, mut flags, mut qpc) =
                    (std::ptr::null_mut(), 0u32, 0u32, 0u64);
                service.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
                let samples = std::slice::from_raw_parts(data as *const f32, frames as usize * 2);
                packets.push((qpc as i64, samples.iter().step_by(2).copied().collect()));
                service.ReleaseBuffer(frames)?;
            }
        }
    }
    Ok(packets)
}

/// When each click starts in the captured output.
fn click_onsets(packets: &[(i64, Vec<f32>)]) -> Vec<i64> {
    let mut onsets = Vec::new();
    let mut quiet_for = usize::MAX;
    for (qpc, samples) in packets {
        for (i, &s) in samples.iter().enumerate() {
            if s.abs() > AMPLITUDE / 4.0 {
                if quiet_for > 4_800 {
                    onsets.push(qpc + (i as f64 / RATE * 1e7) as i64);
                }
                quiet_for = 0;
            } else {
                quiet_for = quiet_for.saturating_add(1);
            }
        }
    }
    onsets
}

/// Gaps of at least 1 ms inside a steady tone (it never stays that close to zero).
fn dropouts(packets: &[(i64, Vec<f32>)]) -> usize {
    let samples: Vec<f32> = packets
        .iter()
        .flat_map(|(_, s)| s.iter().copied())
        .collect();
    let Some(first) = samples.iter().position(|s| s.abs() > AMPLITUDE / 4.0) else {
        return 0;
    };
    let last = samples
        .iter()
        .rposition(|s| s.abs() > AMPLITUDE / 4.0)
        .unwrap_or(first);
    let (mut gaps, mut run) = (0, 0);
    for s in &samples[first..=last] {
        if s.abs() < AMPLITUDE / 50.0 {
            run += 1;
            if run == 48 {
                gaps += 1;
            }
        } else {
            run = 0;
        }
    }
    gaps
}

fn run(device_id: &str, routed: Option<Priority>, tone: bool) -> windows::core::Result<()> {
    let mut child = Player(
        Command::new(std::env::current_exe().unwrap())
            .args(["--play", device_id, if tone { "tone" } else { "clicks" }])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (due_tx, due_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(due) = line
                .strip_prefix("click ")
                .and_then(|d| d.parse::<i64>().ok())
            {
                let _ = due_tx.send(due);
            }
        }
    });
    std::thread::sleep(Duration::from_millis(300));
    let mixer = match routed {
        Some(priority) => {
            let mixer = Mixer::start()?;
            mixer.ensure_bus("Game");
            mixer.set_bus_device("Game", Some(device_id.to_string()));
            mixer.set_bus_priority("Game", priority);
            mixer.add_source(child.0.id(), "probe", "Game")?;
            std::thread::sleep(Duration::from_millis(1500));
            Some(mixer)
        }
        None => None,
    };
    let packets = capture(device_id, 5.0)?;
    let mut label = routed.map_or("direct".to_string(), |p| format!("routed, {p:?} priority"));
    if let Some(mixer) = &mixer {
        let delay = mixer
            .buses()
            .into_iter()
            .find(|b| b.name == "Game")
            .map_or(0.0, |b| b.delay_ms);
        label += &format!(" (mixer's own estimate {delay:.0} ms)");
    }
    if tone {
        println!("{label}: {} dropouts in 5 s", dropouts(&packets));
    } else {
        let dues: Vec<i64> = due_rx.try_iter().collect();
        let mut delays: Vec<f64> = click_onsets(&packets)
            .iter()
            .filter_map(|&onset| {
                let due = dues.iter().copied().filter(|&d| d <= onset).max()?;
                let delay = (onset - due) as f64 / 10_000.0;
                (delay < 1_000.0).then_some(delay)
            })
            .collect();
        delays.sort_by(f64::total_cmp);
        let median = delays.get(delays.len() / 2).copied().unwrap_or(f64::NAN);
        println!(
            "{label}: {} clicks, median {median:.1} ms after due",
            delays.len()
        );
    }
    // Restore the child's volume before stopping it.
    drop(mixer);
    drop(child);
    Ok(())
}

/// Times the per-app capture alone: when each click shows up in a
/// process-loopback capture of the child, and how long after that the
/// packet holding it is handed over.
fn capture_stage(device_id: &str) -> windows::core::Result<()> {
    use audio_engine::loopback::activate_process_loopback_client;
    let mut child = Player(
        Command::new(std::env::current_exe().unwrap())
            .args(["--play", device_id, "clicks"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (due_tx, due_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(due) = line
                .strip_prefix("click ")
                .and_then(|d| d.parse::<i64>().ok())
            {
                let _ = due_tx.send(due);
            }
        }
    });
    std::thread::sleep(Duration::from_millis(300));
    let client = activate_process_loopback_client(child.0.id())?;
    let (mut onsets, mut arrivals) = (Vec::new(), Vec::new());
    // The capture buffer asked for, in ms (LATENCY_PROBE_CAPTURE_MS; 200 by default, as the mixer uses).
    let buffer_ms: i64 = std::env::var("LATENCY_PROBE_CAPTURE_MS").ok().and_then(|ms| ms.parse().ok()).unwrap_or(200);
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            buffer_ms * 10_000,
            0,
            &standard_format(),
            None,
        )?;
        println!("capture buffer asked: {buffer_ms} ms, got {} frames", client.GetBufferSize()?);
        let service: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let start = Instant::now();
        let mut quiet_for = usize::MAX;
        while start.elapsed().as_secs_f64() < 5.0 {
            std::thread::sleep(Duration::from_millis(1));
            while service.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags, mut qpc) =
                    (std::ptr::null_mut(), 0u32, 0u32, 0u64);
                service.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
                let got = now_hns();
                let samples = std::slice::from_raw_parts(data as *const f32, frames as usize * 2);
                for (i, s) in samples.iter().step_by(2).enumerate() {
                    if s.abs() > AMPLITUDE / 4.0 {
                        if quiet_for > 4_800 {
                            let onset = qpc as i64 + (i as f64 / RATE * 1e7) as i64;
                            onsets.push(onset);
                            arrivals.push((got - onset) as f64 / 10_000.0);
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
    let dues: Vec<i64> = due_rx.try_iter().collect();
    let mut stamped: Vec<f64> = onsets
        .iter()
        .filter_map(|&o| {
            dues.iter()
                .copied()
                .filter(|&d| d <= o + 2_000_000)
                .min_by_key(|d| (o - d).abs())
                .map(|d| (o - d) as f64 / 10_000.0)
        })
        .collect();
    stamped.sort_by(f64::total_cmp);
    arrivals.sort_by(f64::total_cmp);
    println!(
        "per-app capture: click stamped {:.1} ms after due (median), handed over {:.1} ms after its stamp",
        stamped.get(stamped.len() / 2).copied().unwrap_or(f64::NAN),
        arrivals.get(arrivals.len() / 2).copied().unwrap_or(f64::NAN),
    );
    Ok(())
}

fn audio_client3(device_id: &str) -> windows::core::Result<IAudioClient3> {
    let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        enumerator.GetDevice(PCWSTR(wide.as_ptr()))?.Activate(CLSCTX_ALL, None)
    }
}

/// A device's engine cycles at its own format, in frames: (default,
/// fundamental, min, max, current).
fn engine_periods(device_id: &str) -> windows::core::Result<(u32, u32, u32, u32, u32, u32)> {
    let client = audio_client3(device_id)?;
    unsafe {
        let format = client.GetMixFormat()?;
        let (mut default, mut fundamental, mut min, mut max) = (0, 0, 0, 0);
        let result = client.GetSharedModeEnginePeriod(format, &mut default, &mut fundamental, &mut min, &mut max);
        let rate = (*format).nSamplesPerSec;
        windows::Win32::System::Com::CoTaskMemFree(Some(format as *const _));
        result?;
        let (mut current_format, mut current) = (std::ptr::null_mut(), 0);
        if client.GetCurrentSharedModeEnginePeriod(&mut current_format, &mut current).is_ok() {
            windows::Win32::System::Com::CoTaskMemFree(Some(current_format as *const _));
        }
        Ok((default, fundamental, min, max, current, rate))
    }
}

/// Holds `device_id`'s engine at its shortest cycle while alive: a started
/// stream that asked for it (silent; nothing is written).
fn hold_short_period(device_id: &str) -> windows::core::Result<IAudioClient3> {
    let client = audio_client3(device_id)?;
    unsafe {
        let format = client.GetMixFormat()?;
        let (mut default, mut fundamental, mut min, mut max) = (0, 0, 0, 0);
        client.GetSharedModeEnginePeriod(format, &mut default, &mut fundamental, &mut min, &mut max)?;
        let result = client.InitializeSharedAudioStream(0, min, format, None);
        let rate = (*format).nSamplesPerSec as f64;
        windows::Win32::System::Com::CoTaskMemFree(Some(format as *const _));
        result?;
        client.Start()?;
        println!(
            "holding the engine at {min} frames ({:.2} ms; default {:.2} ms)",
            min as f64 / rate * 1000.0,
            default as f64 / rate * 1000.0
        );
    }
    Ok(client)
}

fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--play") {
        return play(&args[1], args[2] == "tone");
    }
    if args.first().map(String::as_str) == Some("periods") {
        for device in list_output_devices()? {
            match engine_periods(&device.id) {
                Ok((default, fundamental, min, max, current, rate)) => {
                    let ms = |frames: u32| frames as f64 / rate as f64 * 1000.0;
                    println!(
                        "{}: default {:.2} ms, min {:.2} ms, max {:.2} ms, step {:.2} ms, now {:.2} ms",
                        device.name, ms(default), ms(min), ms(max), ms(fundamental), ms(current)
                    );
                }
                Err(error) => println!("{}: {error}", device.name),
            }
        }
        return Ok(());
    }
    let device = list_output_devices()?
        .into_iter()
        .find(|d| {
            d.name
                .contains(args.first().expect("part of a device name").as_str())
        })
        .expect("no active output device with that name");
    let priority = match args.get(1).map(String::as_str) {
        Some("normal") => Priority::Normal,
        Some("low") => Priority::Low,
        _ => Priority::High,
    };
    let _short = if args.iter().any(|arg| arg == "short") { Some(hold_short_period(&device.id)?) } else { None };
    if args.get(2).map(String::as_str) == Some("capture") {
        return capture_stage(&device.id);
    }
    let tone = args.get(2).map(String::as_str) == Some("tone");
    println!("device: {}", device.name);
    run(&device.id, None, tone)?;
    run(&device.id, Some(priority), tone)
}
