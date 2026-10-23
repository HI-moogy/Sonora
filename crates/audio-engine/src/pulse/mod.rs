//! Linux audio through the sound server's PulseAudio protocol: PulseAudio
//! itself, or PipeWire through pipewire-pulse (Ubuntu, Fedora, SteamOS...).
//!
//! Routing an app works differently from Windows: each routed app gets a
//! silent sink of its own ("Sonora: Discord") that its streams are moved
//! to, and the mixer records that sink's monitor. So the app is only heard
//! through the mixer, and nothing needs turning down. The Mic channel's
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

        let id = mixer.add_source(pid, "Tone", "Game").unwrap();
        let routed = format!("{APP_SINK_PREFIX}{}_{pid}", std::process::id());
        wait_for("the tone to move to its own sink", || sink_of(&mut connection, pid).as_deref() == Some(routed.as_str()));
        // It's heard through the Game channel.
        let mut peak = 0.0f32;
        wait_for("the tone on the Game channel's meter", || {
            peak = peak.max(mixer.buses().iter().find(|bus| bus.name == "Game").map_or(0.0, |bus| bus.level));
            peak > 0.05
        });
        assert!(peak < 0.2, "the tone plays at 0.1, got {peak}");
        assert!(mixer.sources().iter().any(|source| source.id == id && source.pid == Some(pid)));

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
