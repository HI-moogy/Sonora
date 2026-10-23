# Development

## Layout

| Crate | What it does |
| --- | --- |
| `crates/common` | Settings (`config.json`), saved in independent sections, and shared types. |
| `crates/audio-engine` | The mixer: per-app capture, channels (buses) with gain, EQ and priority, output devices, meters, and the mic's noise removal. |
| `crates/capture-engine` | Clips (Windows only): screen capture, H.264 encoding, the rolling buffer and MP4 writing. Empty on other systems. |
| `crates/app` | The `sonora` app (egui): mixer and clips screens, tray, colors, guide. |
| `driver/` | Notes on a possible virtual audio driver (not part of the app). |

## The audio engine

`mixer.rs` is shared: every app or microphone is a *source* whose captured
audio queues up as 48 kHz stereo f32; one render thread mixes each channel
(source gain, EQ, meters, the clip recorder's tap), and keeps each output
device filled only as far as its most urgent channel's priority allows.

Each system provides the rest, chosen in `lib.rs`'s `platform` module:

| | Windows (`*.rs` at the top) | Linux (`pulse/`) |
| --- | --- | --- |
| Capturing an app | Process loopback of the app and its helper processes; the app's own sessions are turned down (`session_volume.rs`) so only the mixer's copy is heard. | The app's streams move to a silent sink of its own, whose monitor is recorded. Nothing is turned down. |
| Capturing the mic | WASAPI capture (`mic.rs`). | A record stream (`pulse/mic.rs`). |
| Playing | WASAPI shared-mode client per device (`output.rs`). | A playback stream per device (`pulse/output.rs`). |
| Voice to Discord | A virtual cable you install (VB-Cable). | Sonora's own virtual microphone, set up while it runs (`pulse/setup.rs`). |
| After a crash | Apps left turned down are turned back up on the next start. | Sinks left behind are removed on the next start. |

Linux talks the PulseAudio protocol through libpulse, which PipeWire also
serves (`pipewire-pulse`). Each thread that uses the sound server owns its
own connection, since libpulse's objects stay on one thread.

## App

`main.rs` holds the mixer screen; `moments.rs` and `clips/` the clips
screen (Windows only). Platform-specific app pieces have a Linux twin
selected by `#[cfg_attr(target_os = "linux", path = ...)]`: `tray.rs` /
`tray_linux.rs`, `autostart.rs` / `autostart_linux.rs`,
`single_instance.rs` / `single_instance_linux.rs`. Colors come from
`theme.rs`, which every screen reads; nothing else hard-codes a color.

## Checks

```sh
cargo build --release -p app
cargo test -p common -p app -p audio-engine --lib
cargo clippy -p app -p audio-engine
```

On Linux, the sound-server tests are opt-in (they use the real server):
`cargo test -p audio-engine --lib pulse -- --ignored --test-threads 1`.
The engine's `examples/` are Windows tools (latency and routing probes).

## Releasing

1. Update `version` in `crates/app/Cargo.toml` and add the release to
   `CHANGELOG.md`.
2. Commit, tag `v<version>` (for example `v0.1.0-alpha.2`), and push the
   tag. The release workflow builds both packages and opens a draft release
   with them; check it and publish it.

Linux packages are built on Ubuntu 22.04 so they run on any system with
glibc 2.35 or newer (Ubuntu 22.04+, Mint 21+, Pop!_OS 22.04+, Fedora 36+,
SteamOS 3.5+). A build made on a newer system needs that system's glibc.
