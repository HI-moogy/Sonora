use std::collections::VecDeque;

use bytes::Bytes;

/// One encoded video frame ready for MP4 (length-prefixed NAL units).
#[derive(Clone)]
pub(crate) struct VideoSample {
    /// Presentation time in 100 ns units since the recorder started.
    pub pts: i64,
    pub keyframe: bool,
    pub data: Bytes,
}

/// The most recent `span` of encoded video. The oldest kept frame is always
/// a keyframe, so every snapshot decodes from its first frame.
pub(crate) struct VideoRing {
    frames: VecDeque<VideoSample>,
    span: i64,
    bytes: usize,
}

impl VideoRing {
    pub(crate) fn new(span: i64) -> Self {
        Self { frames: VecDeque::new(), span, bytes: 0 }
    }

    pub(crate) fn push(&mut self, sample: VideoSample) {
        // Frames before the first keyframe can't be decoded by anything.
        if self.frames.is_empty() && !sample.keyframe {
            return;
        }
        self.bytes += sample.data.len();
        self.frames.push_back(sample);
        let start = self.start_index(self.span);
        for dropped in self.frames.drain(..start) {
            self.bytes -= dropped.data.len();
        }
    }

    /// The frames covering at least the last `span`, starting on a keyframe.
    pub(crate) fn snapshot(&self, span: i64) -> Vec<VideoSample> {
        self.frames.range(self.start_index(span)..).cloned().collect()
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Time covered from the oldest to the newest kept frame.
    pub(crate) fn duration(&self) -> i64 {
        match (self.frames.front(), self.frames.back()) {
            (Some(first), Some(last)) => last.pts - first.pts,
            _ => 0,
        }
    }

    /// The latest keyframe at or before `newest - span`, or the oldest frame
    /// if the buffer doesn't reach back that far yet.
    fn start_index(&self, span: i64) -> usize {
        let Some(newest) = self.frames.back() else {
            return 0;
        };
        let cutoff = newest.pts - span;
        self.frames
            .iter()
            .enumerate()
            .take_while(|(_, frame)| frame.pts <= cutoff)
            .filter(|(_, frame)| frame.keyframe)
            .map(|(index, _)| index)
            .last()
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pts: i64, keyframe: bool) -> VideoSample {
        VideoSample { pts, keyframe, data: Bytes::from(vec![0u8; 10]) }
    }

    /// Frames every 10 time units, a keyframe every 4th frame.
    fn filled(ring: &mut VideoRing, count: i64) {
        for i in 0..count {
            ring.push(frame(i * 10, i % 4 == 0));
        }
    }

    #[test]
    fn drops_leading_non_keyframes() {
        let mut ring = VideoRing::new(1_000);
        ring.push(frame(0, false));
        ring.push(frame(10, false));
        assert_eq!(ring.snapshot(1_000).len(), 0);
        ring.push(frame(20, true));
        assert_eq!(ring.snapshot(1_000)[0].pts, 20);
    }

    #[test]
    fn trims_to_whole_gops_covering_the_span() {
        let mut ring = VideoRing::new(50);
        filled(&mut ring, 20); // pts 0..=190, keyframes at 0,40,80,120,160
        let kept = ring.snapshot(50);
        // newest 190, cutoff 140: latest keyframe at or before 140 is 120.
        assert_eq!(kept.first().unwrap().pts, 120);
        assert!(kept.first().unwrap().keyframe);
        assert_eq!(kept.last().unwrap().pts, 190);
        assert_eq!(ring.bytes(), kept.len() * 10);
    }

    #[test]
    fn shorter_snapshot_than_buffer_still_starts_on_a_keyframe() {
        let mut ring = VideoRing::new(1_000);
        filled(&mut ring, 20);
        let clip = ring.snapshot(30); // cutoff 160 is itself a keyframe
        assert_eq!(clip.first().unwrap().pts, 160);
        assert_eq!(ring.duration(), 190);
    }
}
