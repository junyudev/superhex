//! Feeding and displaying a recording's speculative transcript.
use super::*;

impl Recording {
    /// Hands the stream every complete block of audio it has not seen, in
    /// final-clip coordinates (after the resampler delay).
    pub(super) fn feed_stream(&mut self) {
        let delay = self.resampler.as_ref().map_or(0, Fft::output_delay);
        #[cfg(target_os = "macos")]
        if let Some(session) = &mut self.mouse_voice {
            let recorded = &self.samples[delay.min(self.samples.len())..];
            let fresh = &recorded[session.fed().min(recorded.len())..];
            if fresh.len() >= STREAM_FEED_SAMPLES {
                session.feed(fresh);
            }
        }
        let Some(stream) = &mut self.stream else {
            return;
        };
        let recorded = &self.samples[delay.min(self.samples.len())..];
        let fresh = &recorded[stream.fed().min(recorded.len())..];
        if fresh.len() >= STREAM_FEED_SAMPLES {
            stream.feed(fresh);
        }
    }
}

impl DictationCapture {
    /// The active recording's text so far when its live stream published a
    /// change since the last call: only with live text on, and only for
    /// display. The finished clip's transcript replaces it.
    pub fn take_partial(&mut self) -> Option<Partial> {
        self.recording.as_mut()?.stream.as_mut()?.take_partial()
    }

    /// Shows the live text on the HUDs when it changed, and logs at debug
    /// how far it has got: word counts against the audio fed, never the
    /// words (the engine logs each stream's count of changes). Every OS's
    /// capture loop calls it after ingesting audio; it does nothing without
    /// live text.
    pub fn report_live_text(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(session) = self
            .recording
            .as_mut()
            .and_then(|recording| recording.mouse_voice.as_mut())
        {
            session.report();
            return;
        }
        let Some(partial) = self.take_partial() else {
            return;
        };
        let Some(recording) = &mut self.recording else {
            return;
        };
        tracing::debug!(
            fed_ms = recording
                .stream
                .as_ref()
                .map_or(0, |stream| stream.fed() / 16),
            stable_words = partial.stable.split_whitespace().count(),
            tentative_words = partial.tentative.split_whitespace().count(),
            "live text"
        );
        recording
            .live_text
            .get_or_insert_with(crate::dictation::live_text::Publisher::new)
            .show(&partial);
    }

    /// Opens a live stream for the active recording when the engine's model
    /// can stream. The stream receives the recording's 16 kHz audio from its
    /// first sample (pre-roll included) as it arrives, travels with the
    /// finished clip, and is cancelled with the recording.
    pub fn begin_stream(&mut self, engine: &TranscriptionEngine) {
        let Some(recording) = &mut self.recording else {
            return;
        };
        if recording.stream.is_none() {
            recording.stream = engine.begin();
            recording.feed_stream();
        }
    }

    /// A recording chooses once at its intentional hold, including models
    /// that cannot stream. A settings change affects the next recording.
    #[cfg(target_os = "macos")]
    pub(crate) fn begin_dictation_stream(&mut self, engine: Option<&TranscriptionEngine>) {
        self.begin_stream_with(
            engine,
            mouse_voice::current_backend(),
            mouse_voice::Capture::start,
        );
    }

    #[cfg(target_os = "macos")]
    fn begin_stream_with(
        &mut self,
        engine: Option<&TranscriptionEngine>,
        backend: backend::DictationBackend,
        start: impl FnOnce() -> Option<mouse_voice::Capture>,
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
        if backend == backend::DictationBackend::MouseVoice {
            recording.mouse_voice = start().map(Box::new);
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
    pub(crate) fn with_mouse_voice_for_test(mut self, capture: mouse_voice::Capture) -> Self {
        self.mouse_voice = Some(Box::new(capture));
        self.stream_backend = Some(backend::DictationBackend::MouseVoice);
        self
    }
    pub(crate) fn capture_backend(&self) -> Option<backend::DictationBackend> {
        self.stream_backend
    }
    pub(crate) fn take_mouse_voice(&mut self) -> Option<mouse_voice::Capture> {
        self.mouse_voice.take().map(|capture| *capture)
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
        capture.begin_stream_with(None, backend::DictationBackend::MouseVoice, start);
        assert_eq!(calls.get(), 0);
        assert!(matches!(
            capture.finish(origin + Duration::from_millis(200)),
            Finish::Discard
        ));
        capture.start(origin);
        capture.cancel();
        capture.begin_stream_with(None, backend::DictationBackend::MouseVoice, start);
        assert_eq!(calls.get(), 0);
        capture.start(origin);
        capture.mark_intentional();
        capture.begin_stream_with(None, backend::DictationBackend::MouseVoice, start);
        capture.begin_stream_with(None, backend::DictationBackend::Local, || {
            panic!("only one choice")
        });
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn intentional_capture_feeds_the_complete_backlog_and_hands_its_session_to_the_clip() {
        let origin = CaptureInstant::from_nanos(10_000_000_000);
        let config = mouse_voice::tests::configuration();
        let session = ::mouse_voice::Session::scripted(Vec::new(), Ok("final".into()));
        let mut capture = DictationCapture::new(16_000);
        capture.start(origin);
        capture.ingest(&vec![0.1; 6400], origin + Duration::from_millis(400));
        capture.become_intentional(origin + Duration::from_millis(400));
        capture.begin_stream_with(None, backend::DictationBackend::MouseVoice, || {
            Some(mouse_voice::Capture::new(
                session,
                config,
                ::mouse_voice::Options::default(),
            ))
        });
        let recording = capture.recording.as_ref().unwrap();
        assert_eq!(recording.mouse_voice.as_ref().unwrap().fed(), 6400);
        let Finish::Transcribe(mut clip) = capture.finish(origin + Duration::from_millis(400))
        else {
            panic!("an intentional hold transcribes")
        };
        assert!(capture.recording.is_none());
        assert_eq!(
            clip.capture_backend(),
            Some(backend::DictationBackend::MouseVoice)
        );
        let session = clip.take_mouse_voice().unwrap();
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
