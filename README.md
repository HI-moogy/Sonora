# Sonora (alpha)

A lightweight replacement for SteelSeries GG's Sonar mixer and Moments clips:
route each app to its own channel (Game, Chat, Aux, Media, Mic) with its own
volume, EQ and output device, and save the last few seconds of gameplay with a
hotkey.

**This is an early alpha.** Expect bugs, and please report them (see the end).

## What you need

- Windows 11, 64-bit. (Windows 10 isn't tested, and sending apps to channels
  relies on a Windows feature that most Windows 10 versions don't have.)
- For clips: a graphics card that can encode H.264 video, which is almost any
  NVIDIA, AMD or Intel graphics from the last several years.

## Install

1. Unzip anywhere you like (for example `Documents\Sonora`).
2. Run `sonora.exe`.
3. Windows may say "Windows protected your PC" because the app isn't signed.
   Click **More info**, then **Run anyway**.

Nothing is installed; settings live in `%APPDATA%\sonora`.

## Using it

**Mixer tab**

- Click a channel (Game, Chat, Aux, Media, Mic) to open its settings on the
  right.
- Under **Add an app**, click **Add** next to an app to send it to that
  channel. Its normal volume is turned down and Sonora plays it instead, so
  the channel's fader, EQ and mute control it.
- **Plays on** picks where that channel plays (headset, speakers, TV...).
  "If it disconnects" decides whether it falls back to your default output or
  goes silent.
- **EQ** has presets (Pop, Rap / Hip-Hop, R&B, Jazz, Rock, Bass Boost,
  Footsteps and more), and **Save as...** keeps your own.
- Drag a channel's fader (or scroll on it) to change its volume; double-click
  it for 100%. **Mute** silences the whole channel.
- Your setup is saved and comes back next time, including apps you open
  later.

**Your mic in Discord and games**

- Sonora cleans up your mic (noise removal and EQ on the Mic channel). For
  Discord and games to get that cleaned-up voice, install the free
  [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) (one install, then
  restart Sonora).
- On the Mic channel, set **Send my voice to** "CABLE Input".
- In Discord (and games), pick **CABLE Output** as your microphone.
- **Listen to my mic** plays your voice back to you to check how you sound;
  leave it off while talking.

**Clips tab**

- The first time, it asks what to record: the **whole screen**, or **only
  games and apps you choose** (games are found automatically; only the game's
  window is recorded, and nothing while no game is open).
- Press **Alt+F10** (changeable in the ⚙ settings) to save the last 30 seconds
  (also changeable). A sound plays when it's saved.
- Click a clip to open it. You can change each audio track's volume and save
  a new copy.

**Tray icon**

- Closing the window keeps Sonora running in the tray (by the clock, under
  the ^ arrow). Click the icon to open it again.
- Right-click the icon for **Start with Windows** and **Quit**.

## Known problems

- If an app you routed is suddenly very quiet after Sonora closed or
  crashed: open Sonora again (it puts the volume back), or raise the app in
  Windows' Volume mixer.
- Don't run it together with SteelSeries GG Sonar or similar mixers; they
  fight over the same apps.
- "Only games" recording hasn't been tried with many games yet.
- Clips use roughly 250 MB of memory at 1080p, 60 fps, 60 seconds.

## Uninstall

1. Right-click the tray icon: untick **Start with Windows**, then **Quit**.
2. Delete the Sonora folder, and `%APPDATA%\sonora` for the settings.

## Reporting a bug

Send it to whoever gave you Sonora, with:

1. **Version**: shown at the top of the Mixer tab (for example
   `Sonora 0.1.0-alpha.1`).
2. **Windows version**: press Win+R, type `winver`, press Enter.
3. **Graphics card**, if it's about clips.
4. **What you did**, step by step.
5. **What happened**, and what you expected instead.
6. **Can you make it happen again?** Always, sometimes, or once.
7. A **screenshot** or short video if you can.
8. If settings seem involved: `%APPDATA%\sonora\config.json` (it lists your
   audio devices and the names of apps you routed).
