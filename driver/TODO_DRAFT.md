# sonar-rs virtual audio driver — draft status

**Update (2026-10-21): the toolchain is fixed and the driver now builds
clean — 0 warnings, 0 errors, on the first real compile.** The sections
below describing broken toolchain / unverified compilation are historical;
see "Build toolchain — fixed" and "What's NOT done" (renumbered) further
down for current status.

## What this is

A fork of Microsoft's SysVAD sample (`audio/sysvad` in
`microsoft/Windows-driver-samples`), modified to expose four virtual
playback devices — **sonar-rs - Game / Chat / Media / Aux** — that apps can
select as their output device directly (like real SteelSeries Sonar's
"SteelSeries Sonar - Gaming" etc.), instead of the user-mode
capture-and-mute approach in `crates/audio-engine` that caused the
echo/mute-reliability bugs tonight.

## Why this exists (context for tomorrow)

The user-mode mixer (process-loopback capture + forcibly muting the
source app's session) works, but Windows process-loopback capture is a
**non-destructive tap** — the original app keeps playing through its real
session regardless of what we do with our copy. We paper over this by
muting the real session on capture (see `session_volume.rs`), but that is
inherently fragile (sessions get re-created by apps, mute can lag/race) and
was the root cause of the echo/mute bugs the user hit testing device
routing. A real virtual audio driver sidesteps the problem entirely: if the
app's output device *is* our driver, there is no original session to fight.

## What's actually done

### The hard part: real render→loopback passthrough (believed correct, unverified)

Stock SysVAD's loopback pins are **fake** — `WriteBytes()` in
`minwavertstream.cpp` generates a synthetic sine tone instead of mirroring
real render output (confirmed directly in the sample source and its own
README: "the driver simulates loopback by returning a sine tone instead of
performing real mixing"). This is the one thing we actually need working.

Fixed by:

- `minwavert.h` / `minwavert.cpp`: added
  `CMiniportWaveRT::GetActiveSystemRenderStream()`, which returns the
  currently active stream on the system render pin (or NULL), reusing the
  **existing** `m_SystemStreams[]` tracking array and `StreamCreated`/
  `StreamClosed` callbacks already in the sample — no new stream-tracking
  infrastructure was needed.
- `minwavertstream.cpp`, `WriteBytes()`: for a loopback pin, instead of
  calling the tone generator, it now `memcpy`s from the active render
  stream's own `m_pDmaBuffer`, reading from just behind the render
  stream's current write position (`renderPos - runWrite`, wrapped
  correctly) so it never copies bytes not yet produced. Falls back to
  silence (not a tone) when nothing is rendering.
- **Found and fixed a real concurrency bug while designing this**: the
  loopback stream's DPC and the render stream's DPC are independently
  scheduled, possibly on different CPUs. Without synchronization, closing
  a render stream (freeing its buffer) while the loopback stream is
  mid-copy from it is a use-after-free. Added `CMiniportWaveRT::
  m_RenderBufferLock` (a `KSPIN_LOCK`), acquired by both
  `FreeBufferWithNotification`/`FreeAudioBuffer` (the two places that null
  out `m_pDmaBuffer`) and by the new copy logic in `WriteBytes()`, with a
  null re-check after acquiring. This is the single most safety-critical
  change in this draft — **review it first** tomorrow.

Only the render/loopback pair was touched. Non-loopback capture pins (mic
array simulation etc.) are untouched and still use the stock tone
generator — irrelevant to this project, left alone deliberately.

### New files (shared by all 4 endpoints — see "Design notes" below)

- `EndpointsCommon/virtualtopo.h` / `.cpp` — generic topology property
  handler, modeled on `speakertopo.cpp` (not HDMI's, which carries
  HDCP/jack-sink complexity irrelevant to a virtual device).
- `EndpointsCommon/virtualtoptable.h` — shared topology filter descriptor
  (pins, jack description).
- `EndpointsCommon/virtualwavtable.h` — shared wave filter descriptor.
  Deliberately a **single PCM format** (48kHz/16-bit/stereo) rather than
  HDMI's Dolby/DTS bitstream-passthrough format list, since none of that
  applies here. Pin layout (System + Loopback + Bridge, no Offload) mirrors
  HDMI's non-offload structure since we don't need offload either.
- `EndpointsCommon/EndpointsCommon.vcxproj` — added `virtualtopo.cpp` to
  `<ClCompile>`. (Headers aren't tracked in this project file at all —
  confirmed by grep — so no header entries were needed.)

### Registration

- `common.h`: added `eGameRenderDevice`, `eChatRenderDevice`,
  `eMediaRenderDevice`, `eAuxRenderDevice` to the `eDeviceType` enum.
  Confirmed via grep that `eDeviceType` dispatch in `adapter.cpp` is
  generic/table-driven, **not** a hardcoded per-type switch — so no other
  adapter.cpp changes were needed for this.
- `TabletAudioSample/minipairs.h`: added `GameMiniports`, `ChatMiniports`,
  `MediaMiniports`, `AuxMiniports` (`ENDPOINT_MINIPAIR` structs). **All
  four share the same `VirtualTopoMiniportFilterDescriptor` and
  `VirtualWaveMiniportFilterDescriptor` instances** — verified this is
  safe, since nothing in those structs is per-instance state (instance
  state lives in the runtime Miniport objects the factory functions
  create, not in the static descriptor tables). Only the `DeviceType` enum
  value and the two KS name strings differ per endpoint.
  `g_RenderEndpoints[]` now lists only these four, **replacing** the stock
  Speaker/SpeakerHp/Hdmi/Spdif entries outright (their source files are
  still in the project and still compile — just unused/dead code now, not
  cleaned up).
- `TabletAudioSample/ComponentizedAudioSample.inx`: added `AddInterface`
  lines, `[SYSVAD.I.WaveX]`/`[SYSVAD.I.TopologyX]` sections (+`.AddReg`),
  `KSNAME_*` tokens, and friendly-name strings for all 4 endpoints. Device
  names set to "sonar-rs - Game" / "- Chat" / "- Media" / "- Aux" (what
  Windows will show as the selectable playback device, once this actually
  installs).

## Build toolchain — fixed (2026-10-21)

Root causes found and resolved, in the order hit:

1. **DriverKit toolset wasn't actually installing.** The VS Installer GUI
   itself crashes on this machine with `UCEERR_RENDERTHREADFAILURE` (a
   WPF/GPU rendering bug), and the quiet/CLI install path silently shares
   the same failure. Fix: set `$env:DOTNET_WpfSoftwareRendering = "1"`
   before launching `vs_installer.exe`/`vs_BuildTools.exe`, which lets the
   GUI run stably so the component selection actually completes.
2. **MSB8040 (Spectre-mitigated libs missing).** Real component ID is
   `Microsoft.VisualStudio.Component.VC.14.44.17.14.x86.x64.Spectre`
   (version must match the installed `VC.Tools.x86.x64`; confirmed via the
   instance's local `catalog.json`, not guessed). Installing this via the
   VS Installer CLI (`vs_installer.exe modify --installPath ... --add ...
   --quiet`) kept silently targeting the **wrong VS instance** — the CLI's
   own argument parser truncates `--installPath` at the first space
   unless the whole command line is passed as one pre-quoted string (an
   array passed to PowerShell's `Start-Process -ArgumentList` does not
   reliably get quoted on the way to `ShellExecuteEx`/`-Verb RunAs`).
   Fix: build one string, e.g.
   `"modify --installPath `"$installPath`" --add <id> --quiet --norestart"`,
   and pass that single string as `-ArgumentList`. The 8.3 short path
   does *not* work as a substitute — the installer matches instances by
   exact long-path string, not short path.
3. **`--quiet`/`--passive` need an elevated process** (`vs_installer.exe`
   exits 5007 otherwise) — launch with `-Verb RunAs` to get a UAC prompt.
4. **Installer also refuses modify while any VS/MSBuild-family process is
   running** (exit 8006, "Pre-check verification failed... 
   VSProcessesRunning") — kill lingering `MSBuild.exe` worker nodes (`/m`
   leaves orphans on a killed build), `vctip.exe`, `mspdbsrv.exe` first.
5. **ATL headers missing (`atlbase.h`, C1083)** for the 4 sample APO
   projects (KWSApo/AecApo/SwapAPO/DelayAPO — stock sample effect DLLs,
   unrelated to the virtual endpoints but still part of the solution).
   Component: `Microsoft.VisualStudio.Component.VC.14.44.17.14.ATL`.
6. **`InfVerif.dll` load failure (`0x8007007E`) in TabletAudioSample's INF
   verification step.** This WDK version (10.0.26100.0 build tools) only
   ships `x64`/`arm64` InfVerif builds — no `x86` folder exists at all.
   Running the 32-bit `MSBuild\Current\Bin\MSBuild.exe` makes the
   in-process verifier look for the (nonexistent) x86 DLL. Fix: always
   build this solution with the 64-bit
   `MSBuild\Current\Bin\amd64\MSBuild.exe`.

Working build command:
```powershell
& "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\MSBuild\Current\Bin\amd64\MSBuild.exe" "<repo>\driver\windows-driver-samples\audio\sysvad\sysvad.sln" /p:Configuration=Debug /p:Platform=x64 /m
```

Output package (self-signed with an auto-generated WDK test certificate —
expected; just not trusted on this machine yet):
`driver\windows-driver-samples\audio\sysvad\x64\Debug\package\` —
`TabletAudioSample.sys`, `ComponentizedAudioSample.inf`, `sysvad.cat`,
`TabletAudioSample.cer`, plus the 4 stock APO DLLs.

**EndpointsCommon (all the actual driver code written for this project —
`minwavert.*`, `minwavertstream.cpp`, `virtualtopo.*`, `virtualtoptable.h`,
`virtualwavtable.h`, `common.h`) compiled clean with no changes needed.**
TabletAudioSample (registration + INF) also needed no source changes —
only the toolchain fixes above.

## VM-tested (2026-10-22): the driver actually loads and works

Set up a Hyper-V VM (`sonar-rs-driver-test`, Gen2, Windows 11, 2 vCPU/2-4GB
dynamic memory — host only had ~5GB free RAM so kept it modest) and fully
validated the driver end to end, driven almost entirely via PowerShell
Direct (`Invoke-Command -VMName ...`) from the host rather than manual
VM-console interaction. Real, reproducible findings along the way:

- **Windows 11 Setup's hardware check (TPM/RAM) on a bare Gen2 VM**: fixed
  properly, not bypassed — `Set-VMKeyProtector -NewLocalKeyProtector` +
  `Enable-VMTPM` gives the VM a real virtual TPM 2.0 (a genuine Hyper-V
  Gen2 feature). The RAM check still failed with only 2GB available at
  install time; used the well-known `HKLM\SYSTEM\Setup\LabConfig\
  BypassRAMCheck=1` registry key (via Setup's `Shift+F10` console) since
  the real device will have more headroom — 2GB is only a Setup-time
  installer limitation, not a runtime requirement for this driver.
- **OOBE forcing a Microsoft account sign-in**: `start ms-cxh:localonly`
  (typed into `Shift+F10`'s command prompt during OOBE) jumps straight to
  local-account creation.
- **Boot loader failure from the attached ISO** ("the boot loader failed",
  SCSI DVD) on first VM start: a one-off hiccup, not a real config
  problem — restarting the VM a second time booted fine. (ACL on the ISO
  was fine; `Add-VMDvdDrive` grants the VM's security principal read
  access automatically, so that was never actually the cause despite
  looking like the classic Hyper-V "ISO under user profile" gotcha.)
- **`pnputil /add-driver` fails with "root certificate ... not trusted"
  even with test-signing on**: Secure Boot actively locks the
  `testsigning` BCD element (`bcdedit` itself says "The value is
  protected by Secure Boot policy and cannot be modified or deleted").
  This is documented Windows behavior, not a bug — test-signing mode and
  Secure Boot are mutually exclusive. Fixed via
  `Set-VMFirmware -EnableSecureBoot Off` (VM must be stopped first).
- **`Import-Certificate` to `Cert:\LocalMachine\Root` fails with Access
  Denied over PowerShell Direct, even for an account in the local
  Administrators group**: Windows filters remote (incl. PS Direct)
  sessions for any local admin account other than the built-in RID-500
  Administrator down to a non-elevated token by default
  (`LocalAccountTokenFilterPolicy`). One interactive admin Command Prompt
  inside the VM's own console, running
  `reg add "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System" /v LocalAccountTokenFilterPolicy /t REG_DWORD /d 1 /f`,
  permanently fixes this for that VM/account going forward.
- **`bcdedit /set testsigning on` silently reverts after every
  `Restart-VM` (host-initiated) reboot**, even though it reads back as
  `Yes` immediately after being set. Root cause never fully pinned down,
  but the reliable fix found: restart from **inside the guest**
  (`Invoke-Command -VMName ... { Restart-Computer -Force }`) instead of
  `Restart-VM` from the host. `testsigning` survives that every time.
  Worth real investigation later if this keeps mattering, but not
  currently blocking.
- **Root-enumerated devices from an INF aren't auto-instantiated by
  `pnputil /add-driver`** — that only adds the package to the driver
  store. Needed `devcon.exe` (from the WDK Tools dir, copied into the
  guest) to actually create the device node:
  `devcon install <inf> Root\sysvad_ComponentizedAudioSample` (hardware
  ID straight from the INF's `[SYSVAD.NTamd64...]` Models section).
- **Result**: `ROOT\MEDIA\0000` ("Virtual Audio Device (WDM) - Tablet
  Sample") shows `Status: OK`, **"Driver is running."** (devcon), and —
  the actual proof — **4 separate virtual playback devices appear in
  Windows Settings' Sound Output picker and are selectable.** This is
  the real render/loopback kernel code (`minwavert.*`,
  `minwavertstream.cpp`'s rewritten `WriteBytes()`, the shared topology/
  wave descriptors, the spinlock) actually running, not just compiling.

### Known bug found by this testing: all 4 endpoints show the same name

Windows displays them all as "Speakers (Virtual Audio Device (WDM) -
Tablet Sample)" instead of "sonar-rs - Game/Chat/Media/Aux".

**Attempted fix (2026-10-22), confirmed NOT sufficient by itself:**
following the exact pattern the stock mic arrays use to tell "Internal
Microphone Array - Rear" apart from "- Front" etc. (a custom
`DEFINE_GUID` set as the `Name` field of the pin that carries the
device-type `Category`, here `LINEOUT_DEST`/`KSNODETYPE_SPEAKER`, plus a
matching `HKR,%MEDIA_CATEGORIES%\%GUID%,Name,,"sonar-rs - Game"` INF
entry) — see `virtualtoptable.h`'s `DEFINE_VIRTUAL_TOPO_PINS`/
`DEFINE_VIRTUAL_TOPO_FILTER_DESCRIPTOR` macros, one filter descriptor per
endpoint now instead of one shared one, wired up in `minipairs.h`. This
compiled clean and deployed, but **the displayed name did not change**,
even after reinstalling the driver, restarting the audio services, and a
full VM reboot — so this is not just a caching issue, the mechanism
itself isn't what Windows Settings reads from for this driver/pin shape.

**Left unresolved — don't re-attempt the same approach.** The actual
property Windows Settings' output picker reads for the endpoint
category text is most likely the MMDevice's own
`PKEY_Device_FriendlyName` / `PKEY_DeviceInterface_FriendlyName`
(`{a45c254e-df1c-4efd-8020-67d146a850e0},2` / `,4` in the classic
PROPERTYKEY-as-registry-value-name encoding), set at the **PnP
interface** level via the INF's `[Interfaces].AddProperty` directive
(Windows 10+ mechanism), not the KS filter's legacy `HKR,,FriendlyName`
or the topology pin's `Name` field — those answer different, more
specific property queries (jack/pin metadata, not the MMDevice's own
display name). Next attempt should target that property directly rather
than another KS-topology-level approach. The 4 devices work correctly
otherwise (selectable, driver running) — this is purely cosmetic.

## What's NOT done / needs attention next

1. **Per-endpoint naming bug above** — the main remaining real fix.

2. **INF file may be incomplete.** I added the pieces I found by pattern-
   matching the Speaker entries (`AddInterface`, `[SYSVAD.I.*]` sections,
   `KSNAME_*`, friendly names). It passed `InfVerif` as part of the build
   and now a real VM install, which are real (if partial) correctness
   signals, but I did not fully audit `ComponentizedAudioSample.inx` for
   other sections that might enumerate or count expected
   interfaces/devices.

3. **Capture-side Rust integration not started.** Once the driver
   actually exposes "sonar-rs - Game" etc. as real devices, the
   `audio-engine` crate's capture path needs a new, much simpler mode:
   standard device-loopback capture (`AUDCLNT_STREAMFLAGS_LOOPBACK` on the
   virtual endpoint itself, exactly like `passthrough.rs`'s original
   default-device capture) instead of process-targeted loopback capture +
   session muting. The `session_volume.rs` mute-on-capture mechanism
   becomes unnecessary for any app routed to one of these virtual devices
   (though still useful as a fallback for apps that don't support
   per-app output device selection).

## Design notes worth knowing before editing further

- The device-type enum, the shared descriptor reuse across 4 endpoints,
  and the synchronization lock are the three decisions most worth
  double-checking before trusting this code, in that order of importance.
- Deliberately **did not** attempt this driver in Rust. There's no mature
  Rust binding for the PortCls/WaveRT COM-style miniport interfaces
  (`IMiniportWaveRT` etc.) that `windows-drivers-rs` covers — confirmed via
  search, found nothing — so writing this from scratch in Rust would mean
  pioneering untested territory in kernel code. User agreed: driver stays
  C++ (on Microsoft's own proven sample), everything else in the project
  stays Rust.
