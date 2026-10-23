//! Capturing one app's audio on Linux: the app's streams are moved to a
//! sink of its own, which plays nowhere, and the mixer records that sink's
//! monitor. Stopping moves them back to the default output.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use libpulse_binding::stream::Stream;

use super::conn::{module_text, read_recorded, SinkInput};
use super::{procs, Connection, APP_SINK_PREFIX, PREFIX};
use crate::error::Result;
use crate::mixer::{FinishOnDrop, SourceInner, MAX_BUFFERED_SAMPLES};

/// How often new streams of the app (a new tab playing, a call starting)
/// are looked for and moved over.
const RESCAN_INTERVAL: Duration = Duration::from_millis(500);

/// Runs on its own thread: routes app `pid` to its own sink and records
/// it, reports whether that worked over `ready_tx`, then records into
/// `source.samples` until `source.stop` is set.
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
    let mut samples = Vec::new();
    let mut last_scan = Instant::now();
    while !source.stop.load(Ordering::Relaxed) {
        if route.connection.iterate_for(Duration::from_millis(50)).is_err() {
            break;
        }
        samples.clear();
        if let Some(stream) = route.stream.as_mut() {
            if read_recorded(stream, &mut samples).is_err() {
                break;
            }
        }
        if !samples.is_empty() {
            let mut buffer = source.samples.lock().unwrap();
            buffer.extend(samples.iter().copied());
            while buffer.len() > MAX_BUFFERED_SAMPLES {
                buffer.pop_front();
            }
        }
        if last_scan.elapsed() >= RESCAN_INTERVAL {
            route.move_app_streams();
            last_scan = Instant::now();
        }
    }
    // Dropping `route` gives the app back its output.
}

/// An app routed to its own sink, with the recording of that sink.
struct AppRoute {
    // Dropped (disconnected) before its connection.
    stream: Option<Stream>,
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
            connection,
            pid,
            binary: procs::executable_name(pid),
            sink_name,
            sink_index,
            module,
            family: HashSet::new(),
            strangers: HashSet::new(),
        };
        let monitor = format!("{}.monitor", route.sink_name);
        route.stream = Some(route.connection.record(Some(&monitor), 2, &format!("Sonora: {name}"))?);
        route.move_app_streams();
        Ok(route)
    }

    /// Moves every stream of the app that isn't on its sink yet there.
    /// Cheap when nothing changed: /proc is only read again when a stream
    /// comes from a process it hasn't placed yet (a new helper, say).
    fn move_app_streams(&mut self) {
        let Ok(inputs) = self.connection.sink_inputs() else { return };
        let own_pid = std::process::id();
        let elsewhere: Vec<&SinkInput> =
            inputs.iter().filter(|input| input.sink != self.sink_index && input.pid != Some(own_pid)).collect();
        if elsewhere.is_empty() {
            return;
        }
        let unplaced = |pid: &u32| !self.family.contains(pid) && !self.strangers.contains(pid);
        if elsewhere.iter().filter_map(|input| input.pid).any(|pid| unplaced(&pid)) {
            self.family = procs::with_descendants(self.pid);
            self.strangers = elsewhere.iter().filter_map(|input| input.pid).filter(|pid| !self.family.contains(pid)).collect();
        }
        if !elsewhere.iter().any(|input| self.owns(input)) {
            return;
        }
        // Streams on another Sonora sink are another app's (or this one's,
        // routed to another channel): leave them be.
        let sonora_sinks: HashSet<u32> = self
            .connection
            .sinks()
            .unwrap_or_default()
            .iter()
            .filter(|sink| sink.name.starts_with(PREFIX))
            .map(|sink| sink.index)
            .collect();
        for input in elsewhere {
            if !sonora_sinks.contains(&input.sink) && self.owns(input) {
                let _ = self.connection.move_sink_input(input.index, &self.sink_name);
            }
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
        // Back to the default output, then remove the sink. (The server
        // would move them on its own, but not everywhere promptly.)
        if let (Ok(Some(default)), Ok(inputs)) = (self.connection.default_sink(), self.connection.sink_inputs()) {
            for input in inputs.into_iter().filter(|input| input.sink == self.sink_index) {
                let _ = self.connection.move_sink_input(input.index, &default);
            }
        }
        let _ = self.connection.unload_module(self.module);
    }
}
