use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{Error, Result};
use windows::Win32::Foundation::{CloseHandle, E_INVALIDARG, E_UNEXPECTED, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, IAudioRenderClient, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use common::Priority;

use crate::atomic_f32::AtomicF32;
use crate::com::{ComGuard, ProAudioThread};
use crate::eq::{EqBands, EQ_BAND_COUNT};
use crate::loopback::{activate_process_loopback_client, standard_format};
use crate::denoise::MicCleanup;
use crate::mic::run_mic_capture_thread;
use crate::process_tree::ProcessTree;
use crate::render::open_render_client_for_device;
use crate::session_volume::{self, OwnedSession, ROUTED_BOOST, ROUTED_VOLUME};

const CAPTURE_BUFFER_DURATION_100NS: i64 = 2_000_000;
const CHANNELS: usize = 2;
const SAMPLE_RATE: f32 = 48_000.0;
/// How full each output device is kept, by the most urgent bus playing on
/// it: everything queued there is delay. The audio engine itself needs a
/// little over 10 ms.
fn device_target_ms(priority: Priority) -> f32 {
    match priority {
        Priority::High => 15.0,
        Priority::Normal => 50.0,
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
/// How often a routed app's known sessions are checked and turned down
/// again if something (the app, the Windows mixer) turned them back up.
/// Cheap: reads each known session's volume.
const ROUTE_ENFORCE_INTERVAL: Duration = Duration::from_millis(100);
/// How often an app's audio processes and sessions are looked up again,
/// since apps create sessions and helper processes as they go (see
/// [`crate::process_tree`]). Costly: walks every session on every device
/// and the process list (~17 ms measured), so it runs far less often
/// than enforcement.
const APP_RESCAN_INTERVAL: Duration = Duration::from_secs(3);
/// How soon after the last look-up to look again when the captured audio
/// shows a session that isn't turned down yet (e.g. a new browser tab).
const APP_RESCAN_SOON: Duration = Duration::from_millis(500);
/// A turned-down app can't produce samples above `ROUTED_VOLUME` times its
/// own peak; apps with hot float output can exceed full scale a little, so
/// allow this much before treating a packet as not turned down.
const ROUTED_HEADROOM: f32 = 2.0;
/// After capturing audio that wasn't turned down despite every session
/// reading back as turned down (audio we can't reach, e.g. a child process),
/// don't boost again for this long.
const BOOST_BLOCK: Duration = Duration::from_secs(2);

pub(crate) struct SourceInner {
    /// `None` for the microphone source, which has no owning process.
    pid: Option<u32>,
    name: String,
    bus: Mutex<String>,
    gain: AtomicF32,
    muted: AtomicBool,
    /// Peak since the UI last looked, measured by the render thread.
    level: AtomicF32,
    /// The capture device, for the microphone; `None` is the system default.
    input_device: Option<String>,
    pub(crate) samples: Mutex<VecDeque<f32>>,
    pub(crate) stop: Arc<AtomicBool>,
    /// The app's audio processes currently turned down. Held while turning
    /// them down or restoring them, so removal can't race with the capture
    /// thread turning them down again.
    routed: Mutex<HashSet<u32>>,
}

impl SourceInner {
    /// Stops capturing and, for an app, gives it back its own volume.
    fn stop_and_restore(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if self.pid.is_some() {
            let mut routed = self.routed.lock().unwrap();
            session_volume::unroute(&routed);
            routed.clear();
        }
    }
}

struct BusInner {
    gain: AtomicF32,
    muted: AtomicBool,
    /// Peak since the UI last looked.
    level: AtomicF32,
    eq: Mutex<EqBands>,
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
    pub name: String,
    pub bus: String,
    pub gain: f32,
    pub muted: bool,
    /// Peak since the previous snapshot.
    pub level: f32,
    /// The microphone's capture device; `None` is the system default (and
    /// always `None` for apps).
    pub input_device: Option<String>,
}

/// A snapshot of one bus's current state for UI display.
pub struct BusInfo {
    pub name: String,
    pub gain: f32,
    pub muted: bool,
    /// Peak since the previous snapshot.
    pub level: f32,
    pub eq_gains_db: [f32; EQ_BAND_COUNT],
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
}

impl Mixer {
    pub fn start() -> Result<Self> {
        // Before anything is routed: give back any app a previous run left
        // turned down.
        session_volume::recover(&[]);
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
            return Err(Error::from(E_INVALIDARG));
        }
        let bus_name = bus.into();
        self.ensure_bus(&bus_name);

        let inner = Arc::new(SourceInner {
            pid: Some(pid),
            name: name.into(),
            bus: Mutex::new(bus_name),
            gain: AtomicF32::new(1.0),
            muted: AtomicBool::new(false),
            level: AtomicF32::new(0.0),
            input_device: None,
            samples: Mutex::new(VecDeque::new()),
            stop: Arc::new(AtomicBool::new(false)),
            routed: Mutex::default(),
        });

        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_source = inner.clone();
        std::thread::Builder::new()
            .name(format!("capture-{pid}"))
            .spawn(move || run_capture_thread(pid, thread_source, ready_tx))
            .expect("spawn capture thread");

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| Error::from(E_UNEXPECTED))??;

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
    pub fn add_mic_source(
        &self,
        name: impl Into<String>,
        bus: impl Into<String>,
        device_id: Option<String>,
    ) -> Result<u64> {
        let bus_name = bus.into();
        self.ensure_bus(&bus_name);

        let inner = Arc::new(SourceInner {
            pid: None,
            name: name.into(),
            bus: Mutex::new(bus_name),
            gain: AtomicF32::new(1.0),
            muted: AtomicBool::new(false),
            level: AtomicF32::new(0.0),
            input_device: device_id.clone(),
            samples: Mutex::new(VecDeque::new()),
            stop: Arc::new(AtomicBool::new(false)),
            routed: Mutex::default(),
        });

        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_source = inner.clone();
        let cleanup = self.mic_cleanup.clone();
        std::thread::Builder::new()
            .name("capture-mic".into())
            .spawn(move || run_mic_capture_thread(thread_source, device_id, cleanup, ready_tx))
            .expect("spawn mic capture thread");

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| Error::from(E_UNEXPECTED))??;

        let id = self.next_id();
        self.sources.lock().unwrap().insert(id, inner);
        Ok(id)
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
            return Err(Error::from(E_INVALIDARG));
        };
        let bus = old.bus.lock().unwrap().clone();
        let new_id = self.add_mic_source(old.name.clone(), bus, device_id)?;
        self.set_source_gain(new_id, old.gain.load());
        self.set_source_muted(new_id, old.muted.load(Ordering::Relaxed));
        self.remove_source(id);
        Ok(new_id)
    }

    /// Stops capturing `id`; its thread exits and cleans up its WASAPI
    /// client within one poll interval (<=100ms). If it was an app, gives
    /// the app back its own volume.
    pub fn remove_source(&self, id: u64) {
        let removed = self.sources.lock().unwrap().remove(&id);
        if let Some(source) = removed {
            source.stop_and_restore();
        }
    }

    /// Restores apps a previous run left turned down because it didn't exit
    /// cleanly (crash, killed). Cheap when there's nothing to restore.
    pub fn recover_routed_volumes(&self) {
        if !session_volume::has_pending_routes() {
            return;
        }
        let routed: Vec<u32> = self
            .sources
            .lock()
            .unwrap()
            .values()
            .flat_map(|s| s.routed.lock().unwrap().iter().copied().collect::<Vec<_>>())
            .collect();
        session_volume::recover(&routed);
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

    /// Sets one band (0..[`EQ_BAND_COUNT`]) of `bus`'s EQ, in dB.
    pub fn set_bus_eq_band(&self, bus: &str, band: usize, gain_db: f32) {
        if let Some(bus) = self.buses.lock().unwrap().get(bus) {
            bus.eq.lock().unwrap().set_band(band, gain_db);
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

    pub fn sources(&self) -> Vec<SourceInfo> {
        self.sources
            .lock()
            .unwrap()
            .iter()
            .map(|(&id, s)| SourceInfo {
                id,
                pid: s.pid,
                name: s.name.clone(),
                bus: s.bus.lock().unwrap().clone(),
                gain: s.gain.load(),
                muted: s.muted.load(Ordering::Relaxed),
                level: s.level.take(),
                input_device: s.input_device.clone(),
            })
            .collect()
    }

    pub fn buses(&self) -> Vec<BusInfo> {
        self.buses
            .lock()
            .unwrap()
            .iter()
            .map(|(name, b)| BusInfo {
                name: name.clone(),
                gain: b.gain.load(),
                muted: b.muted.load(Ordering::Relaxed),
                level: b.level.take(),
                eq_gains_db: b.eq.lock().unwrap().gains(),
                device_id: b.device_id.lock().unwrap().clone(),
                fallback_to_default: b.fallback_to_default.load(Ordering::Relaxed),
                heard: b.heard.load(Ordering::Relaxed),
                priority: *b.priority.lock().unwrap(),
                delay_ms: b.delay_ms.load(),
                send: b.send.load(Ordering::Relaxed),
                monitor: b.monitor.load(Ordering::Relaxed),
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
        let gains_db = [0.0; EQ_BAND_COUNT];
        buses.insert(
            name.to_string(),
            Arc::new(BusInner {
                gain: AtomicF32::new(1.0),
                muted: AtomicBool::new(false),
                level: AtomicF32::new(0.0),
                eq: Mutex::new(EqBands::from_gains(SAMPLE_RATE, gains_db)),
                device_id: Mutex::new(None),
                fallback_to_default: AtomicBool::new(true),
                heard: AtomicBool::new(true),
                priority: Mutex::new(Priority::Normal),
                delay_ms: AtomicF32::new(0.0),
                send: AtomicBool::new(false),
                monitor: AtomicBool::new(false),
            }),
        );
    }
}

impl Drop for Mixer {
    fn drop(&mut self) {
        self.render_stop.store(true, Ordering::Relaxed);
        // Closing the mixer must never leave an app turned down.
        for source in self.sources.lock().unwrap().values() {
            source.stop_and_restore();
        }
    }
}

/// A started process-loopback capture of one process (and its descendants).
struct LoopbackCapture {
    pid: u32,
    client: IAudioClient,
    service: IAudioCaptureClient,
    event: HANDLE,
}

impl LoopbackCapture {
    fn start(pid: u32) -> Result<Self> {
        let client = activate_process_loopback_client(pid)?;
        let event = unsafe { CreateEventW(None, false, false, None)? };
        let started = (|| -> Result<IAudioCaptureClient> {
            unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK
                        | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                    CAPTURE_BUFFER_DURATION_100NS,
                    0,
                    &standard_format(),
                    None,
                )?;
                client.SetEventHandle(event)?;
                let service: IAudioCaptureClient = client.GetService()?;
                client.Start()?;
                Ok(service)
            }
        })();
        match started {
            Ok(service) => Ok(Self {
                pid,
                client,
                service,
                event,
            }),
            Err(error) => {
                unsafe {
                    let _ = CloseHandle(event);
                }
                Err(error)
            }
        }
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// What makes up an app's audio right now (see [`crate::process_tree`]).
struct AppAudio {
    /// The app and its helper processes that own audio sessions.
    processes: HashSet<u32>,
    /// The process to capture the app's audio from.
    target: u32,
    /// Every audio session of `processes`.
    sessions: Vec<OwnedSession>,
}

/// Looks up app `pid`'s audio in one pass over the sessions. Falls back to
/// just `pid` if the process list can't be read.
fn app_audio(pid: u32) -> AppAudio {
    let all = session_volume::all_sessions().unwrap_or_default();
    let owners: HashSet<u32> = all.iter().map(|&(owner, _)| owner).collect();
    let (processes, target) = match ProcessTree::snapshot() {
        Ok(tree) => (
            tree.app_audio_processes(pid, &owners),
            tree.capture_target(pid, &owners),
        ),
        Err(_) => (HashSet::from([pid]), pid),
    };
    let sessions = all
        .into_iter()
        .filter(|(owner, _)| processes.contains(owner))
        .collect();
    AppAudio {
        processes,
        target,
        sessions,
    }
}

/// Runs entirely on its own OS thread: initializes this thread's own COM
/// apartment, starts capturing app `pid`'s audio, reports the outcome over
/// `ready_tx`, then (on success) captures until `source.stop` is set. COM
/// interface pointers are never moved across threads — each thread that
/// uses them creates and owns them itself.
fn run_capture_thread(pid: u32, source: Arc<SourceInner>, ready_tx: mpsc::Sender<Result<()>>) {
    let _com = match ComGuard::init() {
        Ok(com) => com,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let _pro_audio = ProAudioThread::join();

    let app = app_audio(pid);
    let mut capture = match LoopbackCapture::start(app.target) {
        Ok(capture) => {
            let _ = ready_tx.send(Ok(()));
            capture
        }
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let mut found = Some(app);
    let mut sessions = Vec::new();
    let mut last_scan = Instant::now();
    let mut next_scan = last_scan + APP_RESCAN_INTERVAL;

    // Apps create new sessions as they go, which start at full volume, so
    // keep turning every session down. The capture is only boosted while
    // every session reads back as turned down; boosting audio that isn't
    // would make it 10,000x too loud.
    let mut last_routed = Instant::now() - ROUTE_ENFORCE_INTERVAL;
    let mut all_routed = false;
    let mut boost_blocked_until = Instant::now();
    let mut boosted = Vec::new();

    while !source.stop.load(Ordering::Relaxed) {
        if found.is_none() && Instant::now() >= next_scan {
            found = Some(app_audio(pid));
        }
        if let Some(app) = found.take() {
            // Keep the old capture if the new one can't start.
            if app.target != capture.pid {
                if let Ok(new_capture) = LoopbackCapture::start(app.target) {
                    capture = new_capture;
                }
            }
            let mut routed = source.routed.lock().unwrap();
            if source.stop.load(Ordering::Relaxed) {
                break;
            }
            // Give back processes that stopped being part of the app.
            let gone: HashSet<u32> = routed.difference(&app.processes).copied().collect();
            if !gone.is_empty() {
                session_volume::unroute(&gone);
            }
            session_volume::record_originals(&app.sessions);
            *routed = app.processes;
            sessions = app.sessions;
            last_scan = Instant::now();
            next_scan = last_scan + APP_RESCAN_INTERVAL;
            last_routed = Instant::now() - ROUTE_ENFORCE_INTERVAL;
        }
        if last_routed.elapsed() >= ROUTE_ENFORCE_INTERVAL {
            let _routed = source.routed.lock().unwrap();
            if source.stop.load(Ordering::Relaxed) {
                break;
            }
            all_routed = session_volume::enforce(&sessions);
            last_routed = Instant::now();
        }

        let wait_result = unsafe { WaitForSingleObject(capture.event, 100) };
        if wait_result != WAIT_OBJECT_0 {
            continue;
        }

        loop {
            let packet_size = match unsafe { capture.service.GetNextPacketSize() } {
                Ok(size) => size,
                Err(_) => break,
            };
            if packet_size == 0 {
                break;
            }

            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut num_frames = 0u32;
            let mut flags = 0u32;
            if unsafe {
                capture
                    .service
                    .GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None)
            }
            .is_err()
            {
                break;
            }

            if num_frames > 0 && !data_ptr.is_null() {
                let samples = unsafe {
                    std::slice::from_raw_parts(
                        data_ptr as *const f32,
                        num_frames as usize * CHANNELS,
                    )
                };
                let raw_peak = samples.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
                let turned_down = raw_peak <= ROUTED_VOLUME * ROUTED_HEADROOM;
                if all_routed && !turned_down {
                    // Louder than a turned-down app can be: some of its audio
                    // isn't turned down, likely a session we haven't found
                    // yet. Don't boost; look for it soon.
                    boost_blocked_until = Instant::now() + BOOST_BLOCK;
                    next_scan = next_scan.min(last_scan + APP_RESCAN_SOON);
                }
                let gain = if all_routed && turned_down && Instant::now() >= boost_blocked_until {
                    ROUTED_BOOST
                } else {
                    1.0
                };
                boosted.clear();
                boosted.extend(samples.iter().map(|&s| s * gain));

                let mut buffer = source.samples.lock().unwrap();
                buffer.extend(boosted.iter().copied());
                while buffer.len() > MAX_BUFFERED_SAMPLES {
                    buffer.pop_front();
                }
            }
            unsafe {
                let _ = capture.service.ReleaseBuffer(num_frames);
            }
        }
    }
}

/// One bus as seen by a single render tick.
struct BusSnapshot {
    name: String,
    bus: Arc<BusInner>,
    members: Vec<Arc<SourceInner>>,
    device_id: Option<String>,
    priority: Priority,
    send: bool,
    monitor: bool,
}

/// One opened output device's WASAPI client, kept alive only while at
/// least one bus currently targets it.
struct DeviceOutput {
    client: IAudioClient,
    render_service: IAudioRenderClient,
    buffer_frames: u32,
}

impl DeviceOutput {
    /// Opens and starts `device_id` (`None` = the system default).
    fn open(device_id: Option<&str>, format: &WAVEFORMATEX) -> Result<Self> {
        let (client, render_service) = open_render_client_for_device(device_id, format)?;
        unsafe {
            let buffer_frames = client.GetBufferSize()?;
            client.Start()?;
            Ok(Self {
                client,
                render_service,
                buffer_frames,
            })
        }
    }
}

impl Drop for DeviceOutput {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
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
/// source), there is one render thread total: it lazily opens/closes a
/// WASAPI render client per *distinct device a bus currently targets*,
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
    let _com = ComGuard::init()?;
    let _pro_audio = ProAudioThread::join();
    let format = standard_format();
    let mut device_outputs: HashMap<Option<String>, DeviceOutput> = HashMap::new();
    // Devices that failed, with when to try each again.
    let mut failed_devices: HashMap<Option<String>, Instant> = HashMap::new();
    // Monitored buses' audio on its way to the default device.
    let mut monitor_queue: VecDeque<f32> = VecDeque::new();
    // Reused across ticks so tapping doesn't allocate on the render thread.
    let mut tap_buffers: Vec<Vec<f32>> = Vec::new();

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
                    let members = sources
                        .values()
                        .filter(|s| *s.bus.lock().unwrap() == *name)
                        .cloned()
                        .collect();
                    BusSnapshot {
                        name: name.clone(),
                        bus: bus.clone(),
                        members,
                        device_id,
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
            match DeviceOutput::open(device_id.as_deref(), &format) {
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
            if !ensure_open(&snapshot.device_id)
                && !snapshot.send
                && snapshot.bus.fallback_to_default.load(Ordering::Relaxed)
            {
                snapshot.device_id = None;
                ensure_open(&None);
            }
        }
        let monitoring = bus_snapshot.iter().any(|s| s.monitor && s.device_id.is_some());
        if monitoring {
            ensure_open(&None);
        } else {
            monitor_queue.clear();
        }
        device_outputs.retain(|device_id, _| {
            bus_snapshot.iter().any(|s| s.device_id == *device_id) || (device_id.is_none() && monitoring)
        });
        // Buses with nowhere to play drop their audio rather than queue it
        // up to play late once a device is back; the rest drop any backlog
        // beyond what their priority allows.
        let mut queued_ms = vec![0.0f32; bus_snapshot.len()];
        for (bus_idx, snapshot) in bus_snapshot.iter().enumerate() {
            let playing = device_outputs.contains_key(&snapshot.device_id);
            let limit = ms_to_frames(queue_limit_ms(snapshot.priority)) as usize * CHANNELS;
            let keep = ms_to_frames(device_target_ms(snapshot.priority)) as usize * CHANNELS;
            for source in &snapshot.members {
                let mut samples = source.samples.lock().unwrap();
                if !playing {
                    samples.clear();
                } else if samples.len() > limit {
                    let excess = samples.len() - keep;
                    samples.drain(..excess - excess % CHANNELS);
                }
                let ms = samples.len() as f32 / CHANNELS as f32 / SAMPLE_RATE * 1000.0;
                queued_ms[bus_idx] = queued_ms[bus_idx].max(ms);
            }
        }
        // Each device is kept only as full as its most urgent bus allows.
        let device_targets: HashMap<&Option<String>, u32> = device_outputs
            .keys()
            .map(|device_id| {
                // The default device also carries monitored buses.
                let target = bus_snapshot
                    .iter()
                    .filter(|s| &s.device_id == device_id || (device_id.is_none() && s.monitor))
                    .map(|s| device_target_ms(s.priority))
                    .fold(f32::MAX, f32::min);
                (device_id, ms_to_frames(target))
            })
            .collect();
        let mut device_queued_ms: HashMap<Option<String>, f32> = HashMap::new();

        let master_gain_value = master_gain.load();
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

        for (device_id, output) in device_outputs.iter() {
            let Ok(padding) = (unsafe { output.client.GetCurrentPadding() }) else {
                broken_devices.push(device_id.clone());
                continue;
            };
            let target = device_targets.get(device_id).copied().unwrap_or(output.buffer_frames);
            device_queued_ms.insert(device_id.clone(), padding as f32 / SAMPLE_RATE * 1000.0);
            let available = output
                .buffer_frames
                .saturating_sub(padding)
                .min(target.saturating_sub(padding));
            if available == 0 {
                continue;
            }
            let Ok(buffer) = (unsafe { output.render_service.GetBuffer(available) }) else {
                broken_devices.push(device_id.clone());
                continue;
            };
            rendered_any = true;
            let render_ptr = buffer as *mut f32;

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
                        device_id: bus_device,
                        ..
                    },
                ) in bus_snapshot.iter().enumerate()
                {
                    if bus_device != device_id {
                        continue;
                    }
                    let mut bus_mix = [0.0f32; CHANNELS];
                    for (src_idx, source) in members.iter().enumerate() {
                        let mut buffer = source.samples.lock().unwrap();
                        if source.muted.load(Ordering::Relaxed) {
                            for _ in 0..CHANNELS {
                                buffer.pop_front();
                            }
                            continue;
                        }
                        let gain = source.gain.load();
                        let mut local_peak = 0.0f32;
                        for channel_mix in bus_mix.iter_mut() {
                            if let Some(sample) = buffer.pop_front() {
                                let gained = sample * gain;
                                *channel_mix += gained;
                                local_peak = local_peak.max(gained.abs());
                            }
                        }
                        drop(buffer);
                        source_peaks[bus_idx][src_idx] =
                            source_peaks[bus_idx][src_idx].max(local_peak);
                    }

                    bus.eq.lock().unwrap().process(&mut bus_mix);
                    if tap.is_some() {
                        tap_buffers[bus_idx].extend_from_slice(&bus_mix);
                    }
                    let bus_frame_peak = bus_mix.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
                    bus_peaks[bus_idx] = bus_peaks[bus_idx].max(bus_frame_peak);

                    if !bus.muted.load(Ordering::Relaxed) {
                        let bus_gain = bus.gain.load();
                        if bus.heard.load(Ordering::Relaxed) {
                            let mix = if bus_snapshot[bus_idx].send { &mut send_mix } else { &mut device_mix };
                            for (m, b) in mix.iter_mut().zip(bus_mix.iter()) {
                                *m += b * bus_gain;
                            }
                        }
                        if bus_snapshot[bus_idx].monitor && bus_device.is_some() {
                            monitor_queue.extend(bus_mix.iter().map(|b| b * bus_gain));
                        }
                    }
                }

                for (channel, m) in device_mix.iter().enumerate() {
                    let sample = (m * master_gain_value + send_mix[channel]).clamp(-1.0, 1.0);
                    master_peak = master_peak.max(sample.abs());
                    unsafe {
                        *render_ptr.add(frame * CHANNELS + channel) = sample;
                    }
                }
            }

            if unsafe { output.render_service.ReleaseBuffer(available, 0) }.is_err() {
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
            snapshot.bus.delay_ms.store(device_ms + queued_ms[bus_idx]);
        }

        if !rendered_any {
            // Short, since a High-priority device holds only ~20 ms.
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        // Levels accumulate until the UI takes them, so it sees every
        // block's peak however often it looks.
        for (bus_idx, BusSnapshot { bus, members, .. }) in bus_snapshot.iter().enumerate() {
            bus.level.raise_to(bus_peaks[bus_idx]);
            for (src_idx, source) in members.iter().enumerate() {
                source.level.raise_to(source_peaks[bus_idx][src_idx]);
            }
        }
        master_level.raise_to(master_peak);
    }

    Ok(())
}
