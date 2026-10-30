# Changelog

## 0.1.0-alpha.9 (2026-10-30)

Also brings everything in 0.1.0-alpha.8, which wasn't published on its own: the 10-band EQ like SteelSeries Sonar's, Share to Discord, and saving clips with a key on its own (see [the changelog](https://github.com/HI-moogy/Sonora/blob/main/CHANGELOG.md)).

### New

- **Change a clip's game**: right-click a clip and pick **Change game…** to put it under the right game, like an "unknown" Radeon replay that's really Team Fortress 2. Pick from your games as you type, or type any name. The file stays where it is, and **Use what Sonora found** puts it back.

### Fixed

- **Radeon clips wouldn't open** ("video uses frame reordering"), nor other recorders' clips made the same way: they now play, edit and save, with their sound where their recorder put it.
- **Discord streams of a game had no sound** when the game's channel had its EQ on: Sonora turns a game's own sound down while it plays its copy through the EQ, and a stream of the game's window records the game's own sound. Pick an output you never listen to in **Menu > Discord streams…** and apps play their own sound there at full volume instead, so streams hear them while you hear Sonora's copy. A game that's already playing keeps its sound (without the EQ) until it opens its sound again.

## 0.1.0-alpha.8 (2026-10-30)

### New

- **A 10-band EQ like SteelSeries Sonar's** (#2): each channel's EQ is a curve with ten dots. Drag a dot to move its band (left and right for where, up and down for how much), scroll on it to make it wider or narrower, double-click it to flatten it, and right-click it to make it a shelf or a cut, turn it off, or reset it. The picked band's values can be typed in, like 440 or 2.6k. **Bigger** opens the EQ in a big window with the frequency ranges named, and **EQ on** switches it off and on without losing your bands. Your EQs and saved presets carry over and sound the same.
- **Share to Discord**: right-click a clip and pick **Share to Discord…** for copies that fit Discord's upload limit: 20 MB for free accounts, or pick Nitro Basic's 50 MB, Nitro's 1 GB, or any size. A clip that fits keeps its picture as it is; one that doesn't is made smaller (down to 720p or 540p, at 30 fps), and split into equal parts when one file would look too poor. Each copy has one sound track with what you heard and your mic. **Copy** puts them on the clipboard to paste in a chat with Ctrl+V; they're kept in the clips folder's **For Discord** folder, which the clip list leaves out.
- **Save clips with a key on its own**: the save-clip hotkey can be Home, End, Page Up/Down, Insert, Delete or an F-key by itself, not only a combination. A key on its own still does its usual thing in other apps (Home still jumps to the start of a line while you type), and it doesn't count while you hold Ctrl or Alt. Letters and numbers still need Ctrl or Alt, so typing doesn't save clips.

### Changed

- **Virtual surround is hidden for now**: it isn't ready yet, so it's off, and its settings are out of the channel panel until it is.

## 0.1.0-alpha.7 (2026-10-29)

Also brings everything in 0.1.0-alpha.5 and 0.1.0-alpha.6, which weren't released here on their own: recording only while a game is open, your games list, and no added delay for channels with a flat EQ (see [the changelog](https://github.com/HI-moogy/Sonora/blob/main/CHANGELOG.md)).

### New

- **Virtual surround for headphones**: turn on Surround for a channel to hear it from two speakers in a room in front of you, instead of inside your head, and set how far away they are. Only what you hear changes: meters and clips keep the channel's own sound, and the Mic channel never gets it. Like the EQ, it plays the channel through Sonora, so its apps don't play direct (and on Windows 10 it can't reach them).
- **Clip tags**: right-click a clip to tag it. Make your own tags, each in a color you pick, then tick them on any clip; they show on its thumbnail. Pick a tag above the clips to see only those. Change a tag's color or delete it from the same menu, and rename tags in ⚙ settings. An edited copy of a clip keeps its tags.

### Fixed

- **Windows 10**: adding an app failed with "Couldn't add that app: Catastrophic failure", then the app kept being offered again (#1). Windows 10 can't record single apps, which Sonora needs to process them. Now apps there play straight on their channel's device with the channel's volume and mute, and their meters come from Windows. The channel EQ and turning an app above 100% need Windows 11, and the channel's panel says so.
- **An app closed and started again** (a game, say) stayed out of its channel: its meter stayed dark and its clip track silent. Sonora kept the closed app and never routed the new one. Now an app that closes goes back to "joins when it starts" in its channel, keeping its volume and mute, and rejoins as soon as it plays again.

## 0.1.0-alpha.6 (2026-10-24)

### New

- **No added delay**: a channel with a flat EQ now plays its apps straight on its device instead of capturing them and playing them again. Volume, mute, meters and clip tracks work as before. With the EQ on (or an app turned above 100%), Sonora still plays the channel itself, and the channel's panel says which it is.
  - **Windows** uses its own per-app output setting. Measured: routed apps came out 83 ms after they were due, now 18 ms, the same as not routing them. An app that's already playing switches once it opens its sound again (restart it to switch now).
  - **Linux** moves the app's streams straight to the channel's output, right away. Measured on PulseAudio: 40 ms through Sonora, now none.

## 0.1.0-alpha.5 (2026-10-23)

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
