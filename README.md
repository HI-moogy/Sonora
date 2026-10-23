# Sonora

A lightweight per-app audio mixer for Windows and Linux, in the spirit of
SteelSeries GG's Sonar and Moments, without the rest of GG:

- **Channels for what you hear.** Send each app to Game, Chat, Aux or Media,
  each with its own volume, mute, 5-band EQ (with presets) and output device.
- **A cleaner mic.** Noise removal (RNNoise, or DeepFilterNet for much
  cleaner speech), EQ, and a before/after test. Your cleaned-up voice goes
  to Discord and games through a virtual microphone.
- **Clips** (Windows): press a hotkey to save the last moments of your game,
  with each channel on its own audio track to rebalance later.
- **Yours to style.** Pick a color preset or change any color.
- Small and quick: a native Rust app, no background services.

> **This is an early alpha.** Expect bugs, and please
> [report them](#reporting-a-bug).

## Download

Get the latest build from the [Releases](../../releases) page:

| System | File |
| --- | --- |
| Windows 11, 64-bit | `sonora-<version>-windows-x64.zip` |
| Linux, 64-bit (PipeWire or PulseAudio) | `sonora-<version>-linux-x86_64.tar.gz` |

## Install

### Windows

1. Unzip anywhere you like (for example `Documents\Sonora`).
2. Run `sonora.exe`.
3. Windows may say "Windows protected your PC" because the app isn't signed
   yet. Click **More info**, then **Run anyway**.

Nothing else is installed; settings live in `%APPDATA%\sonora`.

You need Windows 11 (sending apps to channels relies on a Windows feature
most Windows 10 versions lack). For clips, a graphics card that can encode
H.264 video: almost any NVIDIA, AMD or Intel graphics from the last several
years.

### Linux

```sh
tar -xzf sonora-*-linux-x86_64.tar.gz
cd sonora-*-linux-x86_64
./install.sh
```

Then open **Sonora** from your apps menu. `install.sh` installs for your
user only (in `~/.local`), with no root needed, so it also works in the
Steam Deck's desktop mode. Run it again to update; `./uninstall.sh` removes
it. Settings live in `~/.config/sonora`.

You need a desktop with PipeWire (with `pipewire-pulse`) or PulseAudio,
which is every mainstream one: Ubuntu, Linux Mint, Pop!_OS, Fedora, SteamOS
and more. For the tray icon, KDE Plasma and the Steam Deck work as they
are; on GNOME, install the AppIndicator extension (Ubuntu has it already).
Without a tray, closing the window quits Sonora.

Clips aren't available on Linux yet.

## Using it

Sonora shows a short tour the first time it opens; **Menu > Getting started
guide** shows it again.

**Mixer**

- Click a channel (Game, Chat, Aux, Media, Mic) to open its settings.
- Under **Add an app**, click **Add** next to an app to send it to that
  channel. From then on the channel's fader, EQ and mute control it.
- **Plays on** picks where that channel plays (headset, speakers, TV...).
  "If it disconnects" decides whether it falls back to your default output
  or goes silent.
- **Priority** trades delay for safety from crackles: High for games and
  voice, Normal or Low for music and video.
- **EQ** has presets for games, music, voice and video; **Save as...** keeps
  your own.
- Drag a fader (or scroll on it) to change the volume; double-click it for
  100%. **Mute** silences the whole channel. **Master** changes everything
  you hear.
- Your setup is saved, and apps you routed are routed again whenever they
  open.

**Your mic in Discord and games**

- On the Mic channel, pick your microphone, turn on **Noise removal**, and
  use **Hear the difference** to compare.
- **Windows:** install the free [VB-Audio Virtual Cable](https://vb-audio.com/Cable/)
  once, set **Send my voice to** "CABLE Input", and pick **CABLE Output** as
  your microphone in Discord.
- **Linux:** nothing to install. Set **Send my voice to** "Sonora Virtual
  Microphone", and pick **Sonora Virtual Microphone** in Discord.
- **Listen to my mic** plays your voice back to you to check how you sound;
  leave it off while talking.

**Clips (Windows)**

- The first time, the Clips tab asks what to record: the **whole screen**,
  or **only games and apps you choose** (found automatically).
- Press **Alt+F10** (changeable in the ⚙ clip settings) to save the last 30
  seconds (also changeable). A sound plays when it's saved.
- Clips in subfolders show too; sort them by date or game, or show one game.
- Click a clip to watch it, rebalance each audio track, and save a copy.

**Colors**

**Menu > Colors** has presets (Sonora, Sonora Light, Midnight, Forest,
Synthwave, Classic, High contrast) and every color to change. **Copy my
colors** gives you text to share; paste someone's to use theirs.

**Tray**

Closing the window keeps Sonora running in the tray, so routing (and clip
recording) keeps going. Click the icon to open it again; right-click it for
**Start with Windows** / **Start at login**, and **Quit**.

## Known problems

- **Windows:** if an app you routed is very quiet after Sonora closed or
  crashed, open Sonora again (it puts the volume back), or raise the app in
  Windows' Volume mixer.
- **Linux:** if Sonora is killed (not quit), apps it routed stay silent
  until it starts again, which puts them back.
- Don't run Sonora together with SteelSeries Sonar or similar mixers; they
  fight over the same apps.
- Routed apps play about 60 to 90 ms later than they would directly (half
  of that is Windows' own capture). Use High priority for games and voice.
- "Only games" recording hasn't been tried with many games yet.
- Linux has only been tested on PulseAudio so far; PipeWire reports are
  very welcome.

## Uninstall

- **Windows:** right-click the tray icon, untick **Start with Windows**,
  then **Quit**. Delete the Sonora folder, and `%APPDATA%\sonora` for the
  settings.
- **Linux:** run `./uninstall.sh` from the download (or delete
  `~/.local/bin/sonora` and `~/.local/share/applications/sonora.desktop`),
  and `~/.config/sonora` for the settings.

## Reporting a bug

Open **Menu > About and bug reports** and use **Report on GitHub** (or
[open an issue](../../issues/new/choose)). Please include:

1. The details **Copy details** puts on your clipboard (Sonora's version
   and your system).
2. **What you did**, step by step, **what happened**, and what you expected.
3. **How often**: always, sometimes, or once.
4. If Sonora crashed, the newest file from **Crash reports**.
5. A screenshot or short video if you can.
6. If settings seem involved, `config.json` from **Settings folder** (it
   lists your audio devices and the names of apps you routed).

## Building from source

You need [Rust](https://rustup.rs) (stable).

```sh
cargo build --release -p app        # target/release/sonora(.exe)
cargo test -p common -p app -p audio-engine --lib
```

On Linux, Sonora links to `libpulse.so.0` (part of every PipeWire or
PulseAudio desktop). The engine's sound-server tests run against the
machine's real server, so they're opt-in:
`cargo test -p audio-engine --lib pulse -- --ignored --test-threads 1`.

Release packages: `scripts/package-windows.ps1` and
`scripts/package-linux.sh`, which the release workflow runs for every
`v*` tag. See [docs/development.md](docs/development.md) for how the code
is laid out.

## License

The code is under the [MIT license](LICENSE). The **Sonora name and logo**
belong to Moogy and aren't covered by it: if you share a modified version,
please give it a different name and icon.

Sonora builds on open-source libraries, including
[DeepFilterNet](https://github.com/Rikorose/DeepFilterNet) and
[RNNoise](https://github.com/xiph/rnnoise) (via nnnoiseless) for noise
removal and [egui](https://github.com/emilk/egui) for its interface; their
licenses ship with every release in `THIRD-PARTY-NOTICES.txt`. Sonora isn't
affiliated with SteelSeries.
