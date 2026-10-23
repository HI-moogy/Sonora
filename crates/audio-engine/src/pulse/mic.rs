//! Capturing the microphone on Linux.

use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use super::conn::read_recorded;
use super::Connection;
use crate::denoise::{MicCleanup, MicPipeline};
use crate::error::Result;
use crate::mixer::{FinishOnDrop, SourceInner};

/// Runs on its own thread: records `device_id` (`None`: the default
/// microphone), reports whether that worked over `ready_tx`, then hands
/// the audio to a [`MicPipeline`] until `source.stop` is set or the
/// microphone goes away.
pub(crate) fn run_mic_capture(
    source: Arc<SourceInner>,
    device_id: Option<String>,
    cleanup: Arc<MicCleanup>,
    ready_tx: mpsc::Sender<Result<()>>,
) {
    let _finish = FinishOnDrop(source.clone());
    let started = Connection::open("Sonora microphone").and_then(|mut connection| {
        let stream = connection.record(device_id.as_deref(), 1, "Microphone")?;
        Ok((connection, stream))
    });
    let (mut connection, mut stream) = match started {
        Ok(started) => {
            let _ = ready_tx.send(Ok(()));
            started
        }
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let mut pipeline = MicPipeline::new(cleanup);
    let mut samples = Vec::new();
    while !source.stop.load(Ordering::Relaxed) {
        if connection.iterate_for(Duration::from_millis(50)).is_err() {
            break;
        }
        samples.clear();
        if read_recorded(&mut stream, &mut samples).is_err() {
            break;
        }
        if !pipeline.begin_batch() && !samples.is_empty() {
            pipeline.push(&samples, &source);
        }
    }
    let _ = stream.disconnect();
}
