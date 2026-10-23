//! A connection to the sound server, driven by the thread that owns it
//! (libpulse's objects stay on one thread), with the server queries and
//! changes Sonora uses.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use libpulse_binding as pa;
use pa::callbacks::ListResult;
use pa::context::{Context, FlagSet as ContextFlags, State as ContextState};
use pa::def::BufferAttr;
use pa::mainloop::standard::{IterateResult, Mainloop};
use pa::operation::{Operation, State as OperationState};
use pa::proplist::{properties, Proplist};
use pa::sample::{Format, Spec};
use pa::stream::{FlagSet as StreamFlags, PeekResult, State as StreamState, Stream};
use pa::time::MicroSeconds;

use crate::error::{timed_out, Error, Result};

/// How long to wait for the server to answer.
const TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const SAMPLE_RATE: u32 = 48_000;

pub(crate) struct Sink {
    pub(crate) index: u32,
    pub(crate) name: String,
    pub(crate) description: String,
}

pub(crate) struct Source {
    pub(crate) name: String,
    pub(crate) description: String,
    /// A sink's monitor (what it plays), not a microphone or line-in.
    pub(crate) is_monitor: bool,
}

/// An app's playback stream.
pub(crate) struct SinkInput {
    pub(crate) index: u32,
    /// The sink it plays on.
    pub(crate) sink: u32,
    pub(crate) pid: Option<u32>,
    pub(crate) app_name: Option<String>,
    pub(crate) binary: Option<String>,
    /// Event sounds (notifications, clicks) rather than an app's audio.
    pub(crate) is_event: bool,
}

pub(crate) struct Module {
    pub(crate) index: u32,
    pub(crate) argument: String,
}

pub(crate) struct Connection {
    // Dropped before the mainloop it runs on.
    pub(crate) context: Context,
    mainloop: Mainloop,
}

fn text(value: &Option<std::borrow::Cow<'_, str>>) -> String {
    value.as_deref().unwrap_or_default().to_string()
}

/// `text` safe to put in a module argument's quoted property value.
pub(crate) fn module_text(text: &str) -> String {
    text.chars().filter(|c| !matches!(c, '"' | '\'' | '\\')).collect()
}

impl Connection {
    /// Connects, naming the connection for `purpose` (shown in tools like
    /// pavucontrol).
    pub(crate) fn open(purpose: &str) -> Result<Self> {
        let unavailable = || Error::new("couldn't start talking to the sound server");
        let mainloop = Mainloop::new().ok_or_else(unavailable)?;
        let mut proplist = Proplist::new().ok_or_else(unavailable)?;
        let _ = proplist.set_str(properties::APPLICATION_NAME, "Sonora");
        let _ = proplist.set_str(properties::APPLICATION_ID, "sonora");
        let _ = proplist.set_str(properties::APPLICATION_ICON_NAME, "sonora");
        let mut context = Context::new_with_proplist(&mainloop, purpose, &proplist).ok_or_else(unavailable)?;
        context.connect(None, ContextFlags::NOFLAGS, None).map_err(|error| {
            Error::new(format!("Couldn't connect to the sound server (PipeWire or PulseAudio): {error}"))
        })?;
        let mut connection = Self { context, mainloop };
        let deadline = Instant::now() + TIMEOUT;
        loop {
            connection.iterate_for(Duration::from_millis(20))?;
            match connection.context.get_state() {
                ContextState::Ready => return Ok(connection),
                ContextState::Failed | ContextState::Terminated => {
                    return Err(Error::new("The sound server refused the connection. Is PipeWire or PulseAudio running?"));
                }
                _ if Instant::now() > deadline => return Err(timed_out()),
                _ => {}
            }
        }
    }

    /// Handles what the server sent, waiting up to `timeout` for something.
    pub(crate) fn iterate_for(&mut self, timeout: Duration) -> Result<()> {
        let failed = |error: pa::error::PAErr| Error::new(format!("the sound server connection failed: {error}"));
        self.mainloop.prepare(Some(MicroSeconds(timeout.as_micros() as u64))).map_err(failed)?;
        self.mainloop.poll().map_err(failed)?;
        self.mainloop.dispatch().map_err(failed)?;
        Ok(())
    }

    /// Handles what the server sent, without waiting.
    pub(crate) fn iterate(&mut self) -> Result<()> {
        match self.mainloop.iterate(false) {
            IterateResult::Success(_) => Ok(()),
            IterateResult::Quit(_) => Err(Error::new("the sound server connection closed")),
            IterateResult::Err(error) => Err(Error::new(format!("the sound server connection failed: {error}"))),
        }
    }

    /// Runs until `operation` is done.
    fn wait<T: ?Sized>(&mut self, mut operation: Operation<T>) -> Result<()> {
        let deadline = Instant::now() + TIMEOUT;
        while operation.get_state() == OperationState::Running {
            if Instant::now() > deadline {
                operation.cancel();
                return Err(timed_out());
            }
            self.iterate_for(Duration::from_millis(20))?;
        }
        Ok(())
    }

    pub(crate) fn sinks(&mut self) -> Result<Vec<Sink>> {
        let found = Rc::new(RefCell::new(Vec::new()));
        let into = found.clone();
        let operation = self.context.introspect().get_sink_info_list(move |result| {
            if let ListResult::Item(info) = result {
                into.borrow_mut().push(Sink { index: info.index, name: text(&info.name), description: text(&info.description) });
            }
        });
        self.wait(operation)?;
        Ok(found.take())
    }

    pub(crate) fn sources(&mut self) -> Result<Vec<Source>> {
        let found = Rc::new(RefCell::new(Vec::new()));
        let into = found.clone();
        let operation = self.context.introspect().get_source_info_list(move |result| {
            if let ListResult::Item(info) = result {
                into.borrow_mut().push(Source {
                    name: text(&info.name),
                    description: text(&info.description),
                    is_monitor: info.monitor_of_sink.is_some(),
                });
            }
        });
        self.wait(operation)?;
        Ok(found.take())
    }

    pub(crate) fn sink_inputs(&mut self) -> Result<Vec<SinkInput>> {
        let found = Rc::new(RefCell::new(Vec::new()));
        let into = found.clone();
        let operation = self.context.introspect().get_sink_input_info_list(move |result| {
            if let ListResult::Item(info) = result {
                let property = |key: &str| info.proplist.get_str(key).filter(|value| !value.is_empty());
                into.borrow_mut().push(SinkInput {
                    index: info.index,
                    sink: info.sink,
                    pid: property(properties::APPLICATION_PROCESS_ID).and_then(|pid| pid.parse().ok()),
                    app_name: property(properties::APPLICATION_NAME),
                    binary: property(properties::APPLICATION_PROCESS_BINARY),
                    is_event: property(properties::MEDIA_ROLE).as_deref() == Some("event"),
                });
            }
        });
        self.wait(operation)?;
        Ok(found.take())
    }

    pub(crate) fn modules(&mut self) -> Result<Vec<Module>> {
        let found = Rc::new(RefCell::new(Vec::new()));
        let into = found.clone();
        let operation = self.context.introspect().get_module_info_list(move |result| {
            if let ListResult::Item(info) = result {
                into.borrow_mut().push(Module { index: info.index, argument: text(&info.argument) });
            }
        });
        self.wait(operation)?;
        Ok(found.take())
    }

    /// The output apps play on unless told otherwise.
    pub(crate) fn default_sink(&mut self) -> Result<Option<String>> {
        let found = Rc::new(RefCell::new(None));
        let into = found.clone();
        let operation = self.context.introspect().get_server_info(move |info| {
            *into.borrow_mut() = info.default_sink_name.as_ref().map(|name| name.to_string());
        });
        self.wait(operation)?;
        Ok(found.take())
    }

    /// Loads a server module (a sink, a source...); returns its index.
    pub(crate) fn load_module(&mut self, name: &str, argument: &str) -> Result<u32> {
        let found = Rc::new(RefCell::new(None));
        let into = found.clone();
        let operation = self.context.introspect().load_module(name, argument, move |index| {
            *into.borrow_mut() = Some(index);
        });
        self.wait(operation)?;
        match found.take() {
            Some(index) if index != u32::MAX => Ok(index),
            _ => Err(Error::new(format!("the sound server couldn't set up {name}"))),
        }
    }

    pub(crate) fn unload_module(&mut self, index: u32) -> Result<()> {
        let operation = self.context.introspect().unload_module(index, |_| {});
        self.wait(operation)
    }

    /// Moves an app's stream to the sink named `sink`.
    pub(crate) fn move_sink_input(&mut self, index: u32, sink: &str) -> Result<()> {
        let operation = self.context.introspect().move_sink_input_by_name(index, sink, None);
        self.wait(operation)
    }

    /// Starts recording `device` (`None`: the default microphone) as f32 at
    /// 48 kHz, in blocks of about 10 ms. A chosen device that goes away ends
    /// the recording; the default one follows the server's default.
    pub(crate) fn record(&mut self, device: Option<&str>, channels: u8, purpose: &str) -> Result<Stream> {
        let spec = Spec { format: Format::F32le, channels, rate: SAMPLE_RATE };
        let mut stream = Stream::new(&mut self.context, purpose, &spec, None)
            .ok_or_else(|| Error::new("couldn't create a recording stream"))?;
        let block = SAMPLE_RATE / 100 * u32::from(channels) * 4;
        let attr = BufferAttr { maxlength: u32::MAX, tlength: u32::MAX, prebuf: u32::MAX, minreq: u32::MAX, fragsize: block };
        let flags = if device.is_some() { StreamFlags::ADJUST_LATENCY | StreamFlags::DONT_MOVE } else { StreamFlags::ADJUST_LATENCY };
        stream
            .connect_record(device, Some(&attr), flags)
            .map_err(|error| Error::new(format!("couldn't record {}: {error}", device.unwrap_or("the microphone"))))?;
        self.wait_ready(&stream)?;
        Ok(stream)
    }

    /// Runs until `stream` is ready to use.
    pub(crate) fn wait_ready(&mut self, stream: &Stream) -> Result<()> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match stream.get_state() {
                StreamState::Ready => return Ok(()),
                StreamState::Failed | StreamState::Terminated => {
                    return Err(Error::new("the sound server closed the stream (is the device still there?)"));
                }
                _ if Instant::now() > deadline => return Err(timed_out()),
                _ => self.iterate_for(Duration::from_millis(20))?,
            }
        }
    }
}

thread_local! {
    /// This thread's connection for queries and changes (lists, modules,
    /// moving streams), kept between calls.
    static CONTROL: std::cell::RefCell<Option<Connection>> = const { std::cell::RefCell::new(None) };
}

/// Runs `query` on this thread's long-lived connection, connecting on first
/// use and again if the server went away (it restarted, say), rather than
/// connecting for every query.
pub(crate) fn with_control<T>(query: impl Fn(&mut Connection) -> Result<T>) -> Result<T> {
    CONTROL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(connection) = slot.as_mut() {
            if connection.iterate().is_ok() && matches!(connection.context.get_state(), ContextState::Ready) {
                match query(connection) {
                    Ok(result) => return Ok(result),
                    Err(_) => *slot = None,
                }
            } else {
                *slot = None;
            }
        }
        let mut connection = Connection::open("Sonora")?;
        let result = query(&mut connection);
        *slot = Some(connection);
        result
    })
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.context.disconnect();
    }
}

/// Appends what `stream` has recorded so far to `into`. An error means the
/// stream ended (its device went away).
pub(crate) fn read_recorded(stream: &mut Stream, into: &mut Vec<f32>) -> Result<()> {
    if !matches!(stream.get_state(), StreamState::Ready) {
        return Err(Error::new("the recording stopped"));
    }
    loop {
        match stream.peek().map_err(|error| Error::new(format!("couldn't read the recording: {error}")))? {
            PeekResult::Empty => return Ok(()),
            PeekResult::Hole(_) => {}
            PeekResult::Data(bytes) => {
                into.extend(bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
            }
        }
        stream.discard().map_err(|error| Error::new(format!("couldn't read the recording: {error}")))?;
    }
}
