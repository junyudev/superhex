//! A session and its live-text owner travel together from capture to job.
use std::sync::Arc;
use std::time::Instant;

use voice_buddy::{Canceller, Config, Failed, Options, Session, Update};

use super::super::live_text::{Line, Phase, Publisher};
use crate::transcription::engine::{Partial, Source, Transcribed};

pub(crate) struct Capture {
    session: Session,
    config: Arc<Config>,
    options: Options,
    fed: usize,
    offline: bool,
    display: Display,
}

struct Display {
    publisher: Publisher,
    heard: String,
    /// The service said it is polishing; later updates don't take that back.
    polishing: bool,
}

impl Display {
    fn show(&self, phase: Phase) {
        self.publisher.show_line(Line {
            // The plate keeps the newest of its stable words in view.
            partial: Partial {
                stable: self.heard.clone(),
                tentative: String::new(),
            },
            phase,
        });
    }
    fn update(&mut self, update: &Update, finishing: bool) {
        self.heard.clone_from(&update.text);
        self.polishing |= update.polishing;
        self.show(if self.polishing {
            Phase::Polishing
        } else if finishing {
            Phase::Finishing
        } else {
            Phase::Hearing
        });
    }
}

impl Capture {
    pub(crate) fn start() -> Option<Self> {
        let config = super::config()?;
        let options = super::options(&config);
        let session = Session::start(config.clone(), options);
        Some(Self::new(session, config, options))
    }
    pub(crate) fn new(session: Session, config: Arc<Config>, options: Options) -> Self {
        let display = Display {
            publisher: Publisher::new(),
            heard: String::new(),
            polishing: false,
        };
        display.show(Phase::Hearing);
        Self {
            session,
            config,
            options,
            fed: 0,
            offline: false,
            display,
        }
    }
    pub(crate) fn fed(&self) -> usize {
        self.fed
    }
    pub(crate) fn feed(&mut self, samples: &[f32]) {
        self.session.feed(samples);
        self.fed += samples.len();
    }
    pub(crate) fn report(&mut self) {
        if let Some(update) = self.session.take_update() {
            self.display.heard.clone_from(&update.text);
            self.display.show(if self.offline {
                Phase::Offline
            } else {
                Phase::Hearing
            });
        }
        if self.offline {
            return;
        }
        if let Some(failure) = self.session.failure() {
            super::reject(&self.config, &failure);
            self.offline = true;
            self.display.show(Phase::Offline);
        }
    }
    pub(crate) fn finish(mut self, samples: &[f32]) -> Pending {
        self.display.show(Phase::Finishing);
        // A delayed shortcut edge may trim audio the speculative session
        // already saw. Only the complete, boundary-correct clip may produce
        // output, just as the local stream's overfeed fallback requires.
        let session = if self.fed > samples.len() {
            tracing::debug!(
                fed_ms = self.fed / 16,
                audio_ms = samples.len() / 16,
                "speech session passed the clip boundary; using the whole clip"
            );
            drop(self.session);
            None
        } else {
            self.feed(&samples[self.fed..]);
            Some(self.session.finish())
        };
        let cancel = session
            .as_ref()
            .map_or_else(Canceller::new, voice_buddy::Pending::canceller);
        Pending {
            session,
            cancel,
            config: self.config,
            options: self.options,
            display: self.display,
        }
    }
}

pub(crate) struct Pending {
    session: Option<voice_buddy::Pending>,
    cancel: Canceller,
    config: Arc<Config>,
    options: Options,
    display: Display,
}

impl Pending {
    /// Whole clips run on the job thread, without creating a live session.
    pub(crate) fn whole() -> Option<Self> {
        let config = super::config()?;
        let display = Display {
            publisher: Publisher::new(),
            heard: String::new(),
            polishing: false,
        };
        display.show(Phase::Finishing);
        Some(Self {
            session: None,
            cancel: Canceller::new(),
            options: super::options(&config),
            config,
            display,
        })
    }
    pub(crate) fn canceller(&self) -> Canceller {
        self.cancel.clone()
    }
    pub(crate) fn wait(
        mut self,
        samples: &[f32],
        cancel: &Canceller,
        cancelled: impl Fn() -> bool,
    ) -> Result<Transcribed, Failed> {
        let started = Instant::now();
        let result = match self.session.take() {
            Some(pending) => pending.wait(|update| self.display.update(update, true)),
            None => voice_buddy::transcribe(
                self.config.clone(),
                self.options,
                samples,
                Some(cancel),
                |update| self.display.update(update, true),
            ),
        };
        let result = retry_once(result, cancelled, || {
            self.display.show(Phase::Finishing);
            voice_buddy::transcribe(
                self.config.clone(),
                self.options,
                samples,
                Some(cancel),
                |update| {
                    self.display.heard.clone_from(&update.text);
                    self.display.show(Phase::Finishing);
                },
            )
        });
        if let Err(failed) = &result {
            super::reject(&self.config, &failed.failure);
            super::persist_rejection(&self.config, &failed.failure);
        }
        result.map(|text| Transcribed {
            text,
            source: Source::VoiceBuddy,
            finish_ms: started.elapsed().as_millis() as u64,
        })
    }
}

/// Exactly one retry. The most recent nonempty hypothesis survives failure.
pub(crate) fn retry_once(
    initial: Result<String, Failed>,
    cancelled: impl Fn() -> bool,
    retry: impl FnOnce() -> Result<String, Failed>,
) -> Result<String, Failed> {
    let Err(first) = initial else { return initial };
    if !first.failure.retryable() || cancelled() {
        return Err(first);
    }
    retry().map_err(|mut last| {
        if last.heard.is_empty() {
            last.heard = first.heard;
        }
        last
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    fn failed(failure: voice_buddy::Failure) -> Failed {
        Failed {
            failure,
            heard: "heard".into(),
        }
    }
    #[test]
    fn a_retry_succeeds_once_and_a_second_failure_keeps_the_hypothesis() {
        let calls = Cell::new(0);
        let run = || {
            calls.set(calls.get() + 1);
            Ok("final".into())
        };
        assert_eq!(
            retry_once(Err(failed(voice_buddy::Failure::TimedOut)), || false, run).unwrap(),
            "final"
        );
        assert_eq!(calls.get(), 1);
        let result = retry_once(
            Err(failed(voice_buddy::Failure::TimedOut)),
            || false,
            || {
                Err(Failed {
                    failure: voice_buddy::Failure::Service(503),
                    heard: String::new(),
                })
            },
        )
        .unwrap_err();
        assert_eq!(result.heard, "heard");
        assert_eq!(result.failure, voice_buddy::Failure::Service(503));
    }
    #[test]
    fn rejected_and_cancelled_work_never_retries() {
        for (failure, cancelled) in [
            (voice_buddy::Failure::Rejected(401), false),
            (voice_buddy::Failure::TimedOut, true),
        ] {
            let original = failed(failure);
            assert_eq!(
                retry_once(
                    Err(original.clone()),
                    || cancelled,
                    || panic!("must not retry")
                ),
                Err(original)
            );
        }
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::dictation::live_text;
    use std::sync::Mutex;

    #[test]
    fn publisher_moves_from_recording_through_finish_and_polish_and_then_clears() {
        let phases = Arc::new(Mutex::new(Vec::new()));
        let log = phases.clone();
        live_text::watch(move |line| log.lock().unwrap().push(line.map(|line| line.phase)));
        let config = super::super::tests::configuration();
        let session = Session::scripted(
            vec![
                Update {
                    text: "heard".into(),
                    polishing: false,
                },
                Update {
                    text: "heard".into(),
                    polishing: true,
                },
            ],
            Ok("final".into()),
        );
        let mut capture = Capture::new(session, config, Options::default());
        capture.feed(&[0.1; 320]);
        capture.report();
        assert_eq!(live_text::current().unwrap().stable, "heard");
        let pending = capture.finish(&[0.1; 640]);
        assert!(
            live_text::current().is_some(),
            "recording handed its Publisher to the job"
        );
        let cancel = pending.canceller();
        assert_eq!(
            pending.wait(&[0.1; 640], &cancel, || false).unwrap().text,
            "final"
        );
        assert!(live_text::current().is_none());
        assert_eq!(
            *phases.lock().unwrap(),
            [
                Some(Phase::Hearing),
                Some(Phase::Hearing),
                Some(Phase::Finishing),
                Some(Phase::Polishing),
                None
            ]
        );
    }

    #[test]
    fn failed_sessions_show_offline_and_keep_feeding_until_capture_is_cancelled() {
        let phases = Arc::new(Mutex::new(Vec::new()));
        let log = phases.clone();
        live_text::watch(move |line| log.lock().unwrap().push(line.map(|line| line.phase)));
        let config = super::super::tests::configuration();
        let session = Session::scripted(
            Vec::new(),
            Err(Failed {
                failure: voice_buddy::Failure::TimedOut,
                heard: "heard".into(),
            }),
        );
        let mut capture = Capture::new(session, config, Options::default());
        capture.feed(&[0.1; 320]);
        capture.report();
        capture.report();
        assert_eq!(
            phases
                .lock()
                .unwrap()
                .iter()
                .filter(|phase| **phase == Some(Phase::Offline))
                .count(),
            1
        );
        capture.feed(&[0.2; 640]);
        assert_eq!(capture.fed(), 960);
        assert_eq!(phases.lock().unwrap().last(), Some(&Some(Phase::Offline)));
        drop(capture);
        assert_eq!(phases.lock().unwrap().last(), Some(&None));
    }
    #[test]
    fn audio_past_a_delayed_release_cannot_supply_the_final_text() {
        let config = super::super::tests::configuration();
        let session = Session::scripted(Vec::new(), Ok("words after release".into()));
        let mut capture = Capture::new(session, config, Options::default());
        capture.feed(&[0.1; 640]);
        let pending = capture.finish(&[0.1; 320]);
        assert!(
            pending.session.is_none(),
            "the job must transcribe the exact whole clip instead"
        );
    }

    #[test]
    fn polishing_is_not_taken_back_by_a_later_update() {
        let phases = Arc::new(Mutex::new(Vec::new()));
        let log = phases.clone();
        live_text::watch(move |line| log.lock().unwrap().push(line.map(|line| line.phase)));
        let update = |polishing| Update {
            text: "heard".into(),
            polishing,
        };
        let session = Session::scripted(vec![update(true), update(false)], Ok("final".into()));
        let capture = Capture::new(
            session,
            super::super::tests::configuration(),
            Options::default(),
        );
        let pending = capture.finish(&[]);
        let cancel = pending.canceller();
        pending.wait(&[], &cancel, || false).unwrap();
        let phases = phases.lock().unwrap();
        assert_eq!(phases[phases.len() - 2], Some(Phase::Polishing));
    }
}
