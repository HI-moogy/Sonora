use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use windows::core::{Error, Result};
use windows::Win32::Foundation::{CloseHandle, E_INVALIDARG, E_UNEXPECTED, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, IAudioRenderClient, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::atomic_f32::AtomicF32;
use crate::com::ComGuard;
use crate::eq::{EqBands, EQ_BAND_COUNT};
use crate::loopback::{activate_process_loopback_client, standard_format};
use crate::mic::run_mic_capture_thread;
use crate::render::open_render_client_for_device;
use crate::session_volume;

const CAPTURE_BUFFER_DURATION_100NS: i64 = 2_000_000;
const CHANNELS: usize = 2;
const SAMPLE_RATE: f32 = 48_000.0;
/// Bounds how far a source's buffer can grow if the render thread falls
/// behind, so a stalled render loop can't leak memory indefinitely.
pub(crate) const MAX_BUFFERED_SAMPLES: usize = 48_000 * CHANNELS * 2;

pub(crate) struct SourceInner {
    /// `None` for the microphone source, which has no owning process.
    pid: Option<u32>,
    name: String,
    bus: Mutex<String>,
    gain: AtomicF32,
    muted: AtomicBool,
    pub(crate) level: AtomicF32,
    pub(crate) samples: Mutex<VecDeque<f32>>,
    pub(crate) stop: Arc<AtomicBool>,
}

struct BusInner {
    gain: AtomicF32,
    muted: AtomicBool,
    level: AtomicF32,
    eq: Mutex<EqBands>,
    /// `None` renders to the system default output device.
    device_id: Mutex<Option<String>>,
}

/// A snapshot of one source's current state for UI display.
pub struct SourceInfo {
    pub id: u64,
    pub pid: Option<u32>,
    pub name: String,
    pub bus: String,
    pub gain: f32,
    pub muted: bool,
    pub level: f32,
}

/// A snapshot of one bus's current state for UI display.
pub struct BusInfo {
    pub name: String,
    pub gain: f32,
    pub muted: bool,
    pub level: f32,
    pub eq_gains_db: [f32; EQ_BAND_COUNT],
    /// `None` means this bus renders to the system default output device.
    pub device_id: Option<String>,
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
}

impl Mixer {
    pub fn start() -> Result<Self> {
        let sources = Arc::new(Mutex::new(HashMap::new()));
        let buses = Arc::new(Mutex::new(HashMap::<String, Arc<BusInner>>::new()));
        let master_gain = Arc::new(AtomicF32::new(1.0));
        let master_level = Arc::new(AtomicF32::new(0.0));
        let render_stop = Arc::new(AtomicBool::new(false));

        {
            let sources = sources.clone();
            let buses = buses.clone();
            let master_gain = master_gain.clone();
            let master_level = master_level.clone();
            let render_stop = render_stop.clone();
            std::thread::Builder::new()
                .name("mixer-render".into())
                .spawn(move || {
                    if let Err(error) =
                        run_render_loop(sources, buses, master_gain, master_level, render_stop)
                    {
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
        })
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
    pub fn add_source(&self, pid: u32, name: impl Into<String>, bus: impl Into<String>) -> Result<u64> {
        // Capturing our own process would loop the mix back into itself, and
        // the capture thread's session muting would silence our own output.
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
            samples: Mutex::new(VecDeque::new()),
            stop: Arc::new(AtomicBool::new(false)),
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

        // Capturing is a non-destructive tap: pid's own session keeps
        // playing through its real output unless we separately silence it
        // here, so our mixer's processed copy becomes the only audible
        // path (and our gain/mute controls actually have an effect).
        session_volume::set_session_muted(pid, true);

        let id = self.next_id();
        self.sources.lock().unwrap().insert(id, inner);
        Ok(id)
    }

    /// Starts capturing the default microphone, denoised through RNNoise,
    /// into the named bus. Same activation/blocking contract as
    /// [`Self::add_source`].
    pub fn add_mic_source(&self, name: impl Into<String>, bus: impl Into<String>) -> Result<u64> {
        let bus_name = bus.into();
        self.ensure_bus(&bus_name);

        let inner = Arc::new(SourceInner {
            pid: None,
            name: name.into(),
            bus: Mutex::new(bus_name),
            gain: AtomicF32::new(1.0),
            muted: AtomicBool::new(false),
            level: AtomicF32::new(0.0),
            samples: Mutex::new(VecDeque::new()),
            stop: Arc::new(AtomicBool::new(false)),
        });

        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_source = inner.clone();
        std::thread::Builder::new()
            .name("capture-mic".into())
            .spawn(move || run_mic_capture_thread(thread_source, ready_tx))
            .expect("spawn mic capture thread");

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| Error::from(E_UNEXPECTED))??;

        let id = self.next_id();
        self.sources.lock().unwrap().insert(id, inner);
        Ok(id)
    }

    /// Stops capturing `id`; its thread exits and cleans up its WASAPI
    /// client within one poll interval (<=100ms). If it was a process
    /// source, restores that process's own session to unmuted, handing
    /// normal audio control back to it.
    pub fn remove_source(&self, id: u64) {
        if let Some(source) = self.sources.lock().unwrap().remove(&id) {
            source.stop.store(true, Ordering::Relaxed);
            if let Some(pid) = source.pid {
                session_volume::set_session_muted(pid, false);
            }
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

    /// Writes every currently-known bus's EQ settings to disk, so they're
    /// restored next time that bus name is used.
    pub fn save_eq_presets(&self) -> std::io::Result<()> {
        let mut presets = common::load_presets();
        for (name, bus) in self.buses.lock().unwrap().iter() {
            let gains_db = bus.eq.lock().unwrap().gains();
            presets.eq.insert(name.clone(), common::EqPreset { gains_db });
        }
        common::save_presets(&presets)
    }

    pub fn set_master_gain(&self, gain: f32) {
        self.master_gain.store(gain.max(0.0));
    }

    pub fn master_gain(&self) -> f32 {
        self.master_gain.load()
    }

    pub fn master_level(&self) -> f32 {
        self.master_level.load()
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
                level: s.level.load(),
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
                level: b.level.load(),
                eq_gains_db: b.eq.lock().unwrap().gains(),
                device_id: b.device_id.lock().unwrap().clone(),
            })
            .collect()
    }

    /// Creates `name`'s bus state if it doesn't exist yet, restoring its
    /// saved EQ preset (or flat/0dB if none was ever saved). Safe to call
    /// up front for buses that should always be shown (e.g. a fixed set of
    /// channels in the UI) even before anything is routed to them.
    pub fn ensure_bus(&self, name: &str) {
        let mut buses = self.buses.lock().unwrap();
        if buses.contains_key(name) {
            return;
        }
        let gains_db = common::load_presets()
            .eq
            .get(name)
            .map(|preset| preset.gains_db)
            .unwrap_or([0.0; EQ_BAND_COUNT]);
        buses.insert(
            name.to_string(),
            Arc::new(BusInner {
                gain: AtomicF32::new(1.0),
                muted: AtomicBool::new(false),
                level: AtomicF32::new(0.0),
                eq: Mutex::new(EqBands::from_gains(SAMPLE_RATE, gains_db)),
                device_id: Mutex::new(None),
            }),
        );
    }
}

impl Drop for Mixer {
    fn drop(&mut self) {
        self.render_stop.store(true, Ordering::Relaxed);
        for source in self.sources.lock().unwrap().values() {
            source.stop.store(true, Ordering::Relaxed);
            // Hand real audio control back to any still-captured process so
            // closing the mixer never leaves an app muted at the OS level.
            if let Some(pid) = source.pid {
                session_volume::set_session_muted(pid, false);
            }
        }
    }
}

/// Runs entirely on its own OS thread: initializes this thread's own COM
/// apartment, activates process-loopback capture for `pid`, reports the
/// outcome over `ready_tx`, then (on success) captures until `source.stop`
/// is set. COM interface pointers are never moved across threads — each
/// thread that uses them creates and owns them itself.
fn run_capture_thread(pid: u32, source: Arc<SourceInner>, ready_tx: mpsc::Sender<Result<()>>) {
    let _com = match ComGuard::init() {
        Ok(com) => com,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    let setup = (|| -> Result<(IAudioClient, IAudioCaptureClient, HANDLE)> {
        let capture_client = activate_process_loopback_client(pid)?;
        let event = unsafe { CreateEventW(None, false, false, None)? };
        let format = standard_format();
        unsafe {
            capture_client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                CAPTURE_BUFFER_DURATION_100NS,
                0,
                &format,
                None,
            )?;
            capture_client.SetEventHandle(event)?;
        }
        let capture_service: IAudioCaptureClient = unsafe { capture_client.GetService()? };
        unsafe {
            capture_client.Start()?;
        }
        Ok((capture_client, capture_service, event))
    })();

    let (capture_client, capture_service, event) = match setup {
        Ok(triple) => {
            let _ = ready_tx.send(Ok(()));
            triple
        }
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    // Capturing a process's audio does not stop it creating a *new* audio
    // session later (common for browsers/media apps as content changes),
    // which a one-time SetMute at activation can't reach. Re-assert it
    // periodically so a freshly-created session gets caught quickly rather
    // than silently playing alongside our processed copy.
    const MUTE_ENFORCE_INTERVAL: Duration = Duration::from_millis(500);
    let mut last_mute_enforced = std::time::Instant::now() - MUTE_ENFORCE_INTERVAL;

    while !source.stop.load(Ordering::Relaxed) {
        if last_mute_enforced.elapsed() >= MUTE_ENFORCE_INTERVAL {
            session_volume::set_session_muted(pid, true);
            last_mute_enforced = std::time::Instant::now();
        }

        let wait_result = unsafe { WaitForSingleObject(event, 100) };
        if wait_result != WAIT_OBJECT_0 {
            continue;
        }

        loop {
            let packet_size = match unsafe { capture_service.GetNextPacketSize() } {
                Ok(size) => size,
                Err(_) => break,
            };
            if packet_size == 0 {
                break;
            }

            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut num_frames = 0u32;
            let mut flags = 0u32;
            if unsafe { capture_service.GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None) }
                .is_err()
            {
                break;
            }

            if num_frames > 0 && !data_ptr.is_null() {
                let samples = unsafe {
                    std::slice::from_raw_parts(data_ptr as *const f32, num_frames as usize * CHANNELS)
                };
                let peak = samples.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
                source.level.store(peak);

                let mut buffer = source.samples.lock().unwrap();
                buffer.extend(samples.iter().copied());
                while buffer.len() > MAX_BUFFERED_SAMPLES {
                    buffer.pop_front();
                }
            }
            unsafe {
                let _ = capture_service.ReleaseBuffer(num_frames);
            }
        }
    }

    unsafe {
        let _ = capture_client.Stop();
        let _ = CloseHandle(event);
    }
}

/// One opened output device's WASAPI client, kept alive only while at
/// least one bus currently targets it.
struct DeviceOutput {
    client: IAudioClient,
    render_service: IAudioRenderClient,
    buffer_frames: u32,
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
) -> Result<()> {
    let _com = ComGuard::init()?;
    let format = standard_format();
    let mut device_outputs: HashMap<Option<String>, DeviceOutput> = HashMap::new();

    while !stop.load(Ordering::Relaxed) {
        // Snapshot once per render tick (not per sample), grouped by bus so
        // each bus's EQ runs once on that bus's own summed signal.
        let bus_snapshot: Vec<(Arc<BusInner>, Vec<Arc<SourceInner>>, Option<String>)> = {
            let sources = sources.lock().unwrap();
            let buses = buses.lock().unwrap();
            buses
                .iter()
                .map(|(name, bus)| {
                    let device_id = bus.device_id.lock().unwrap().clone();
                    let members = sources
                        .values()
                        .filter(|s| *s.bus.lock().unwrap() == *name)
                        .cloned()
                        .collect();
                    (bus.clone(), members, device_id)
                })
                .collect()
        };

        // Open any newly-needed device clients; close ones no bus targets
        // anymore.
        let needed_devices: std::collections::HashSet<Option<String>> =
            bus_snapshot.iter().map(|(_, _, device_id)| device_id.clone()).collect();
        for device_id in &needed_devices {
            if device_outputs.contains_key(device_id) {
                continue;
            }
            match open_render_client_for_device(device_id.as_deref(), &format) {
                Ok((client, render_service)) => {
                    let buffer_frames = unsafe { client.GetBufferSize()? };
                    unsafe {
                        client.Start()?;
                    }
                    device_outputs.insert(
                        device_id.clone(),
                        DeviceOutput { client, render_service, buffer_frames },
                    );
                }
                Err(error) => {
                    eprintln!("mixer: failed to open render device {device_id:?}: {error:?}");
                }
            }
        }
        device_outputs.retain(|device_id, output| {
            if needed_devices.contains(device_id) {
                true
            } else {
                unsafe {
                    let _ = output.client.Stop();
                }
                false
            }
        });

        let master_gain_value = master_gain.load();
        let mut master_peak = 0.0f32;
        let mut source_peaks: Vec<Vec<f32>> =
            bus_snapshot.iter().map(|(_, members, _)| vec![0.0f32; members.len()]).collect();
        let mut bus_peaks: Vec<f32> = vec![0.0f32; bus_snapshot.len()];
        let mut rendered_any = false;

        for (device_id, output) in device_outputs.iter() {
            let padding = unsafe { output.client.GetCurrentPadding()? };
            let available = output.buffer_frames.saturating_sub(padding);
            if available == 0 {
                continue;
            }
            rendered_any = true;

            let render_ptr = unsafe { output.render_service.GetBuffer(available)? } as *mut f32;

            for frame in 0..available as usize {
                let mut device_mix = [0.0f32; CHANNELS];

                for (bus_idx, (bus, members, bus_device)) in bus_snapshot.iter().enumerate() {
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
                        source_peaks[bus_idx][src_idx] = source_peaks[bus_idx][src_idx].max(local_peak);
                    }

                    bus.eq.lock().unwrap().process(&mut bus_mix);
                    let bus_frame_peak = bus_mix.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
                    bus_peaks[bus_idx] = bus_peaks[bus_idx].max(bus_frame_peak);

                    if !bus.muted.load(Ordering::Relaxed) {
                        let bus_gain = bus.gain.load();
                        for (m, b) in device_mix.iter_mut().zip(bus_mix.iter()) {
                            *m += b * bus_gain;
                        }
                    }
                }

                for (channel, m) in device_mix.iter().enumerate() {
                    let sample = (m * master_gain_value).clamp(-1.0, 1.0);
                    master_peak = master_peak.max(sample.abs());
                    unsafe {
                        *render_ptr.add(frame * CHANNELS + channel) = sample;
                    }
                }
            }

            unsafe {
                output.render_service.ReleaseBuffer(available, 0)?;
            }
        }

        for (bus_idx, (bus, members, _)) in bus_snapshot.iter().enumerate() {
            bus.level.store(bus_peaks[bus_idx]);
            for (src_idx, source) in members.iter().enumerate() {
                source.level.store(source_peaks[bus_idx][src_idx]);
            }
        }
        master_level.store(master_peak);

        if !rendered_any {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    for output in device_outputs.values() {
        unsafe {
            let _ = output.client.Stop();
        }
    }
    Ok(())
}
