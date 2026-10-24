//! Linux audio through the sound server's PulseAudio protocol: PulseAudio
//! itself, or PipeWire through pipewire-pulse (Ubuntu, Fedora, SteamOS...).
//!
//! Routing an app works differently from Windows: each routed app gets a
//! silent sink of its own ("Sonora: Discord") that its streams are moved
//! to, and the mixer records that sink's monitor. So the app is only heard
//! through the mixer, and nothing needs turning down. When nothing needs
//! the mixer, its streams go straight to the channel's output instead
//! (see `capture.rs`). The Mic channel's
//! voice goes to a sink whose monitor is a virtual microphone, so Discord
//! and games get it without installing anything. Everything Sonora creates
//! is named `sonora_...`, so what a crash leaves behind is found and
//! removed on the next start.

pub mod capture;
mod conn;
pub mod devices;
pub mod mic;
pub mod output;
mod procs;
pub mod sessions;
pub mod setup;

pub(crate) use conn::Connection;

/// The start of every sink and source name Sonora creates.
pub(crate) const PREFIX: &str = "sonora_";
/// The start of a routed app's sink name; the rest is
/// `<Sonora's pid>_<app pid>`.
pub(crate) const APP_SINK_PREFIX: &str = "sonora_app_";
/// The sink the Mic channel sends your voice to. Its monitor feeds the
/// virtual microphone.
pub(crate) const VOICE_SINK: &str = "sonora_voice";
/// The virtual microphone Discord and games record from.
pub(crate) const VIRTUAL_MIC: &str = "sonora_virtual_mic";
/// What the virtual microphone is called in Discord and games.
pub(crate) const VIRTUAL_MIC_NAME: &str = "Sonora Virtual Microphone";

/// Runs against the sound server this machine has, so they're ignored by
/// default: `cargo test -p audio-engine --lib pulse -- --ignored --test-threads 1`.
#[cfg(test)]
mod tests {
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    use super::output::DeviceOutput;
    use super::*;
    use crate::Mixer;

    /// Set in a child copy of the tests to make it play a tone instead.
    const TONE_ENV: &str = "SONORA_TEST_TONE";

    /// Plays a quiet 440 Hz tone on the default output for a few seconds,
    /// in a child process started by [`route_an_app_through_the_mixer`].
    #[test]
    #[ignore]
    fn tone_player() {
        if std::env::var_os(TONE_ENV).is_none() {
            return;
        }
        let mut output = DeviceOutput::open(None).expect("open the default output");
        let started = Instant::now();
        let mut phase = 0.0f32;
        let mut block = Vec::new();
        while started.elapsed() < Duration::from_secs(8) {
            let room = output.capacity_frames().min(4_800).saturating_sub(output.queued_frames().expect("queued"));
            block.clear();
            for _ in 0..room {
                let sample = (phase * std::f32::consts::TAU).sin() * 0.1;
                phase = (phase + 440.0 / 48_000.0) % 1.0;
                block.extend([sample, sample]);
            }
            output.write(&block).expect("play");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    struct Player(Child);

    impl Drop for Player {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn sink_of(connection: &mut Connection, pid: u32) -> Option<String> {
        let sinks = connection.sinks().unwrap();
        let input = connection.sink_inputs().unwrap().into_iter().find(|input| input.pid == Some(pid))?;
        sinks.into_iter().find(|sink| sink.index == input.sink).map(|sink| sink.name)
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// This test program under another name, so the tone player looks like
    /// a separate app rather than a helper of the test (see
    /// [`procs::app_root`]).
    fn tone_program() -> std::path::PathBuf {
        let exe = std::env::current_exe().unwrap();
        let other = exe.with_file_name(format!("sonora-test-tone-{}", std::process::id()));
        let _ = std::fs::remove_file(&other);
        std::fs::hard_link(&exe, &other).or_else(|_| std::fs::copy(&exe, &other).map(|_| ())).unwrap();
        other
    }

    #[test]
    #[ignore]
    fn route_an_app_through_the_mixer() {
        let program = tone_program();
        let player = Player(
            Command::new(&program)
                .args(["--exact", "pulse::tests::tone_player", "--ignored", "--nocapture"])
                .env(TONE_ENV, "1")
                .spawn()
                .unwrap(),
        );
        let pid = player.0.id();
        let mut connection = Connection::open("Sonora test").unwrap();
        wait_for("the tone to play", || sink_of(&mut connection, pid).is_some());
        let default = connection.default_sink().unwrap().unwrap();
        assert_eq!(sink_of(&mut connection, pid).as_deref(), Some(default.as_str()));

        let mixer = Mixer::start().unwrap();
        // The virtual microphone is there, offered as the voice output and
        // not as a microphone.
        assert!(connection.sources().unwrap().iter().any(|source| source.name == VIRTUAL_MIC));
        assert!(devices::list_output_devices().unwrap().iter().any(|d| d.id == VOICE_SINK && d.voice_cable_mic.is_some()));
        assert!(!devices::list_input_devices().unwrap().iter().any(|d| d.id.starts_with(PREFIX)));
        // The tone is listed as an app.
        assert!(sessions::enumerate_sessions().unwrap().iter().any(|session| session.process_id == pid));

        // With the channel's EQ on, it's played through the mixer from a
        // sink of its own.
        mixer.ensure_bus("Game");
        mixer.set_bus_eq_band("Game", 2, 3.0);
        let id = mixer.add_source(pid, "Tone", "Game").unwrap();
        let routed = format!("{APP_SINK_PREFIX}{}_{pid}", std::process::id());
        wait_for("the tone to move to its own sink", || sink_of(&mut connection, pid).as_deref() == Some(routed.as_str()));
        let game_peak = |mixer: &Mixer| {
            let mut peak = 0.0f32;
            wait_for("the tone on the Game channel's meter", || {
                peak = peak.max(mixer.buses().iter().find(|bus| bus.name == "Game").map_or(0.0, |bus| bus.level));
                peak > 0.05
            });
            peak
        };
        let peak = game_peak(&mixer);
        assert!(peak < 0.25, "the tone plays at 0.1 (the EQ lifts it a little), got {peak}");
        assert!(mixer.sources().iter().any(|source| source.id == id && source.pid == Some(pid) && !source.direct));

        // With the EQ flat, it plays straight on the channel's output (the
        // default one), still on the meter.
        mixer.set_bus_eq_band("Game", 2, 0.0);
        wait_for("the tone to play direct", || mixer.sources().iter().any(|source| source.id == id && source.direct));
        assert_eq!(sink_of(&mut connection, pid).as_deref(), Some(default.as_str()));
        let peak = game_peak(&mixer);
        assert!(peak < 0.2, "the tone plays at 0.1, got {peak}");

        // Removing it gives the app back its output and removes its sink
        // before returning, so quitting right after can't leave it silent.
        mixer.remove_source(id);
        assert_eq!(sink_of(&mut connection, pid).as_deref(), Some(default.as_str()));
        assert!(!connection.sinks().unwrap().iter().any(|sink| sink.name == routed));

        // Stopping the mixer takes the virtual microphone down.
        drop(mixer);
        wait_for("the virtual mic to go", || !connection.sources().unwrap().iter().any(|source| source.name == VIRTUAL_MIC));
        drop(player);
        let _ = std::fs::remove_file(program);
    }

    /// Set in a child copy of the tests to make it play clicks on the sink
    /// it names.
    const CLICK_ENV: &str = "SONORA_TEST_CLICKS";
    /// A scratch output to time clicks on, removed afterwards.
    const TIMING_SINK: &str = "latency_test_out";
    const CLICK_LEVEL: f32 = 0.05;

    /// Wall-clock microseconds, the same in both processes.
    fn now_us() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as i64
    }

    /// Plays a short click every half second on the sink [`CLICK_ENV`]
    /// names, printing when each is due to play (written, plus what's
    /// queued ahead of it), in a child process started by [`routing_delay`].
    #[test]
    #[ignore]
    fn click_player() {
        use libpulse_binding::def::BufferAttr;
        use libpulse_binding::sample::{Format, Spec};
        use libpulse_binding::stream::{FlagSet, SeekMode, Stream};
        let Ok(sink) = std::env::var(CLICK_ENV) else { return };
        let mut connection = Connection::open("Sonora test clicks").unwrap();
        let spec = Spec { format: Format::F32le, channels: 2, rate: 48_000 };
        let mut stream = Stream::new(&mut connection.context, "Clicks", &spec, None).unwrap();
        // 20 ms queued, like a game.
        const QUEUE: u64 = 960;
        let attr = BufferAttr { maxlength: u32::MAX, tlength: QUEUE as u32 * 8, prebuf: 0, minreq: u32::MAX, fragsize: u32::MAX };
        stream.connect_playback(Some(&sink), Some(&attr), FlagSet::NOFLAGS, None, None).unwrap();
        connection.wait_ready(&stream).unwrap();
        let (mut written, mut next_click) = (0u64, 14_400u64);
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(12) {
            connection.iterate_for(Duration::from_millis(2)).unwrap();
            let room = stream.writable_size().unwrap_or(0) as u64 / 8;
            if room == 0 {
                continue;
            }
            let queued = QUEUE.saturating_sub(room);
            let now = now_us();
            let mut bytes = Vec::with_capacity(room as usize * 8);
            for i in 0..room {
                let position = written + i;
                if position == next_click {
                    println!("click {}", now + ((queued + i) * 1_000_000 / 48_000) as i64);
                }
                let value = if position >= next_click && position < next_click + 240 { CLICK_LEVEL } else { 0.0 };
                if position + 1 == next_click + 240 {
                    next_click += 24_000;
                }
                bytes.extend(value.to_le_bytes());
                bytes.extend(value.to_le_bytes());
            }
            stream.write(&bytes, None, 0, SeekMode::Relative).unwrap();
            written += room;
        }
    }

    /// When each click comes out on [`TIMING_SINK`] over `seconds`, from
    /// when its recording was read, and the loudest sample.
    fn click_arrivals(connection: &mut Connection, seconds: f64) -> (Vec<i64>, f32) {
        let mut stream = connection.record(Some(&format!("{TIMING_SINK}.monitor")), 2, "Sonora test timing").unwrap();
        let (mut samples, mut onsets, mut peak, mut quiet) = (Vec::new(), Vec::new(), 0.0f32, usize::MAX);
        let started = Instant::now();
        while started.elapsed().as_secs_f64() < seconds {
            connection.iterate_for(Duration::from_millis(2)).unwrap();
            samples.clear();
            conn::read_recorded(&mut stream, &mut samples).unwrap();
            let read_at = now_us();
            let frames = samples.len() / 2;
            for (i, frame) in samples.chunks_exact(2).enumerate() {
                peak = peak.max(frame[0].abs());
                if frame[0].abs() > CLICK_LEVEL / 8.0 {
                    if quiet > 4_800 {
                        onsets.push(read_at - (frames - i) as i64 * 1_000_000 / 48_000);
                    }
                    quiet = 0;
                } else {
                    quiet = quiet.saturating_add(1);
                }
            }
        }
        let _ = stream.disconnect();
        (onsets, peak)
    }

    /// One timed run: clicks played on [`TIMING_SINK`] by a separate app,
    /// not routed (`eq: None`), or routed to the Game channel playing
    /// there, with its EQ on (`Some(true)`) or flat. Prints how late they
    /// came out (median), how loud, and how the app was routed.
    fn timed_run(program: &std::path::Path, eq: Option<bool>, gain: f32) {
        use std::io::{BufRead, BufReader};
        let connection = &mut Connection::open("Sonora test timing").unwrap();
        let mut player = Player(
            Command::new(program)
                .args(["--exact", "pulse::tests::click_player", "--ignored", "--nocapture"])
                .env(CLICK_ENV, TIMING_SINK)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let pid = player.0.id();
        let stdout = player.0.stdout.take().unwrap();
        let (due_tx, due_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(due) = line.strip_prefix("click ").and_then(|due| due.parse::<i64>().ok()) {
                    let _ = due_tx.send(due);
                }
            }
        });
        wait_for("the clicks to play", || sink_of(connection, pid).is_some());
        let mixer = eq.map(|eq| {
            let mixer = Mixer::start().unwrap();
            mixer.ensure_bus("Game");
            mixer.set_bus_device("Game", Some(TIMING_SINK.into()));
            mixer.set_bus_priority("Game", common::Priority::High);
            if eq {
                mixer.set_bus_eq_band("Game", 2, 3.0);
            }
            let id = mixer.add_source(pid, "Clicks", "Game").unwrap();
            mixer.set_source_gain(id, gain);
            std::thread::sleep(Duration::from_millis(1500));
            mixer
        });
        let (onsets, peak) = click_arrivals(connection, 5.0);
        let dues: Vec<i64> = due_rx.try_iter().collect();
        // Each click against the due time nearest it: one not routed can
        // come out a hair before its estimate.
        let mut delays: Vec<f64> = onsets
            .iter()
            .filter_map(|&onset| {
                let due = dues.iter().copied().min_by_key(|&due| (onset - due).abs())?;
                let delay = (onset - due) as f64 / 1000.0;
                (delay.abs() < 250.0).then_some(delay)
            })
            .collect();
        delays.sort_by(f64::total_cmp);
        let route = match &mixer {
            None => "not routed".to_string(),
            Some(mixer) => {
                // The meter's peak over the run, as a share of the click.
                let sources = mixer.sources();
                let direct = sources.iter().any(|source| source.direct);
                let meter = sources.iter().map(|source| source.level).fold(0.0f32, f32::max);
                format!(
                    "routed, EQ {}, app at {:.0}%: {} (meter {:.0}%)",
                    if eq == Some(true) { "on" } else { "flat" },
                    gain * 100.0,
                    if direct { "direct" } else { "through the mixer" },
                    meter / CLICK_LEVEL * 100.0
                )
            }
        };
        println!(
            "{route}: {} clicks, median {:.1} ms after due, at {:.0}% of the click's level",
            delays.len(),
            delays.get(delays.len() / 2).copied().unwrap_or(f64::NAN),
            peak / CLICK_LEVEL * 100.0
        );
        drop(mixer);
    }

    fn set_default_sink(connection: &mut Connection, name: &str) {
        let done = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = done.clone();
        let _ = connection.context.set_default_sink(name, move |_| flag.set(true));
        wait_for("the default output to change", || {
            let _ = connection.iterate_for(Duration::from_millis(20));
            done.get()
        });
    }

    /// Times routed audio against not routed, on a scratch output:
    /// `cargo test -p audio-engine --lib routing_delay -- --ignored --nocapture`.
    /// SONORA_TEST_ROUTE picks one run: `none`, `eq`, `flat` or `flat50`
    /// (the app at 50%); all of them otherwise.
    #[test]
    #[ignore]
    fn routing_delay() {
        let mut connection = Connection::open("Sonora test").unwrap();
        let module = connection.load_module("module-null-sink", &format!("sink_name={TIMING_SINK}")).unwrap();
        // The default output while timing, so an app given back its own
        // lands there. In WSL the default is WSLg's RDP sink, and waking it
        // once idle stalls the server for seconds (or for good).
        let previous_default = connection.default_sink().unwrap().unwrap_or_default();
        set_default_sink(&mut connection, TIMING_SINK);
        let program = tone_program();
        let runs = [("none", None, 1.0), ("eq", Some(true), 1.0), ("flat", Some(false), 1.0), ("flat50", Some(false), 0.5)];
        let only = std::env::var("SONORA_TEST_ROUTE").ok();
        for (name, eq, gain) in runs {
            if only.as_deref().is_none_or(|only| only == name) {
                timed_run(&program, eq, gain);
            }
        }
        set_default_sink(&mut connection, &previous_default);
        let _ = connection.unload_module(module);
        let _ = std::fs::remove_file(program);
    }

    #[test]
    #[ignore]
    fn a_helper_playing_for_an_app_is_listed_as_the_app() {
        // The tone player is a child running the test's own program: a
        // helper of the test process, which is Sonora's own here, so it
        // isn't offered as an app at all.
        let player = Player(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "pulse::tests::tone_player", "--ignored", "--nocapture"])
                .env(TONE_ENV, "1")
                .spawn()
                .unwrap(),
        );
        let pid = player.0.id();
        let mut connection = Connection::open("Sonora test").unwrap();
        wait_for("the tone to play", || sink_of(&mut connection, pid).is_some());
        assert_eq!(procs::app_root(pid), std::process::id());
        assert!(!sessions::enumerate_sessions().unwrap().iter().any(|s| s.process_id == pid || s.process_id == std::process::id()));
    }
}
