# Changelog

## Unreleased

### New

- **Record the whole screen only while a game is open**: a new choice in the ⚙ clip settings. It records the monitor the game is on, and nothing while no game is open.
- **Your games list**: the clip settings list every game Sonora found (Steam and Epic games by name, other launchers' games, and the ones Windows knows) with a search box. Remove a game to leave it out, put it back, or add any program with **Browse…** or from the apps that are open.

### Changed

- **Whole screen, all the time** now warns that it keeps your graphics card's video encoder busy nonstop: Task Manager can show Sonora using half the GPU, even on an idle desktop.
- Until you choose what clips record, they record the whole screen only while a game is open (it was the whole screen, all the time).
- Wallpaper Engine, Lossless Scaling, Proton and other Steam apps that aren't games no longer start a recording.

### Fixed

- Recording a monitor turned on its side (portrait) gave black video; it's recorded upright now.

## 0.1.0-alpha.4 (2026-10-23)

### New

- **Add a program by name**: under a channel's **Add an app**, **Can't find your program? Add it manually** takes a program's name (like `deadlock.exe`) or the program picked with **Browse…**. It joins the channel whenever it plays, and is saved and removed like any app you added.

### Changed

- **Copy details** also says how your voice gets to Discord: which mic is recorded, where the Mic channel sends it, **Listen to my mic**, noise removal and EQ, and on Windows the mic's format and whether your default output is the virtual cable. That's what's needed to tell why a voice sounds wrong.
- The Mic panel suggests turning off Discord's own noise suppression while Sonora's is on, since two in a row make a voice sound underwater.

### Fixed

- **A doubled, underwater voice in Discord.** If your saved mic couldn't open when Sonora started (a headset not connected yet) and you picked a mic again, both were recorded and sent to the cable a few milliseconds apart, and it came back on every start. Sonora now records one mic, and switching mics never plays both.
- **Listen to my mic** no longer sends a second copy of your voice when Windows' default output is the virtual cable.
- Mics and virtual cables that don't run at 48 kHz sound cleaner: Windows now converts them with its better converter.
- A game started from Steam was missing from **Add an app** while Steam itself had a sound going (voice chat, a notification): the game was counted as part of Steam. Programs with a window of their own are listed on their own now.
- An app Sonora isn't allowed to look into (some run as administrator, or protected by anti-cheat) was missing from **Add an app**, and wasn't routed again when it opened.

## 0.1.0-alpha.3 (2026-10-23)

### New

- **Trim clips**: split a clip at the playhead (scissors, or S), right-click a part to cut it out (or press Delete), and save. Playback skips cut parts. The video is copied, not re-encoded, so no quality is lost; a kept part starts at the keyframe just before your split, which the timeline shows in amber.

### Changed

- **Reporting a bug**: Menu > About and bug reports asks you to send what happened to whoever gave you Sonora (with Copy details and the newest crash report). Its Report on GitHub button is gone while the repository is private, since testers couldn't open it.

## 0.1.0-alpha.2 (2026-10-23)

### New

- **Linux.** The mixer, mic cleanup, EQ, colors and tour on Linux, with PipeWire or PulseAudio. Apps are routed without turning anything down, and a built-in **Sonora Virtual Microphone** carries your voice to Discord and games (no VB-Cable needed). Installs without root, so it works on the Steam Deck. Clips are Windows-only for now.
- **Noise removal for your mic**: Off, Light (RNNoise), Standard and Strong (DeepFilterNet 3), an option to turn down voices far quieter than yours, and a record-then-play test to hear the difference.
- **Send your voice to Discord and games** through a virtual cable, with its EQ and noise removal.
- **Lower delay**: routed audio plays about 60 to 90 ms late instead of about 250 ms, with a per-channel priority (High for games and voice, Normal or Low for music and video).
- **More EQ presets** for games, chat, mic and video, listed per channel.
- **Colors**: seven presets, any color changeable, and colors to share as text (Menu > Colors).
- **A getting-started tour** the first time Sonora opens (Menu > Getting started guide).
- **Clips**: clips in subfolders show too; sort by date or game and filter by game; the game is read from Radeon and SteelSeries file names. The editor has previous/next clip buttons, skip to start/end, and drawn controls.
- **Bug reports**: Menu > About and bug reports copies your version and system, and Sonora writes a crash report if it crashes.
- **Windows download is one file**: `Sonora.exe`, nothing to unzip or install. Replace it to update.

### Changed

- **Renamed to Sonora**, with a new look and icon. Settings move from `sonar-rs` on their own.

### Fixed

- You no longer hear your own mic unless you turn on **Listen to my mic**.
- Channel faders were upside down.
- Clip settings no longer hide behind the clip list; editor sliders fit.
- One unreadable setting no longer resets all your settings.
- An app removed from a channel during a device change could stay almost silent with nothing to turn it back up.
- A damaged clip file no longer stops every clip's thumbnail, or leaves the editor stuck on Loading.
- Sonora says why if its audio can't start, instead of closing.

## 0.1.0-alpha.1 (2026-10-22)

First build for friends to try (as sonar-rs): Game, Chat, Aux, Media and Mic channels with per-app routing, EQ with genre presets, output device per channel with fallback when it disconnects; always-on clips of the screen or only games, saved with a hotkey, with per-channel audio tracks and an editor to rebalance them; tray icon, start with Windows, and the mixer restored between runs.
