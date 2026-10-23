# Changelog

## 0.1.0-alpha.2 (2026-10-23)

### New

- **Linux.** The mixer, mic cleanup, EQ, colors and tour on Linux, with
  PipeWire or PulseAudio. Apps are routed without turning anything down,
  and a built-in **Sonora Virtual Microphone** carries your voice to
  Discord and games (no VB-Cable needed). Installs without root, so it
  works on the Steam Deck. Clips are Windows-only for now.
- **Noise removal for your mic**: Off, Light (RNNoise), Standard and Strong
  (DeepFilterNet 3), an option to turn down voices far quieter than yours,
  and a record-then-play test to hear the difference.
- **Send your voice to Discord and games** through a virtual cable, with
  its EQ and noise removal.
- **Lower delay**: routed audio plays about 60 to 90 ms late instead of
  about 250 ms, with a per-channel priority (High for games and voice,
  Normal or Low for music and video).
- **More EQ presets** for games, chat, mic and video, listed per channel.
- **Colors**: seven presets, any color changeable, and colors to share as
  text (Menu > Colors).
- **A getting-started tour** the first time Sonora opens (Menu > Getting
  started guide).
- **Clips**: clips in subfolders show too; sort by date or game and filter
  by game; the game is read from Radeon and SteelSeries file names. The
  editor has previous/next clip buttons, skip to start/end, and drawn
  controls.
- **Bug reports**: Menu > About and bug reports copies your version and
  system, and Sonora writes a crash report if it crashes.

### Changed

- **Renamed to Sonora**, with a new look and icon. Settings move from
  `sonar-rs` on their own.

### Fixed

- You no longer hear your own mic unless you turn on **Listen to my mic**.
- Channel faders were upside down.
- Clip settings no longer hide behind the clip list; editor sliders fit.

## 0.1.0-alpha.1 (2026-10-22)

First build for friends to try (as sonar-rs): Game, Chat, Aux, Media and
Mic channels with per-app routing, EQ with genre presets, output device per
channel with fallback when it disconnects; always-on clips of the screen
or only games, saved with a hotkey, with per-channel audio tracks and an
editor to rebalance them; tray icon, start with Windows, and the mixer
restored between runs.
