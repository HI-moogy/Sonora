use std::collections::{HashMap, VecDeque};
#[cfg(windows)]
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
#[cfg(not(windows))]
use std::sync::Condvar;
use std::time::{Duration, Instant};

use common::{Priority, Surround};

use crate::atomic_f32::AtomicF32;
use crate::denoise::MicCleanup;
use crate::eq::{EqBand, EqBands, EQ_BANDS};
use crate::error::{self, Result};
use crate::platform::{self, DeviceOutput};
use crate::surround::VirtualSurround;

pub(crate) const CHANNELS: usize = 2;
pub(crate) const SAMPLE_RATE: f32 = 48_000.0;
/// How full each output device is kept, by the most urgent bus playing on
/// it: everything queued there is delay. The audio engine itself needs a
/// little over 10 ms.
#[cfg(windows)]
fn device_target_ms(priority: Priority) -> f32 {
    match priority {
        Priority::High => 15.0,
        Priority::Normal => 50.0,
        Priority::Low => 120.0,
    }
}

/// On Linux the sound server takes audio in blocks (PipeWire's are about
/// 21 ms by default), so each device keeps at least one block queued.
#[cfg(not(windows))]
fn device_target_ms(priority: Priority) -> f32 {
    match priority {
        Priority::High => 30.0,
        Priority::Normal => 60.0,
        Priority::Low => 120.0,
    }
}

/// How much audio an app's queue may build up (say after a hiccup) before
/// the oldest is dropped to catch up, rather than staying that late.
fn queue_limit_ms(priority: Priority) -> f32 {
    match priority {
        Priority::High => 30.0,
        Priority::Normal => 100.0,
        Priority::Low => 250.0,
    }
}

fn ms_to_frames(ms: f32) -> u32 {
    (ms / 1000.0 * SAMPLE_RATE) as u32
}

/// Bounds how far a source's buffer can grow if the render thread falls
/// behind, so a stalled render loop can't leak memory indefinitely.
pub(crate) const MAX_BUFFERED_SAMPLES: usize = 48_000 * CHANNELS * 2;

/// How an app should be routed, as the render loop works it out from its
/// channel each tick; its capture thread carries it out.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RoutePlan {
    /// Play straight on the channel's device, with no delay added (Windows'
    /// per-app output; on Linux, its streams moved there): possible when
    /// nothing needs the mixer's processing
    /// (flat EQ, no boost above 100%) and the channel plays on its own device.
    pub(crate) direct: bool,
    /// The channel's device; `None` is Windows' default.
    pub(crate) device: Option<String>,
    /// The app's own volume while direct: its fader times its channel's
    /// and the master (0 when either is muted).
    pub(crate) volume: f32,
}

pub(crate) struct SourceInner {
    /// `None` for the microphone source, which has no owning process.
    pub(crate) pid: Option<u32>,
    /// The app's program as apps are saved: its lowercase executable name
    /// ("deadlock.exe"), read when it was added. `None` for the microphone,
    /// or if it couldn't be read.
    program: Option<String>,
    pub(crate) name: String,
    bus: Mutex<String>,
    gain: AtomicF32,
    muted: AtomicBool,
    /// Peak since the UI last looked, measured by the render thread.
    level: AtomicF32,
    /// The capture device, for the microphone; `None` is the system default.
    input_device: Option<String>,
    pub(crate) samples: Mutex<VecDeque<f32>>,
    pub(crate) stop: Arc<AtomicBool>,
    /// Set by the render loop, carried out by the capture thread.
    pub(crate) plan: Mutex<RoutePlan>,
    /// Set by the capture thread once the app plays straight on its
    /// channel's device: its captured audio (at the app's own volume) is
    /// then only metered and recorded, not played again.
    pub(crate) direct: AtomicBool,
    /// Set by the capture thread once the app has exited: it captures
    /// nothing more, and has given the app's processes back their output.
    pub(crate) exited: AtomicBool,
    /// The app's audio processes currently turned down. Held while turning
    /// them down or restoring them, so removal can't race with the capture
    /// thread turning them down again.
    #[cfg(windows)]
    pub(crate) routed: Mutex<HashSet<u32>>,
    /// Set by the capture thread once it has stopped and given the app back
    /// its output (which happens there on Linux).
    #[cfg(not(windows))]
    pub(crate) finished: Finished,
}

/// Whether a capture thread has finished, for waiting on it.
#[cfg(not(windows))]
#[derive(Default)]
pub(crate) struct Finished {
    done: Mutex<bool>,
    changed: Condvar,
}

#[cfg(not(windows))]
impl Finished {
    fn set(&self) {
        *self.done.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait(&self, timeout: Duration) {
        let done = self.done.lock().unwrap();
        let _ = self.changed.wait_timeout_while(done, timeout, |done| !*done).unwrap();
    }
}

/// Marks its source's capture as finished when dropped, so a capture thread
/// reports it however it ends. Make it the thread's first local, so it's
/// dropped last.
#[cfg(not(windows))]
pub(crate) struct FinishOnDrop(pub(crate) Arc<SourceInner>);

#[cfg(not(windows))]
impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        self.0.finished.set();
    }
}

/// How long removing a source (or quitting) waits for its capture to give
/// the app back its output.
#[cfg(not(windows))]
const FINISH_TIMEOUT: Duration = Duration::from_secs(2);

impl SourceInner {
    fn new(pid: Option<u32>, name: String, bus: String, input_device: Option<String>, gain: f32, muted: bool) -> Self {
        Self {
            pid,
            program: None,
            name,
            bus: Mutex::new(bus),
            gain: AtomicF32::new(gain),
            muted: AtomicBool::new(muted),
            level: AtomicF32::new(0.0),
            input_device,
            samples: Mutex::new(VecDeque::new()),
            stop: Arc::new(AtomicBool::new(false)),
            plan: Mutex::default(),
            direct: AtomicBool::new(false),
            exited: AtomicBool::new(false),
            #[cfg(windows)]
            routed: Mutex::default(),
            #[cfg(not(windows))]
            finished: Finished::default(),
        }
    }

    /// For an app the mixer doesn't capture: its level, as Windows meters it.
    #[cfg(windows)]
    pub(crate) fn report_level(&self, peak: f32) {
        self.level.raise_to(peak);
    }

    /// Stops capturing and, for an app, gives it back its own output (on
    /// Linux its capture thread does, as it stops: see [`Self::wait_restored`]).
    fn stop_and_restore(&self) {
        self.stop.store(true, Ordering::Relaxed);
        #[cfg(windows)]
        if self.pid.is_some() {
            let mut routed = self.routed.lock().unwrap();
            crate::session_volume::unroute(&routed);
            routed.clear();
        }
    }

    /// Waits for a stopped capture to have given the app back its output,
    /// so quitting right after can't leave the app on a silent sink.
    fn wait_restored(&self) {
        #[cfg(not(windows))]
        self.finished.wait(FINISH_TIMEOUT);
    }
}

struct BusInner {
    gain: AtomicF32,
    muted: AtomicBool,
    /// Peak since the UI last looked.
    level: AtomicF32,
    eq: Mutex<EqBands>,
    surround: Mutex<VirtualSurround>,
    /// `None` renders to the system default output device.
    device_id: Mutex<Option<String>>,
    /// Whether the bus plays on the default device while its own device is
    /// unavailable, rather than going silent.
    fallback_to_default: AtomicBool,
    /// Whether you hear the bus. One you don't hear is still mixed, metered
    /// and passed to the bus tap (clips); it just isn't played.
    heard: AtomicBool,
    priority: Mutex<Priority>,
    /// Roughly how late the bus plays, in ms: what's queued on its device
    /// plus its apps' queues.
    delay_ms: AtomicF32,
    /// A send bus carries a voice to other apps through its device (a
    /// virtual cable): your master volume doesn't change it, and it never
    /// falls back to the default device, which would play it to you.
    send: AtomicBool,
    /// Also play a copy on the default device, to hear a send bus.
    monitor: AtomicBool,
    /// Whether its apps can play straight on its device (see
    /// [`RoutePlan::direct`]), as of the last render tick.
    direct: AtomicBool,
}

/// Receives every bus's audio as the mixer renders it: after the bus's EQ,
/// before its volume fader and mute, so recordings don't depend on how loud
/// you're listening. Called on the render thread, so it must be cheap.
pub trait BusTap: Send + Sync {
    /// `samples` is interleaved stereo f32 at 48 kHz.
    fn bus_audio(&self, bus: &str, samples: &[f32]);
}

type SharedTap = Arc<Mutex<Option<Arc<dyn BusTap>>>>;

/// A snapshot of one source's current state for UI display.
pub struct SourceInfo {
    pub id: u64,
    pub pid: Option<u32>,
    /// The app's lowercase executable name ("deadlock.exe"), read when it
    /// was added; `None` for the microphone, or if it couldn't be read.
    pub program: Option<String>,
    pub name: String,
    pub bus: String,
    pub gain: f32,
    pub muted: bool,
    /// Peak since the previous snapshot.
    pub level: f32,
    /// The microphone's capture device; `None` is the system default (and
    /// always `None` for apps).
    pub input_device: Option<String>,
    /// Whether the app plays straight on its channel's device, with no
    /// delay added (see [`BusInfo::direct`]).
    pub direct: bool,
    /// The app has closed: nothing more is captured, and it already has its
    /// own output back. Remove it; the app, started again, is a new process
    /// to add.
    pub exited: bool,
}

/// A snapshot of one bus's current state for UI display.
pub struct BusInfo {
    pub name: String,
    pub gain: f32,
    pub muted: bool,
    /// Peak since the previous snapshot.
    pub level: f32,
    pub eq: [EqBand; EQ_BANDS],
    /// Whether its EQ is on (off, it keeps its bands but changes nothing).
    pub eq_enabled: bool,
    /// `None` means this bus renders to the system default output device.
    pub device_id: Option<String>,
    /// See [`Mixer::set_bus_fallback`].
    pub fallback_to_default: bool,
    /// See [`Mixer::set_bus_heard`].
    pub heard: bool,
    /// See [`Mixer::set_bus_priority`].
    pub priority: Priority,
    /// Roughly how late this bus plays, in ms (the mixer's own delay, not
    /// the device's or the app's).
    pub delay_ms: f32,
    /// See [`Mixer::set_bus_send`].
    pub send: bool,
    /// See [`Mixer::set_bus_monitor`].
    pub monitor: bool,
    /// Whether its apps can play straight on its device, with no delay
    /// added: nothing needs the mixer (EQ flat, surround off) and the device
    /// is there. An app turned above 100% still goes through the mixer, and
    /// one already playing moves once it opens its audio again.
    pub direct: bool,
    /// See [`Mixer::set_bus_surround`].
    pub surround: Surround,
}

/// Captures audio from any number of processes (plus optionally the
/// microphone) simultaneously, applies per-source gain, per-bus gain and
/// 5-band EQ, and renders the sum to the default output device. One
/// background thread per captured source plus one render thread; the
/// mixer itself only coordinates shared, lock-protected state.
pub struct Mixer {
    sources: Arc<Mutex<HashMap<u64, Arc<SourceInner>>>>,
    buses: Arc<Mutex<HashMap<String, Arc<BusInner>>>>,
    next_id: Mutex<u64>,
    master_gain: Arc<AtomicF32>,
    master_level: Arc<AtomicF32>,
    render_stop: Arc<AtomicBool>,
    bus_tap: SharedTap,
    mic_cleanup: Arc<MicCleanup>,
    /// What the platform set up for the mixer (on Linux, the virtual mic);
    /// taken down with it.
    _platform: platform::MixerSetup,
}

impl Mixer {
    pub fn start() -> Result<Self> {
        // Before anything is routed: give back any app a previous run left
        // turned down (Windows) or moved (Linux), and set up what the
        // platform needs.
        let platform_setup = platform::MixerSetup::start()?;
        let sources = Arc::new(Mutex::new(HashMap::new()));
        let buses = Arc::new(Mutex::new(HashMap::<String, Arc<BusInner>>::new()));
        let master_gain = Arc::new(AtomicF32::new(1.0));
        let master_level = Arc::new(AtomicF32::new(0.0));
        let render_stop = Arc::new(AtomicBool::new(false));
        let bus_tap: SharedTap = Arc::new(Mutex::new(None));

        {
            let sources = sources.clone();
            let buses = buses.clone();
            let master_gain = master_gain.clone();
            let master_level = master_level.clone();
            let render_stop = render_stop.clone();
            let bus_tap = bus_tap.clone();
            std::thread::Builder::new()
                .name("mixer-render".into())
                .spawn(move || {
                    if let Err(error) = run_render_loop(
                        sources,
                        buses,
                        master_gain,
                        master_level,
                        render_stop,
                        bus_tap,
                    ) {
                        eprintln!("mixer render thread exited with error: {error:?}");
                    }
                })
                .expect("spawn mixer render thread");
        }

        Ok(Self {
            sources,
            buses,
            next_id: Mutex::new(0),
            master_gain,
            master_level,
            render_stop,
            bus_tap,
            mic_cleanup: Arc::default(),
            _platform: platform_setup,
        })
    }

    /// The microphone's noise removal settings and before/after test.
    pub fn mic_cleanup(&self) -> &MicCleanup {
        &self.mic_cleanup
    }

    /// Starts (or with `None`, stops) handing every bus's rendered audio to
    /// `tap`.
    pub fn set_bus_tap(&self, tap: Option<Arc<dyn BusTap>>) {
        *self.bus_tap.lock().unwrap() = tap;
    }

    fn next_id(&self) -> u64 {
        let mut next_id = self.next_id.lock().unwrap();
        let id = *next_id;
        *next_id += 1;
        id
    }

    /// Starts capturing `pid` into the named bus (created on first use) and
    /// returns a handle for later gain/mute/removal calls. Blocks briefly
    /// until the capture thread confirms activation succeeded, so a bad pid
    /// surfaces as an `Err` here rather than silently failing in the
    /// background.
    pub fn add_source(
        &self,
        pid: u32,
        name: impl Into<String>,
        bus: impl Into<String>,
    ) -> Result<u64> {
        // Capturing our own process would loop the mix back into itself, and
        // turning the app down would silence our own output.
        if pid == std::process::id() {
            return Err(error::invalid_argument());
        }
        let bus_name = bus.into();
        self.ensure_bus(&bus_name);

        let inner = Arc::new(SourceInner {
            program: crate::process_executable_name(pid).map(|name| name.to_lowercase()),
            ..SourceInner::new(Some(pid), name.into(), bus_name, None, 1.0, false)
        });

        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_source = inner.clone();
        std::thread::Builder::new()
            .name(format!("capture-{pid}"))
            .spawn(move || platform::run_app_capture(pid, thread_source, ready_tx))
            .expect("spawn capture thread");

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| error::timed_out())??;

        // The capture thread turns the app's own output down right away, so
        // the mixer's processed copy becomes the only audible path.
        let id = self.next_id();
        self.sources.lock().unwrap().insert(id, inner);
        Ok(id)
    }

    /// Starts capturing a microphone (`device_id` from
    /// [`crate::devices::list_input_devices`], or the system default when
    /// `None`), cleaned up as [`Self::mic_cleanup`] says, into the named bus. Same
    /// activation/blocking contract as [`Self::add_source`].
    ///
    /// The mixer records one microphone: this replaces the one running, if
    /// any, once the new one has started (on failure the old one keeps
    /// going). Two would put two copies of your voice in the mix, a few ms
    /// apart, which sounds doubled and underwater.
    pub fn add_mic_source(
        &self,
        name: impl Into<String>,
        bus: impl Into<String>,
        device_id: Option<String>,
    ) -> Result<u64> {
        self.start_mic(name.into(), bus.into(), device_id, 1.0, false)
    }

    /// Switches microphone source `id` to another capture device, keeping
    /// its bus, gain and mute. Returns the source's new id. On failure the
    /// old capture keeps running.
    pub fn set_mic_device(&self, id: u64, device_id: Option<String>) -> Result<u64> {
        let old = self
            .sources
            .lock()
            .unwrap()
            .get(&id)
            .filter(|s| s.pid.is_none())
            .cloned();
        let Some(old) = old else {
            return Err(error::invalid_argument());
        };
        let bus = old.bus.lock().unwrap().clone();
        self.start_mic(old.name.clone(), bus, device_id, old.gain.load(), old.muted.load(Ordering::Relaxed))
    }

    fn start_mic(&self, name: String, bus: String, device_id: Option<String>, gain: f32, muted: bool) -> Result<u64> {
        self.ensure_bus(&bus);
        let inner = Arc::new(SourceInner::new(None, name, bus, device_id.clone(), gain, muted));

        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_source = inner.clone();
        let cleanup = self.mic_cleanup.clone();
        std::thread::Builder::new()
            .name("capture-mic".into())
            .spawn(move || platform::run_mic_capture(thread_source, device_id, cleanup, ready_tx))
            .expect("spawn mic capture thread");

        let started = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| Err(error::timed_out()));
        if started.is_err() {
            // So a capture that starts after all can't run unseen.
            inner.stop_and_restore();
        }
        started?;

        let id = self.next_id();
        let replaced = swap_in_mic(&mut self.sources.lock().unwrap(), id, inner);
        for old in replaced {
            old.stop_and_restore();
            old.wait_restored();
        }
        Ok(id)
    }

    /// Stops capturing `id`; its thread exits and cleans up its capture
    /// within one poll interval (<=100ms). If it was an app, gives
    /// the app back its own volume.
    pub fn remove_source(&self, id: u64) {
        let removed = self.sources.lock().unwrap().remove(&id);
        if let Some(source) = removed {
            source.stop_and_restore();
            source.wait_restored();
        }
    }

    /// Restores apps a previous run left turned down because it didn't exit
    /// cleanly (crash, killed). Cheap when there's nothing to restore.
    pub fn recover_routed_volumes(&self) {
        #[cfg(windows)]
        {
            if !crate::session_volume::has_pending_routes() {
                return;
            }
            let routed: Vec<u32> = self
                .sources
                .lock()
                .unwrap()
                .values()
                .flat_map(|s| s.routed.lock().unwrap().iter().copied().collect::<Vec<_>>())
                .collect();
            crate::session_volume::recover(&routed);
        }
    }

    pub fn set_source_gain(&self, id: u64, gain: f32) {
        if let Some(source) = self.sources.lock().unwrap().get(&id) {
            source.gain.store(gain.max(0.0));
        }
    }

    pub fn set_source_muted(&self, id: u64, muted: bool) {
        if let Some(source) = self.sources.lock().unwrap().get(&id) {
            source.muted.store(muted, Ordering::Relaxed);
        }
    }

    pub fn set_source_bus(&self, id: u64, bus: impl Into<String>) {
        let bus_name = bus.into();
        self.ensure_bus(&bus_name);
        if let Some(source) = self.sources.lock().unwrap().get(&id) {
            *source.bus.lock().unwrap() = bus_name;
        }
    }

    pub fn set_bus_gain(&self, bus: &str, gain: f32) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            bus.gain.store(gain.max(0.0));
        }
    }

    pub fn set_bus_muted(&self, bus: &str, muted: bool) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            bus.muted.store(muted, Ordering::Relaxed);
        }
    }

    /// Makes `bus` a send bus: it carries a voice to other apps through its
    /// device (a virtual cable), so the master volume doesn't apply and it
    /// never falls back to the default device. Off by default.
    pub fn set_bus_send(&self, bus: &str, send: bool) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            bus.send.store(send, Ordering::Relaxed);
        }
    }

    /// Also plays `bus` on the default device, to hear what a send bus
    /// sends (your own mic, say). Off by default.
    pub fn set_bus_monitor(&self, bus: &str, monitor: bool) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            bus.monitor.store(monitor, Ordering::Relaxed);
        }
    }

    /// How much delay `bus` may have (`Normal` by default).
    pub fn set_bus_priority(&self, bus: &str, priority: Priority) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            *bus.priority.lock().unwrap() = priority;
        }
    }

    /// Whether `bus` is played to you (on by default). Turning it off keeps
    /// the bus in clips and on its meter; it's for a microphone you don't
    /// want to hear yourself through.
    pub fn set_bus_heard(&self, bus: &str, heard: bool) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            bus.heard.store(heard, Ordering::Relaxed);
        }
    }

    /// Plays `bus` as if from speakers in a room around you (for headphones),
    /// or not, and sets how far away they are. Turning it on or off fades
    /// over a few ms. Only what you hear changes: meters and clips keep the
    /// bus's own sound, and a send bus never gets it. Off by default.
    pub fn set_bus_surround(&self, bus: &str, surround: Surround) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            bus.surround.lock().unwrap().set(surround);
        }
    }

    /// Sets band `index` (0..[`EQ_BANDS`]) of `bus`'s EQ.
    pub fn set_bus_eq_band(&self, bus: &str, index: usize, band: EqBand) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            bus.eq.lock().unwrap().set_band(index, band);
        }
    }

    /// Turns `bus`'s EQ on or off; off, it keeps its bands.
    pub fn set_bus_eq_enabled(&self, bus: &str, enabled: bool) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            bus.eq.lock().unwrap().set_enabled(enabled);
        }
    }

    /// Routes `bus`'s output to `device_id` (from
    /// [`crate::devices::list_output_devices`]), or back to the system
    /// default device when `device_id` is `None`. Takes effect within one
    /// render tick; the render thread opens/closes device clients lazily
    /// as bus-to-device assignments change.
    pub fn set_bus_device(&self, bus: &str, device_id: Option<String>) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            *bus.device_id.lock().unwrap() = device_id;
        }
    }

    /// What `bus` does while its output device is unavailable (unplugged,
    /// turned off, taken over by another app): play on the default device
    /// until it's back (`true`, the default), or go silent (`false`). Other
    /// buses keep playing either way.
    pub fn set_bus_fallback(&self, bus: &str, fallback_to_default: bool) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            bus.fallback_to_default
                .store(fallback_to_default, Ordering::Relaxed);
        }
    }

    pub fn set_master_gain(&self, gain: f32) {
        self.master_gain.store(gain.max(0.0));
    }

    pub fn master_gain(&self) -> f32 {
        self.master_gain.load()
    }

    /// Peak of the final mix since the previous call.
    pub fn master_level(&self) -> f32 {
        self.master_level.take()
    }

    /// Every source, with its peak since the previous call (for meters:
    /// read once per frame).
    pub fn sources(&self) -> Vec<SourceInfo> {
        self.source_infos(true)
    }

    /// Every source's settings, leaving its peak for [`Self::sources`] (for
    /// saving, and anything else that isn't a meter).
    pub fn source_settings(&self) -> Vec<SourceInfo> {
        self.source_infos(false)
    }

    fn source_infos(&self, take_levels: bool) -> Vec<SourceInfo> {
        self.sources
            .lock()
            .unwrap()
            .iter()
            .map(|(&id, s)| SourceInfo {
                id,
                pid: s.pid,
                program: s.program.clone(),
                name: s.name.clone(),
                bus: s.bus.lock().unwrap().clone(),
                gain: s.gain.load(),
                muted: s.muted.load(Ordering::Relaxed),
                level: if take_levels { s.level.take() } else { s.level.load() },
                input_device: s.input_device.clone(),
                direct: s.direct.load(Ordering::Relaxed),
                exited: s.exited.load(Ordering::Relaxed),
            })
            .collect()
    }

    /// Every bus, with its peak since the previous call (for meters: read
    /// once per frame).
    pub fn buses(&self) -> Vec<BusInfo> {
        self.bus_infos(true)
    }

    /// Every bus's settings, leaving its peak for [`Self::buses`].
    pub fn bus_settings(&self) -> Vec<BusInfo> {
        self.bus_infos(false)
    }

    fn bus_infos(&self, take_levels: bool) -> Vec<BusInfo> {
        self.buses
            .lock()
            .unwrap()
            .iter()
            .map(|(name, b)| {
                // Locked once: a second lock in the same expression would
                // wait on the first, which lives until the expression ends.
                let (eq, eq_enabled) = {
                    let eq = b.eq.lock().unwrap();
                    (eq.bands(), eq.enabled())
                };
                BusInfo {
                    name: name.clone(),
                    gain: b.gain.load(),
                    muted: b.muted.load(Ordering::Relaxed),
                    level: if take_levels { b.level.take() } else { b.level.load() },
                    eq,
                    eq_enabled,
                    device_id: b.device_id.lock().unwrap().clone(),
                    fallback_to_default: b.fallback_to_default.load(Ordering::Relaxed),
                    heard: b.heard.load(Ordering::Relaxed),
                    priority: *b.priority.lock().unwrap(),
                    delay_ms: b.delay_ms.load(),
                    send: b.send.load(Ordering::Relaxed),
                    monitor: b.monitor.load(Ordering::Relaxed),
                    direct: b.direct.load(Ordering::Relaxed),
                    surround: b.surround.lock().unwrap().settings(),
                }
            })
            .collect()
    }

    /// Creates `name`'s bus state (flat EQ, unity gain, default device) if
    /// it doesn't exist yet. Safe to call up front for buses that should
    /// always be shown (e.g. a fixed set of channels in the UI) even before
    /// anything is routed to them.
    pub fn ensure_bus(&self, name: &str) {
        let mut buses = self.buses.lock().unwrap();
        if buses.contains_key(name) {
            return;
        }
        buses.insert(
            name.to_string(),
            Arc::new(BusInner {
                gain: AtomicF32::new(1.0),
                muted: AtomicBool::new(false),
                level: AtomicF32::new(0.0),
                eq: Mutex::new(EqBands::new(SAMPLE_RATE)),
                surround: Mutex::new(VirtualSurround::new(Surround::default())),
                device_id: Mutex::new(None),
                fallback_to_default: AtomicBool::new(true),
                heard: AtomicBool::new(true),
                priority: Mutex::new(Priority::Normal),
                delay_ms: AtomicF32::new(0.0),
                send: AtomicBool::new(false),
                monitor: AtomicBool::new(false),
                direct: AtomicBool::new(false),
            }),
        );
    }
}

/// Puts microphone `mic` into `sources` as `id`, taking out any other
/// microphone in the same step (so the mix never has both), and returns
/// those for the caller to stop.
fn swap_in_mic(sources: &mut HashMap<u64, Arc<SourceInner>>, id: u64, mic: Arc<SourceInner>) -> Vec<Arc<SourceInner>> {
    let old_ids: Vec<u64> = sources.iter().filter(|(_, s)| s.pid.is_none()).map(|(&id, _)| id).collect();
    let replaced = old_ids.iter().filter_map(|old_id| sources.remove(old_id)).collect();
    sources.insert(id, mic);
    replaced
}

impl Drop for Mixer {
    fn drop(&mut self) {
        self.render_stop.store(true, Ordering::Relaxed);
        // Closing the mixer must never leave an app turned down (or moved).
        let sources = self.sources.lock().unwrap();
        for source in sources.values() {
            source.stop_and_restore();
        }
        for source in sources.values() {
            source.wait_restored();
        }
    }
}

/// One bus as seen by a single render tick.
struct BusSnapshot {
    name: String,
    bus: Arc<BusInner>,
    members: Vec<Arc<SourceInner>>,
    /// For each member, whether it plays straight on the device itself
    /// (see [`SourceInner::direct`]).
    members_direct: Vec<bool>,
    device_id: Option<String>,
    /// Whether the bus's own device opened (rather than its fallback).
    device_ok: bool,
    priority: Priority,
    send: bool,
    monitor: bool,
}

/// Works out how each app should be routed (see [`RoutePlan`]) for its
/// capture thread to carry out. Its bus can play direct when nothing needs
/// the mixer: a flat EQ, surround off, a bus you hear on its own device (not
/// a send, and not falling back to the default because its device is gone).
/// An app turned above 100% overall still needs the mixer to boost it. Where
/// the mixer can't capture apps (`can_process` false: Windows 10), direct is
/// all there is: the EQ, surround and boosts don't reach them.
fn update_route_plans(bus_snapshot: &[BusSnapshot], master_gain: f32, can_process: bool) {
    for snapshot in bus_snapshot {
        let bus = &snapshot.bus;
        let flat = bus.eq.lock().unwrap().is_flat();
        let surround = bus.surround.lock().unwrap().settings().enabled;
        let bus_direct = snapshot.device_ok
            && ((flat && !surround) || !can_process)
            && !snapshot.send
            && bus.heard.load(Ordering::Relaxed);
        bus.direct.store(bus_direct, Ordering::Relaxed);
        let bus_volume = if bus.muted.load(Ordering::Relaxed) { 0.0 } else { bus.gain.load() * master_gain };
        for source in snapshot.members.iter().filter(|source| source.pid.is_some()) {
            let volume = if source.muted.load(Ordering::Relaxed) { 0.0 } else { source.gain.load() * bus_volume };
            let plan = RoutePlan {
                direct: bus_direct && (volume <= 1.0 + 1e-3 || !can_process),
                device: snapshot.device_id.clone(),
                volume: volume.min(1.0),
            };
            let mut current = source.plan.lock().unwrap();
            if *current != plan {
                *current = plan;
            }
        }
    }
}

/// Turns a direct app's capture, taken at its own volume (its fader times
/// its channel's and the master), back into what the mixer would have had
/// after its fader: the level meters and clips use.
fn direct_correction(bus_volume: f32) -> f32 {
    if bus_volume > 1e-3 {
        1.0 / bus_volume
    } else {
        0.0
    }
}

/// How long to wait before trying an output device again after it failed
/// (unplugged, turned off, taken over by an exclusive-mode app). Its buses
/// play on the default device meanwhile.
const DEVICE_RETRY_INTERVAL: Duration = Duration::from_secs(2);
/// The same for the default device, retried sooner because a failure
/// there usually means Windows just switched the default to another device.
const DEFAULT_DEVICE_RETRY_INTERVAL: Duration = Duration::from_millis(200);

fn device_retry_interval(device_id: &Option<String>) -> Duration {
    if device_id.is_some() {
        DEVICE_RETRY_INTERVAL
    } else {
        DEFAULT_DEVICE_RETRY_INTERVAL
    }
}

/// Runs entirely on its own OS thread. Unlike capture threads (one per
/// source), there is one render thread total: it lazily opens/closes an
/// output per *distinct device a bus currently targets*,
/// re-checked every tick, so buses can be freely re-routed between output
/// devices (including the system default, `None`) while running.
fn run_render_loop(
    sources: Arc<Mutex<HashMap<u64, Arc<SourceInner>>>>,
    buses: Arc<Mutex<HashMap<String, Arc<BusInner>>>>,
    master_gain: Arc<AtomicF32>,
    master_level: Arc<AtomicF32>,
    stop: Arc<AtomicBool>,
    bus_tap: SharedTap,
) -> Result<()> {
    let _thread = platform::audio_thread()?;
    let mut device_outputs: HashMap<Option<String>, DeviceOutput> = HashMap::new();
    // Devices that failed, with when to try each again.
    let mut failed_devices: HashMap<Option<String>, Instant> = HashMap::new();
    // Monitored buses' audio on its way to the default device.
    let mut monitor_queue: VecDeque<f32> = VecDeque::new();
    // Reused across ticks so tapping and rendering don't allocate on the
    // render thread.
    let mut tap_buffers: Vec<Vec<f32>> = Vec::new();
    let mut rendered: Vec<f32> = Vec::new();
    // Each source's audio for the block being rendered, gain applied.
    let mut blocks: Vec<Vec<f32>> = Vec::new();

    while !stop.load(Ordering::Relaxed) {
        // Snapshot once per render tick (not per sample), grouped by bus so
        // each bus's EQ runs once on that bus's own summed signal.
        let mut bus_snapshot: Vec<BusSnapshot> = {
            let sources = sources.lock().unwrap();
            let buses = buses.lock().unwrap();
            buses
                .iter()
                .map(|(name, bus)| {
                    let device_id = bus.device_id.lock().unwrap().clone();
                    let priority = *bus.priority.lock().unwrap();
                    let send = bus.send.load(Ordering::Relaxed);
                    let monitor = bus.monitor.load(Ordering::Relaxed);
                    let members: Vec<Arc<SourceInner>> = sources
                        .values()
                        .filter(|s| *s.bus.lock().unwrap() == *name)
                        .cloned()
                        .collect();
                    let members_direct = members.iter().map(|s| s.direct.load(Ordering::Relaxed)).collect();
                    BusSnapshot {
                        name: name.clone(),
                        bus: bus.clone(),
                        members,
                        members_direct,
                        device_id,
                        device_ok: false,
                        priority,
                        send,
                        monitor,
                    }
                })
                .collect()
        };

        // Open the devices buses ask for; a bus whose device can't be opened
        // plays on the default device until it can, or goes silent if it's
        // set to. Then close devices no bus plays on anymore.
        let now = Instant::now();
        let mut ensure_open = |device_id: &Option<String>| {
            if device_outputs.contains_key(device_id) {
                return true;
            }
            if failed_devices
                .get(device_id)
                .is_some_and(|&retry_at| now < retry_at)
            {
                return false;
            }
            match DeviceOutput::open(device_id.as_deref()) {
                Ok(output) => {
                    failed_devices.remove(device_id);
                    device_outputs.insert(device_id.clone(), output);
                    true
                }
                Err(_) => {
                    failed_devices
                        .insert(device_id.clone(), now + device_retry_interval(device_id));
                    false
                }
            }
        };
        for snapshot in &mut bus_snapshot {
            snapshot.device_ok = ensure_open(&snapshot.device_id);
            if !snapshot.device_ok
                && !snapshot.send
                && snapshot.bus.fallback_to_default.load(Ordering::Relaxed)
            {
                snapshot.device_id = None;
                ensure_open(&None);
            }
        }
        let master_gain_value = master_gain.load();
        update_route_plans(&bus_snapshot, master_gain_value, platform::per_app_capture_supported());
        let monitoring = bus_snapshot.iter().any(|s| s.monitor && s.device_id.is_some());
        if monitoring {
            ensure_open(&None);
        } else {
            monitor_queue.clear();
        }
        device_outputs.retain(|device_id, _| {
            bus_snapshot.iter().any(|s| s.device_id == *device_id) || (device_id.is_none() && monitoring)
        });
        // A monitor copy plays on the default device. When that's the very
        // device the bus plays on (Windows' default output set to the
        // virtual cable), the copy would land there too, a few ms after the
        // first: a doubled voice. So it's skipped.
        if let Some(default) = device_outputs.get(&None) {
            for snapshot in &mut bus_snapshot {
                if snapshot.device_id.is_some() && snapshot.device_id.as_deref() == default.endpoint_id() {
                    snapshot.monitor = false;
                }
            }
        }
        // Buses with nowhere to play drop their audio rather than queue it
        // up to play late once a device is back; the rest drop any backlog
        // beyond what their priority allows.
        // Only apps the mixer plays count towards a bus's delay; direct ones
        // play on the device themselves.
        let mut queued_ms: Vec<Option<f32>> = vec![None; bus_snapshot.len()];
        for (bus_idx, snapshot) in bus_snapshot.iter().enumerate() {
            let playing = device_outputs.contains_key(&snapshot.device_id);
            let limit = ms_to_frames(queue_limit_ms(snapshot.priority)) as usize * CHANNELS;
            let keep = ms_to_frames(device_target_ms(snapshot.priority)) as usize * CHANNELS;
            for (source, &direct) in snapshot.members.iter().zip(&snapshot.members_direct) {
                let mut samples = source.samples.lock().unwrap();
                if !playing {
                    samples.clear();
                } else if samples.len() > limit {
                    let excess = samples.len() - keep;
                    samples.drain(..excess - excess % CHANNELS);
                }
                if !direct {
                    let ms = samples.len() as f32 / CHANNELS as f32 / SAMPLE_RATE * 1000.0;
                    queued_ms[bus_idx] = Some(queued_ms[bus_idx].unwrap_or(0.0).max(ms));
                }
            }
        }
        // Each device is kept only as full as its most urgent bus allows.
        let device_targets: HashMap<Option<String>, u32> = device_outputs
            .keys()
            .map(|device_id| {
                // The default device also carries monitored buses.
                let target = bus_snapshot
                    .iter()
                    .filter(|s| &s.device_id == device_id || (device_id.is_none() && s.monitor))
                    .map(|s| device_target_ms(s.priority))
                    .fold(f32::MAX, f32::min);
                (device_id.clone(), ms_to_frames(target))
            })
            .collect();
        let mut device_queued_ms: HashMap<Option<String>, f32> = HashMap::new();

        let mut master_peak = 0.0f32;
        let mut source_peaks: Vec<Vec<f32>> = bus_snapshot
            .iter()
            .map(|snapshot| vec![0.0f32; snapshot.members.len()])
            .collect();
        let mut bus_peaks: Vec<f32> = vec![0.0f32; bus_snapshot.len()];
        let tap = bus_tap.lock().unwrap().clone();
        if tap.is_some() {
            tap_buffers.resize_with(bus_snapshot.len(), Vec::new);
            tap_buffers.iter_mut().for_each(Vec::clear);
        }
        let mut rendered_any = false;
        let mut broken_devices = Vec::new();
        // Where each bus's sources start in `blocks`.
        let block_starts: Vec<usize> = bus_snapshot
            .iter()
            .scan(0, |next, snapshot| {
                let start = *next;
                *next += snapshot.members.len();
                Some(start)
            })
            .collect();
        blocks.resize_with(bus_snapshot.iter().map(|s| s.members.len()).sum(), Vec::new);

        for (device_id, output) in device_outputs.iter_mut() {
            let Ok(padding) = output.queued_frames() else {
                broken_devices.push(device_id.clone());
                continue;
            };
            let capacity = output.capacity_frames();
            let target = device_targets.get(device_id).copied().unwrap_or(capacity);
            device_queued_ms.insert(device_id.clone(), padding as f32 / SAMPLE_RATE * 1000.0);
            let available = capacity.saturating_sub(padding).min(target.saturating_sub(padding));
            if available == 0 {
                continue;
            }
            rendered_any = true;
            rendered.clear();

            // Take each source's audio for the whole block at once: one lock
            // per source per block rather than per sample, so the capture
            // threads filling the queues are rarely made to wait. A source
            // that runs short is silent for the rest of the block; a muted
            // one's audio is dropped.
            let wanted = available as usize * CHANNELS;
            for (bus_idx, snapshot) in bus_snapshot.iter().enumerate() {
                if &snapshot.device_id != device_id {
                    continue;
                }
                let bus_volume = snapshot.bus.gain.load() * master_gain_value;
                for (src_idx, source) in snapshot.members.iter().enumerate() {
                    let block = &mut blocks[block_starts[bus_idx] + src_idx];
                    block.clear();
                    let mut buffer = source.samples.lock().unwrap();
                    let take = wanted.min(buffer.len() - buffer.len() % CHANNELS);
                    if snapshot.members_direct[src_idx] {
                        // Its own volume (and mute) is already in it.
                        let correction = direct_correction(bus_volume);
                        block.extend(buffer.drain(..take).map(|sample| sample * correction));
                    } else if source.muted.load(Ordering::Relaxed) {
                        buffer.drain(..take);
                    } else {
                        let gain = source.gain.load();
                        block.extend(buffer.drain(..take).map(|sample| sample * gain));
                    }
                }
            }
            // Each bus's EQ, held for the block.
            let mut eqs: Vec<_> = bus_snapshot
                .iter()
                .map(|snapshot| (&snapshot.device_id == device_id).then(|| snapshot.bus.eq.lock().unwrap()))
                .collect();
            // And its surround, which is for what you hear: never on a send
            // bus (a voice going to other apps).
            let mut surrounds: Vec<_> = bus_snapshot
                .iter()
                .map(|snapshot| {
                    (&snapshot.device_id == device_id && !snapshot.send).then(|| snapshot.bus.surround.lock().unwrap())
                })
                .collect();

            for frame in 0..available as usize {
                let mut device_mix = [0.0f32; CHANNELS];
                // Send buses aren't changed by the master volume.
                let mut send_mix = [0.0f32; CHANNELS];
                if device_id.is_none() {
                    for m in device_mix.iter_mut() {
                        *m += monitor_queue.pop_front().unwrap_or(0.0);
                    }
                }

                for (
                    bus_idx,
                    BusSnapshot {
                        bus,
                        members,
                        members_direct,
                        device_id: bus_device,
                        ..
                    },
                ) in bus_snapshot.iter().enumerate()
                {
                    if bus_device != device_id {
                        continue;
                    }
                    // What the mixer plays, and what direct apps play on the
                    // device themselves (only metered and recorded here).
                    let mut played_mix = [0.0f32; CHANNELS];
                    let mut direct_mix = [0.0f32; CHANNELS];
                    for src_idx in 0..members.len() {
                        let block = &blocks[block_starts[bus_idx] + src_idx];
                        let Some(samples) = block.get(frame * CHANNELS..(frame + 1) * CHANNELS) else {
                            continue;
                        };
                        let mix = if members_direct[src_idx] { &mut direct_mix } else { &mut played_mix };
                        let mut local_peak = 0.0f32;
                        for (channel_mix, &sample) in mix.iter_mut().zip(samples) {
                            *channel_mix += sample;
                            local_peak = local_peak.max(sample.abs());
                        }
                        source_peaks[bus_idx][src_idx] =
                            source_peaks[bus_idx][src_idx].max(local_peak);
                    }

                    if let Some(eq) = eqs[bus_idx].as_mut() {
                        eq.process(&mut played_mix);
                    }
                    // Direct apps only play while the EQ is flat, so the sum
                    // is the bus as heard.
                    let bus_mix: [f32; CHANNELS] = std::array::from_fn(|c| played_mix[c] + direct_mix[c]);
                    if tap.is_some() {
                        tap_buffers[bus_idx].extend_from_slice(&bus_mix);
                    }
                    let bus_frame_peak = bus_mix.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
                    bus_peaks[bus_idx] = bus_peaks[bus_idx].max(bus_frame_peak);
                    // What you hear can be surround; meters and clips keep the
                    // bus's own sound. Apps on a bus with surround on don't
                    // play direct, so the mixer plays all of it.
                    if let Some(surround) = surrounds[bus_idx].as_mut() {
                        if !bus.muted.load(Ordering::Relaxed) && bus.heard.load(Ordering::Relaxed) {
                            surround.process(&mut played_mix);
                        } else {
                            surround.skip();
                        }
                    }

                    if !bus.muted.load(Ordering::Relaxed) {
                        let bus_gain = bus.gain.load();
                        if bus.heard.load(Ordering::Relaxed) {
                            let mix = if bus_snapshot[bus_idx].send { &mut send_mix } else { &mut device_mix };
                            for (m, b) in mix.iter_mut().zip(played_mix.iter()) {
                                *m += b * bus_gain;
                            }
                        }
                        if bus_snapshot[bus_idx].monitor && bus_device.is_some() {
                            monitor_queue.extend(played_mix.iter().map(|b| b * bus_gain));
                        }
                    }
                }

                for (channel, m) in device_mix.iter().enumerate() {
                    let sample = (m * master_gain_value + send_mix[channel]).clamp(-1.0, 1.0);
                    master_peak = master_peak.max(sample.abs());
                    rendered.push(sample);
                }
            }

            drop(eqs);
            drop(surrounds);
            if output.write(&rendered).is_err() {
                broken_devices.push(device_id.clone());
            }
        }
        // A device that fails mid-stream is gone (or taken over): close it,
        // and its buses fall back to the default device from the next tick.
        for device_id in broken_devices {
            device_outputs.remove(&device_id);
            failed_devices.insert(
                device_id.clone(),
                Instant::now() + device_retry_interval(&device_id),
            );
        }

        if let Some(tap) = &tap {
            for (snapshot, samples) in bus_snapshot.iter().zip(&tap_buffers) {
                if !samples.is_empty() {
                    tap.bus_audio(&snapshot.name, samples);
                }
            }
        }

        // A monitor copy that piled up (the default device stalled) is
        // dropped rather than heard late.
        let monitor_limit = ms_to_frames(queue_limit_ms(Priority::High)) as usize * CHANNELS;
        if monitor_queue.len() > monitor_limit {
            let excess = monitor_queue.len() - monitor_limit / 2;
            monitor_queue.drain(..excess - excess % CHANNELS);
        }

        for (bus_idx, snapshot) in bus_snapshot.iter().enumerate() {
            let device_ms = device_queued_ms.get(&snapshot.device_id).copied().unwrap_or(0.0);
            // A bus whose apps all play direct adds nothing.
            let delay = queued_ms[bus_idx].map_or(0.0, |queued| device_ms + queued);
            snapshot.bus.delay_ms.store(delay);
        }

        if !rendered_any {
            // Short, since a High-priority device holds only ~20 ms.
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        // Levels accumulate until the UI takes them, so it sees every
        // block's peak however often it looks.
        let captured = platform::per_app_capture_supported();
        for (bus_idx, BusSnapshot { bus, members, .. }) in bus_snapshot.iter().enumerate() {
            bus.level.raise_to(bus_peaks[bus_idx]);
            for (src_idx, source) in members.iter().enumerate() {
                source.level.raise_to(source_peaks[bus_idx][src_idx]);
                // Apps the mixer can't capture are metered by Windows; their
                // bus shows the loudest.
                if !captured && source.pid.is_some() {
                    bus.level.raise_to(source.level.load());
                }
            }
        }
        master_level.raise_to(master_peak);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(pid: Option<u32>, bus: &str) -> Arc<SourceInner> {
        Arc::new(SourceInner::new(pid, "test".into(), bus.into(), None, 1.0, false))
    }

    #[test]
    fn starting_a_microphone_replaces_the_one_running() {
        let mut sources = HashMap::new();
        let game = source(Some(100), "Game");
        let old_mic = source(None, "Mic");
        sources.insert(0, game.clone());
        sources.insert(1, old_mic.clone());

        let new_mic = source(None, "Mic");
        let replaced = swap_in_mic(&mut sources, 2, new_mic.clone());

        assert_eq!(replaced.len(), 1);
        assert!(Arc::ptr_eq(&replaced[0], &old_mic));
        let mics: Vec<u64> = sources.iter().filter(|(_, s)| s.pid.is_none()).map(|(&id, _)| id).collect();
        assert_eq!(mics, [2], "one microphone, the new one");
        assert!(Arc::ptr_eq(&sources[&0], &game), "apps stay");
    }

    /// The third EQ band, raised by `gain_db`.
    fn raised(gain_db: f32) -> EqBand {
        EqBand { gain_db, ..crate::eq::DEFAULT_EQ_BANDS[2] }
    }

    fn bus() -> Arc<BusInner> {
        Arc::new(BusInner {
            gain: AtomicF32::new(1.0),
            muted: AtomicBool::new(false),
            level: AtomicF32::new(0.0),
            eq: Mutex::new(EqBands::new(SAMPLE_RATE)),
            surround: Mutex::new(VirtualSurround::new(Surround::default())),
            device_id: Mutex::new(Some("headset".into())),
            fallback_to_default: AtomicBool::new(true),
            heard: AtomicBool::new(true),
            priority: Mutex::new(Priority::High),
            delay_ms: AtomicF32::new(0.0),
            send: AtomicBool::new(false),
            monitor: AtomicBool::new(false),
            direct: AtomicBool::new(false),
        })
    }

    /// The plan for one app on `bus` after a tick (`device_ok`: whether the
    /// bus's own device opened), where apps can be captured.
    fn plan_for(bus: &Arc<BusInner>, app: &Arc<SourceInner>, device_ok: bool, master: f32) -> RoutePlan {
        plan_where(bus, app, device_ok, master, true)
    }

    fn plan_where(bus: &Arc<BusInner>, app: &Arc<SourceInner>, device_ok: bool, master: f32, can_process: bool) -> RoutePlan {
        let snapshot = BusSnapshot {
            name: "Game".into(),
            bus: bus.clone(),
            members: vec![app.clone()],
            members_direct: vec![false],
            device_id: bus.device_id.lock().unwrap().clone(),
            device_ok,
            priority: Priority::High,
            send: bus.send.load(Ordering::Relaxed),
            monitor: false,
        };
        update_route_plans(&[snapshot], master, can_process);
        app.plan.lock().unwrap().clone()
    }

    #[test]
    fn apps_play_direct_only_when_nothing_needs_the_mixer() {
        let (game, app) = (bus(), source(Some(100), "Game"));
        game.gain.store(0.5);
        app.gain.store(0.8);
        let plan = plan_for(&game, &app, true, 1.0);
        assert_eq!(plan, RoutePlan { direct: true, device: Some("headset".into()), volume: 0.4 });
        assert!(game.direct.load(Ordering::Relaxed));

        // Muted, it still plays direct, at no volume.
        game.muted.store(true, Ordering::Relaxed);
        assert_eq!(plan_for(&game, &app, true, 1.0), RoutePlan { direct: true, device: Some("headset".into()), volume: 0.0 });
        game.muted.store(false, Ordering::Relaxed);

        // Above 100% overall, only the mixer can boost it.
        app.gain.store(2.0);
        let plan = plan_for(&game, &app, true, 1.5);
        assert!(!plan.direct);
        assert_eq!(plan.volume, 1.0);
        app.gain.store(0.8);

        // The EQ needs the mixer.
        game.eq.lock().unwrap().set_band(2, raised(3.0));
        assert!(!plan_for(&game, &app, true, 1.0).direct);
        game.eq.lock().unwrap().set_band(2, raised(0.0));

        // So does surround.
        game.surround.lock().unwrap().set(Surround { enabled: true, ..Surround::default() });
        assert!(!plan_for(&game, &app, true, 1.0).direct);
        assert!(!game.direct.load(Ordering::Relaxed));
        game.surround.lock().unwrap().set(Surround::default());
        assert!(plan_for(&game, &app, true, 1.0).direct);

        // So does a bus whose own device is gone (it's on its fallback).
        assert!(!plan_for(&game, &app, false, 1.0).direct);
    }

    #[test]
    fn without_app_capture_apps_always_play_direct() {
        let (game, app) = (bus(), source(Some(100), "Game"));
        game.eq.lock().unwrap().set_band(2, raised(3.0));
        app.gain.store(2.0);
        // The EQ and the boost can't reach it, but its volume is capped.
        let plan = plan_where(&game, &app, true, 1.0, false);
        assert_eq!(plan, RoutePlan { direct: true, device: Some("headset".into()), volume: 1.0 });
        // A bus without its device still can't play it.
        assert!(!plan_where(&game, &app, false, 1.0, false).direct);
    }

    #[test]
    fn a_microphone_has_no_route_plan() {
        let (mic_bus, mic) = (bus(), source(None, "Mic"));
        mic_bus.send.store(true, Ordering::Relaxed);
        assert_eq!(plan_for(&mic_bus, &mic, true, 1.0), RoutePlan::default());
        assert!(!mic_bus.direct.load(Ordering::Relaxed), "a send bus never plays direct");
    }

    #[test]
    fn direct_audio_is_metered_as_after_its_own_fader() {
        assert_eq!(direct_correction(0.5), 2.0);
        assert_eq!(direct_correction(0.0), 0.0, "a silenced bus captures nothing to undo");
    }

    #[test]
    fn the_first_microphone_replaces_nothing() {
        let mut sources = HashMap::new();
        sources.insert(0, source(Some(100), "Game"));
        assert!(swap_in_mic(&mut sources, 1, source(None, "Mic")).is_empty());
        assert_eq!(sources.len(), 2);
    }
}
