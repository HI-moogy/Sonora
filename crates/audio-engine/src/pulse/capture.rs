//! Capturing one app's audio on Linux. Through the mixer, the app's
//! streams are moved to a sink of its own, which plays nowhere, and the
//! mixer records that sink's monitor. When nothing needs the mixer (see
//! [`crate::mixer::RoutePlan`]), they're moved straight to the channel's
//! output instead, at the channel's volume, and each is recorded there
//! only for the meters. Stopping moves them back to the default output, at
//! their own volume.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use libpulse_binding::stream::Stream;
use libpulse_binding::volume::{ChannelVolumes, Volume, VolumeLinear};

use super::conn::{module_text, read_recorded, SinkInput};
use super::{procs, Connection, APP_SINK_PREFIX, PREFIX};
use crate::error::Result;
use crate::mixer::{FinishOnDrop, RoutePlan, SourceInner, MAX_BUFFERED_SAMPLES};

/// How often new streams of the app (a new tab playing, a call starting)
/// are looked for and moved over.
const RESCAN_INTERVAL: Duration = Duration::from_millis(500);

/// Runs on its own thread: routes app `pid` to its own sink and records
/// it, reports whether that worked over `ready_tx`, then records into
/// `source.samples` until `source.stop` is set, following the route plan.
/// Stops early, marking the source exited, once the app closes.
pub(crate) fn run_app_capture(pid: u32, source: Arc<SourceInner>, ready_tx: mpsc::Sender<Result<()>>) {
    let _finish = FinishOnDrop(source.clone());
    let mut route = match AppRoute::start(pid, &source.name) {
        Ok(route) => {
            let _ = ready_tx.send(Ok(()));
            route
        }
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let watch = procs::ProcessWatch::open(pid);
    let mut exited = false;
    let mut samples = Vec::new();
    let mut last_scan = Instant::now();
    let mut plan = RoutePlan::default();
    while !source.stop.load(Ordering::Relaxed) {
        if route.connection.iterate_for(Duration::from_millis(50)).is_err() {
            break;
        }
        samples.clear();
        if route.read(&mut samples).is_err() {
            break;
        }
        if !samples.is_empty() {
            let mut buffer = source.samples.lock().unwrap();
            buffer.extend(samples.iter().copied());
            while buffer.len() > MAX_BUFFERED_SAMPLES {
                buffer.pop_front();
            }
        }
        let latest = source.plan.lock().unwrap().clone();
        let replanned = latest != plan;
        if replanned || last_scan.elapsed() >= RESCAN_INTERVAL {
            if watch.as_ref().is_some_and(procs::ProcessWatch::exited) {
                exited = true;
                break;
            }
            plan = latest;
            let was_direct = route.direct.is_some();
            route.follow(&plan);
            if route.direct.is_some() != was_direct {
                // What's queued was recorded the other way.
                source.samples.lock().unwrap().clear();
                source.direct.store(route.direct.is_some(), Ordering::Relaxed);
            }
            last_scan = Instant::now();
        }
    }
    // Gives the app back its output (helper processes can outlive it).
    drop(route);
    if exited {
        source.exited.store(true, Ordering::Relaxed);
    }
}

/// An app routed to its own sink, with the recording of that sink; or
/// playing direct on a channel's output.
struct AppRoute {
    // Dropped (disconnected) before its connection.
    stream: Option<Stream>,
    /// While direct: each of the app's streams (by index), recorded where
    /// it plays.
    monitors: HashMap<u32, Stream>,
    connection: Connection,
    pid: u32,
    /// The app's program, for streams whose pid isn't one we can see (an
    /// app in a sandbox, like a Flatpak, reports its own pids).
    binary: Option<String>,
    sink_name: String,
    sink_index: u32,
    module: u32,
    /// The app's processes, as of the last look through /proc.
    family: HashSet<u32>,
    /// Other processes with streams, as of that look: not the app's.
    strangers: HashSet<u32>,
    /// The output the app plays direct on, while it does.
    direct: Option<String>,
    /// The volume its streams get while direct.
    volume: f32,
    /// Each of the app's streams' own volume from before Sonora set it, to
    /// give back.
    own_volumes: HashMap<u32, ChannelVolumes>,
}

impl AppRoute {
    fn start(pid: u32, name: &str) -> Result<Self> {
        let mut connection = Connection::open("Sonora app capture")?;
        let sink_name = format!("{APP_SINK_PREFIX}{}_{pid}", std::process::id());
        let module = connection.load_module(
            "module-null-sink",
            &format!("sink_name={sink_name} sink_properties=\"device.description='{}'\"", module_text(&format!("Sonora: {name}"))),
        )?;
        let sink_index = connection.sinks()?.into_iter().find(|sink| sink.name == sink_name).map_or(u32::MAX, |sink| sink.index);
        let mut route = Self {
            stream: None,
            monitors: HashMap::new(),
            connection,
            pid,
            binary: procs::executable_name(pid),
            sink_name,
            sink_index,
            module,
            family: HashSet::new(),
            strangers: HashSet::new(),
            direct: None,
            volume: 1.0,
            own_volumes: HashMap::new(),
        };
        let monitor = format!("{}.monitor", route.sink_name);
        route.stream = Some(route.connection.record(Some(&monitor), 2, &format!("Sonora: {name}"))?);
        route.move_app_streams();
        Ok(route)
    }

    /// Appends what was recorded since last time: the app's sink (through
    /// the mixer), or each of its streams (direct; only for the meters, so
    /// they're simply one after another).
    fn read(&mut self, into: &mut Vec<f32>) -> Result<()> {
        if self.direct.is_some() {
            // A stream that ended (the app closed it) just stops counting.
            self.monitors.retain(|_, stream| read_recorded(stream, into).is_ok());
            Ok(())
        } else {
            match self.stream.as_mut() {
                Some(stream) => read_recorded(stream, into),
                None => Ok(()),
            }
        }
    }

    /// Carries out `plan`: plays the app direct on its channel's output
    /// (the server's default for `None`), or through its own sink, and
    /// picks up streams it opened since.
    fn follow(&mut self, plan: &RoutePlan) {
        let wanted = if plan.direct {
            match &plan.device {
                Some(device) => Some(device.clone()),
                None => self.connection.default_sink().ok().flatten(),
            }
        } else {
            None
        };
        if wanted != self.direct {
            if self.direct.is_some() {
                self.give_back_volumes();
                self.monitors.clear();
            }
            self.direct = wanted;
        }
        self.volume = plan.volume;
        self.move_app_streams();
    }

    /// Where the app's streams belong now: the output it plays direct on,
    /// or its own sink.
    fn destination(&mut self) -> Option<(u32, String)> {
        match &self.direct {
            None => Some((self.sink_index, self.sink_name.clone())),
            Some(name) => {
                let index = self.connection.sinks().ok()?.into_iter().find(|sink| &sink.name == name)?.index;
                Some((index, name.clone()))
            }
        }
    }

    /// Moves every stream of the app that isn't where it belongs there and,
    /// while direct, gives it the channel's volume and records it. Cheap
    /// when nothing changed: /proc is only read again when a stream comes
    /// from a process it hasn't placed yet (a new helper, say).
    fn move_app_streams(&mut self) {
        let Some((destination, destination_name)) = self.destination() else { return };
        let Ok(inputs) = self.connection.sink_inputs() else { return };
        let own_pid = std::process::id();
        let others: Vec<&SinkInput> = inputs.iter().filter(|input| input.pid != Some(own_pid)).collect();
        let unplaced = |pid: &u32| !self.family.contains(pid) && !self.strangers.contains(pid);
        if others.iter().filter_map(|input| input.pid).any(|pid| unplaced(&pid)) {
            self.family = procs::with_descendants(self.pid);
            self.strangers = others.iter().filter_map(|input| input.pid).filter(|pid| !self.family.contains(pid)).collect();
        }
        let mine: Vec<&SinkInput> = others.into_iter().filter(|input| self.owns(input)).collect();
        // Streams on another Sonora sink are another app's (or this one's,
        // routed to another channel): leave them be.
        if mine.iter().any(|input| input.sink != destination) {
            let others_sinks: HashSet<u32> = self
                .connection
                .sinks()
                .unwrap_or_default()
                .iter()
                .filter(|sink| sink.name.starts_with(PREFIX) && sink.index != self.sink_index)
                .map(|sink| sink.index)
                .collect();
            for input in &mine {
                if input.sink != destination && !others_sinks.contains(&input.sink) {
                    let _ = self.connection.move_sink_input(input.index, &destination_name);
                }
            }
        }
        if self.direct.is_some() {
            let volume = Volume::from(VolumeLinear(f64::from(self.volume)));
            for input in &mine {
                self.own_volumes.entry(input.index).or_insert(input.volume);
                if input.volume.max() != volume || input.volume.min() != volume {
                    let mut set = input.volume;
                    set.set(set.len(), volume);
                    let _ = self.connection.set_sink_input_volume(input.index, &set);
                }
                if !self.monitors.contains_key(&input.index) {
                    if let Ok(stream) = self.connection.record_sink_input(&destination_name, input.index, "Sonora meter") {
                        self.monitors.insert(input.index, stream);
                    }
                }
            }
        }
        // Streams gone for good don't need their volume back.
        let open: HashSet<u32> = inputs.iter().map(|input| input.index).collect();
        self.own_volumes.retain(|index, _| open.contains(index));
    }

    /// Puts back the app's streams' own volumes.
    fn give_back_volumes(&mut self) {
        for (index, volume) in std::mem::take(&mut self.own_volumes) {
            let _ = self.connection.set_sink_input_volume(index, &volume);
        }
    }

    fn owns(&self, input: &SinkInput) -> bool {
        match input.pid {
            Some(pid) if pid == std::process::id() => false,
            Some(pid) if self.family.contains(&pid) => true,
            // A pid we can't see is a sandboxed app's own numbering.
            Some(pid) if !procs::exists(pid) => self.binary.is_some() && input.binary == self.binary,
            _ => false,
        }
    }
}

impl Drop for AppRoute {
    fn drop(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            let _ = stream.disconnect();
        }
        for (_, mut stream) in self.monitors.drain() {
            let _ = stream.disconnect();
        }
        // Back to the default output at their own volume, then remove the
        // sink. (The server would move them on its own, but not everywhere
        // promptly.)
        let direct = self.direct.take();
        self.give_back_volumes();
        if let (Ok(Some(default)), Ok(sinks), Ok(inputs)) =
            (self.connection.default_sink(), self.connection.sinks(), self.connection.sink_inputs())
        {
            let direct_index = direct.and_then(|name| sinks.iter().find(|sink| sink.name == name).map(|sink| sink.index));
            for input in inputs {
                let on_its_sink = input.sink == self.sink_index;
                let direct_there = Some(input.sink) == direct_index && self.owns(&input);
                if on_its_sink || direct_there {
                    let _ = self.connection.move_sink_input(input.index, &default);
                }
            }
        }
        let _ = self.connection.unload_module(self.module);
    }
}
