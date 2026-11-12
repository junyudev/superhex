//! VoiceBuddy's share of a recording's speculative transcript: which backend a
//! recording chose, its service session, and the clip handed to the job.
//! The local stream's feeding and display live in [`super`].
use super::*;

impl DictationCapture {
    /// A recording chooses once at its intentional hold, including models
    /// that cannot stream. A settings change affects the next recording.
    #[cfg(target_os = "macos")]
    pub(crate) fn begin_dictation_stream(&mut self, engine: Option<&TranscriptionEngine>) {
        self.begin_stream_with(
            engine,
            voice_buddy::current_backend(),
            voice_buddy::Capture::start,
        );
    }

    #[cfg(target_os = "macos")]
    fn begin_stream_with(
        &mut self,
        engine: Option<&TranscriptionEngine>,
        backend: backend::DictationBackend,
        start: impl FnOnce() -> Option<voice_buddy::Capture>,
    ) {
        if !self.is_intentional() {
            return;
        }
        let Some(recording) = &mut self.recording else {
            return;
        };
        if recording.stream_backend.is_some() {
            return;
        }
        recording.stream_backend = Some(backend);
        if backend == backend::DictationBackend::VoiceBuddy {
            recording.voice_buddy = start().map(Box::new);
            recording.feed_stream();
            return;
        }
        if let Some(engine) = engine {
            self.begin_stream(engine);
        }
    }
}

#[cfg(target_os = "macos")]
impl DictationClip {
    #[cfg(test)]
    pub(crate) fn with_voice_buddy_for_test(mut self, capture: voice_buddy::Capture) -> Self {
        self.voice_buddy = Some(Box::new(capture));
        self.stream_backend = Some(backend::DictationBackend::VoiceBuddy);
        self
    }
    pub(crate) fn capture_backend(&self) -> Option<backend::DictationBackend> {
        self.stream_backend
    }
    pub(crate) fn take_voice_buddy(&mut self) -> Option<voice_buddy::Capture> {
        self.voice_buddy.take().map(|capture| *capture)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn short_taps_and_cancels_never_open_a_service_session() {
        let origin = CaptureInstant::from_nanos(10_000_000_000);
        let calls = Cell::new(0);
        let mut capture = DictationCapture::new(16_000);
        let start = || {
            calls.set(calls.get() + 1);
            None
        };
        capture.start(origin);
        capture.ingest(&vec![0.1; 3200], origin + Duration::from_millis(200));
        capture.begin_stream_with(None, backend::DictationBackend::VoiceBuddy, start);
        assert_eq!(calls.get(), 0);
        assert!(matches!(
            capture.finish(origin + Duration::from_millis(200)),
            Finish::Discard
        ));
        capture.start(origin);
        capture.cancel();
        capture.begin_stream_with(None, backend::DictationBackend::VoiceBuddy, start);
        assert_eq!(calls.get(), 0);
        capture.start(origin);
        capture.mark_intentional();
        capture.begin_stream_with(None, backend::DictationBackend::VoiceBuddy, start);
        capture.begin_stream_with(None, backend::DictationBackend::Local, || {
            panic!("only one choice")
        });
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn intentional_capture_feeds_the_complete_backlog_and_hands_its_session_to_the_clip() {
        let origin = CaptureInstant::from_nanos(10_000_000_000);
        let config = voice_buddy::tests::configuration();
        let session = ::voice_buddy::Session::scripted(Vec::new(), Ok("final".into()));
        let mut capture = DictationCapture::new(16_000);
        capture.start(origin);
        capture.ingest(&vec![0.1; 6400], origin + Duration::from_millis(400));
        capture.become_intentional(origin + Duration::from_millis(400));
        capture.begin_stream_with(None, backend::DictationBackend::VoiceBuddy, || {
            Some(voice_buddy::Capture::new(
                session,
                config,
                ::voice_buddy::Options::default(),
            ))
        });
        let recording = capture.recording.as_ref().unwrap();
        assert_eq!(recording.voice_buddy.as_ref().unwrap().fed(), 6400);
        let Finish::Transcribe(mut clip) = capture.finish(origin + Duration::from_millis(400))
        else {
            panic!("an intentional hold transcribes")
        };
        assert!(capture.recording.is_none());
        assert_eq!(
            clip.capture_backend(),
            Some(backend::DictationBackend::VoiceBuddy)
        );
        let session = clip.take_voice_buddy().unwrap();
        assert_eq!(session.fed(), 6400);
        let samples = clip.into_transcription_samples();
        let pending = session.finish(&samples);
        let cancel = pending.canceller();
        assert_eq!(
            pending.wait(&samples, &cancel, || false).unwrap().text,
            "final"
        );
    }
}
