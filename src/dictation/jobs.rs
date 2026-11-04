//! Dictation admission and transcription sources. Local-only callers never enter here.
use crate::dictation::backend::{DictationBackend, FailedAction, FailureReason};
use crate::dictation::voice_buddy::{self as remote, Pending, RetryClip};
use crate::parakeet::*;
use crate::transcription::engine::{PendingTranscript, Transcribed};

struct Submission {
    samples: Vec<f32>,
    audio_ms: u64,
    target: TranscriptionTarget,
    context: ContextSnapshot,
    print: Option<crate::control::PrintReply>,
    backend: DictationBackend,
    stream: Option<crate::transcription::engine::LiveStream>,
    remote_stream: Option<remote::Capture>,
    heard: Option<String>,
    fallback_heard: String,
}

pub(super) enum Transcript {
    Local {
        pending: PendingTranscript,
        unconfigured: bool,
    },
    Remote {
        pending: Option<Pending>,
        clip: RetryClip,
    },
    Heard(String),
    Unavailable(RetryClip),
}

impl Transcript {
    fn wait(self, control: &JobControl) -> Result<Transcribed, String> {
        match self {
            Self::Local {
                pending,
                unconfigured,
            } => pending.wait().map_err(|error| {
                if unconfigured {
                    return remote::failure_message(FailureReason::NotSetUp).into();
                }
                error
            }),
            Self::Unavailable(clip) => {
                Err(failed_clip(clip, FailureReason::NotSetUp, String::new()))
            }
            Self::Heard(text) => Ok(Transcribed {
                text,
                source: crate::transcription::engine::Source::VoiceBuddy,
                finish_ms: 0,
            }),
            Self::Remote { pending, clip } => {
                let Some(pending) = pending else {
                    return Err(failed_clip(clip, FailureReason::NotSetUp, String::new()));
                };
                let cancel = pending.canceller();
                control.set_remote(cancel.clone());
                let result = pending.wait(&clip.samples, &cancel, || control.is_cancelled());
                control
                    .remote
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                if control.is_cancelled() {
                    return Err("dictation was cancelled".into());
                }
                result.map_err(|failed| {
                    failed_clip(clip, remote::failure_reason(&failed.failure), failed.heard)
                })
            }
        }
    }
}

fn failed_clip(mut clip: RetryClip, reason: FailureReason, heard: String) -> String {
    let heard = if heard.is_empty() {
        std::mem::take(&mut clip.fallback_heard)
    } else {
        heard
    };
    clip.fallback_heard = String::new();
    if matches!(
        clip.target,
        TranscriptionTarget::Paste | TranscriptionTarget::RawPaste
    ) {
        remote::hold(clip, reason, heard);
    }
    remote::failure_message(reason).into()
}

fn needs_setup(engine: Option<&TranscriptionEngine>) -> bool {
    if remote::chosen_backend() != DictationBackend::VoiceBuddy
        || remote::current_backend() != DictationBackend::Local
    {
        return false;
    }
    let selection = crate::app_settings::transcription_selection().1;
    if engine.is_some_and(|engine| engine.holds(&selection)) {
        return false;
    }
    !crate::transcription::models::validate(&selection)
        .is_ok_and(|model| crate::transcription::models::is_installed(model, &selection.language))
}

impl JobControl {
    fn set_remote(&self, cancel: voice_buddy::Canceller) {
        let mut token = self.remote.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_cancelled() {
            cancel.cancel();
        }
        *token = Some(cancel);
    }
    pub(super) fn cancel_remote(&self) {
        if !self.remote_job {
            return;
        }
        self.cancelled.store(true, Ordering::Release);
        if let Some(cancel) = &*self.remote.lock().unwrap_or_else(|e| e.into_inner()) {
            cancel.cancel();
        }
    }
}

impl DictationWorker {
    pub(super) fn submit(
        &self,
        mut clip: DictationClip,
        target: TranscriptionTarget,
        context: ContextSnapshot,
        print: Option<crate::control::PrintReply>,
    ) -> Result<DictationJobId, &'static str> {
        let captured_backend = clip.capture_backend();
        let stream = clip.take_stream();
        let remote_stream = clip.take_voice_buddy();
        let backend = if let Some(backend) = captured_backend {
            backend
        } else if remote_stream.is_some() {
            DictationBackend::VoiceBuddy
        } else if stream.is_some() {
            DictationBackend::Local
        } else {
            remote::current_backend()
        };
        let _capture = clip.take_capture_marker();
        let audio_ms = clip.duration_ms();
        let samples = clip.into_transcription_samples();
        self.submit_samples(Submission {
            samples,
            audio_ms,
            target,
            context,
            print,
            backend,
            stream,
            remote_stream,
            heard: None,
            fallback_heard: String::new(),
        })
    }

    fn submit_samples(&self, work: Submission) -> Result<DictationJobId, &'static str> {
        let Submission {
            samples,
            audio_ms,
            target,
            context,
            print,
            backend,
            stream,
            remote_stream,
            heard,
            fallback_heard,
        } = work;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let job_id = DictationJobId(state.next_output_sequence()?);
        let Some(jobs) = &self.inference_jobs else {
            return Err("dictation worker is unavailable");
        };
        let control = Arc::new(JobControl {
            print: Mutex::new(print),
            remote_job: backend == DictationBackend::VoiceBuddy,
            ..JobControl::default()
        });
        let submitted_at = Instant::now();
        let recording = if heard.is_some() || matches!(target, TranscriptionTarget::Service) {
            None
        } else {
            crate::history::Recording::keep(&samples)
        };
        let transcript = if let Some(text) = heard {
            Transcript::Heard(text)
        } else if backend == DictationBackend::VoiceBuddy {
            let pending = remote_stream
                .map(|capture| capture.finish(&samples))
                .or_else(Pending::whole);
            if let Some(pending) = &pending {
                control.set_remote(pending.canceller());
            }
            Transcript::Remote {
                pending,
                clip: RetryClip {
                    samples: samples.into(),
                    audio_ms,
                    target,
                    fallback_heard,
                },
            }
        } else if needs_setup(self.engine.as_deref()) {
            Transcript::Unavailable(RetryClip {
                samples: samples.into(),
                audio_ms,
                target,
                fallback_heard,
            })
        } else {
            let engine = self
                .engine
                .as_ref()
                .ok_or("dictation worker is unavailable")?;
            crate::dictation::diagnostics::persist(&samples);
            let selection = crate::app_settings::transcription_selection().1;
            let offline = {
                let control = control.clone();
                Box::new(move |model: &mut WarmTranscriber, samples| {
                    if control.is_cancelled() {
                        return Err("dictation was cancelled".into());
                    }
                    let _busy = DictationInference::begin();
                    model
                        .activate(&selection)
                        .and_then(|model| model.transcribe(samples))
                        .map_err(|e| e.to_string())
                })
            };
            let pending = engine
                .finish_with(stream, samples, offline)
                .map_err(|_| "dictation worker is unavailable")?;
            Transcript::Local {
                pending,
                unconfigured: remote::chosen_backend() == DictationBackend::VoiceBuddy
                    && remote::current_backend() == DictationBackend::Local,
            }
        };
        let prepare_ms = submitted_at.elapsed().as_millis();
        let busy = control.remote_job.then(DictationInference::begin);
        jobs.try_send(InferenceJob {
            job_id,
            control: control.clone(),
            submitted_at,
            transcript,
            _busy: busy,
            audio_ms,
            prepare_ms,
            target,
            context,
            recording,
        })
        .map_err(queue_error)?;
        state.next_sequence += 1;
        state.jobs.insert(job_id, control);
        Ok(job_id)
    }

    /// The listener admits recovery work through the same bounded pipeline.
    /// A full queue leaves the original clip available for another attempt.
    pub(crate) fn retry_failed(&self, context: &ContextSnapshot) -> Option<DictationJobId> {
        let ready = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .next_output_sequence()
            .is_ok();
        let retry = remote::take_retry(ready)?;
        let local = retry.action == FailedAction::RetryLocal;
        let heard = (retry.action == FailedAction::PasteHeard).then_some(retry.heard);
        let clip = retry.clip;
        let target = if heard.is_some() {
            TranscriptionTarget::RawPaste
        } else {
            clip.target
        };
        let samples = if heard.is_some() {
            Vec::new()
        } else {
            clip.samples.to_vec()
        };
        let backend = if local {
            DictationBackend::Local
        } else {
            DictationBackend::VoiceBuddy
        };
        let job_id = self
            .submit_samples(Submission {
                samples,
                audio_ms: clip.audio_ms,
                target,
                context: context.clone(),
                print: None,
                backend,
                stream: None,
                remote_stream: None,
                heard,
                fallback_heard: clip.fallback_heard,
            })
            .ok()?;
        remote::act_on_failed(retry.id, FailedAction::Dismiss);
        Some(job_id)
    }
}

/// Waits for each accepted job's transcript in submission order, then hands
/// it to processing. The engine thread runs the inference itself.
pub(super) fn run_inference_worker(
    jobs: Receiver<InferenceJob>,
    processor_jobs: &SyncSender<ProcessorJob>,
    output: &SyncSender<OutputJob>,
    events: &mpsc::Sender<WorkerEvent>,
) {
    while let Ok(job) = jobs.recv() {
        if job.control.is_cancelled() {
            let _ = output.send(OutputJob::Cancelled { job_id: job.job_id });
            continue;
        }
        let _busy = DictationInference::begin();
        let _ = events.send(WorkerEvent::Stage {
            job_id: job.job_id,
            stage: DictationJobStage::Transcribing,
        });
        let total_started = job.submitted_at;
        let queue_ms = total_started.elapsed().as_millis();
        let inference_started = Instant::now();
        let audio_ms = job.audio_ms;
        let result = job.transcript.wait(&job.control).map(|transcribed| {
            tracing::info!(
                source = transcribed.source.label(),
                finish_ms = transcribed.finish_ms,
                audio_ms,
                "transcribed dictation"
            );
            let text = transcribed.text;
            let corrected = if matches!(job.target, TranscriptionTarget::Service) {
                text.clone()
            } else {
                text.trim().to_string()
            };
            tracing::debug!(
                text_bytes = corrected.len(),
                "prepared dictation transcript"
            );
            corrected
        });
        let timings = JobTimings {
            total_started,
            queue_ms,
            audio_ms,
            prepare_ms: job.prepare_ms,
            inference_ms: inference_started.elapsed().as_millis(),
        };
        if job.control.is_cancelled() {
            let _ = output.send(OutputJob::Cancelled { job_id: job.job_id });
            continue;
        }
        let target = job.target;
        let sent = match result {
            Ok(text)
                if matches!(
                    target,
                    TranscriptionTarget::Paste | TranscriptionTarget::Print
                ) && !text.trim().is_empty() =>
            {
                processor_jobs
                    .send(ProcessorJob {
                        job_id: job.job_id,
                        control: job.control,
                        target,
                        text,
                        context: job.context,
                        timings,
                        recording: job.recording,
                    })
                    .is_ok()
            }
            result => {
                let application = job.context.application;
                let recording = job.recording;
                let result = result.map(|text| CompletedTranscript {
                    raw: text.clone(),
                    text,
                    application,
                    timings,
                    processing: None,
                    recording,
                });
                output
                    .send(OutputJob::Completed {
                        job_id: job.job_id,
                        control: job.control,
                        target,
                        result: Box::new(result),
                    })
                    .is_ok()
            }
        };
        if !sent {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_paste_targets_hold_failed_audio_and_the_error_never_contains_heard_words() {
        remote::clear_failed();
        for target in [
            TranscriptionTarget::Print,
            TranscriptionTarget::RawPrint,
            TranscriptionTarget::Service,
            TranscriptionTarget::Paste,
            TranscriptionTarget::RawPaste,
        ] {
            let clip = RetryClip {
                samples: vec![0.1; 160].into(),
                audio_ms: 10,
                target,
                fallback_heard: String::new(),
            };
            let error = failed_clip(clip, FailureReason::TimedOut, "private heard words".into());
            assert!(!error.contains("private heard words"));
        }
        let held = remote::failed();
        assert_eq!(held.len(), 2);
        assert!(
            held.iter()
                .all(|entry| entry.reason == FailureReason::TimedOut && entry.audio_ms == 10)
        );
        assert_eq!(held[0].heard, "private heard words");
        assert!(remote::retry_newest());
        let retry = remote::take_retry(true).unwrap();
        assert_eq!(retry.id, held[1].id);
        assert_eq!(retry.action, FailedAction::Retry);
        assert_eq!(retry.clip.samples.len(), 160);
        remote::clear_failed();
        assert!(remote::failed().is_empty());
    }
    #[test]
    fn an_older_slow_voice_buddy_job_pastes_before_a_newer_finished_job() {
        use crate::dictation::live_text::{self, Phase};
        use std::time::Duration;
        let config = remote::tests::configuration();
        let (entered, waiting) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let released = Mutex::new(released);
        live_text::watch(move |line| {
            if line.is_some_and(|line| {
                line.phase == Phase::Polishing && line.partial.stable == "older"
            }) {
                entered.send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
            }
        });
        let job = |id, text: &str, updates| {
            let session = voice_buddy::Session::scripted(updates, Ok(text.into()));
            let capture =
                remote::Capture::new(session, config.clone(), voice_buddy::Options::default());
            InferenceJob {
                job_id: DictationJobId(id),
                control: Arc::new(JobControl {
                    remote_job: true,
                    ..Default::default()
                }),
                submitted_at: Instant::now(),
                transcript: Transcript::Remote {
                    pending: Some(capture.finish(&[])),
                    clip: RetryClip {
                        samples: Arc::from([]),
                        audio_ms: 0,
                        target: TranscriptionTarget::RawPaste,
                        fallback_heard: String::new(),
                    },
                },
                _busy: None,
                audio_ms: 0,
                prepare_ms: 0,
                target: TranscriptionTarget::RawPaste,
                context: ContextSnapshot::default(),
                recording: None,
            }
        };
        let (send, jobs) = mpsc::sync_channel(2);
        let (processor, _processing) = mpsc::sync_channel(2);
        let (output, results) = mpsc::sync_channel(2);
        let (events, _events) = mpsc::channel();
        send.send(job(
            0,
            "older final",
            vec![voice_buddy::Update {
                text: "older".into(),
                polishing: true,
            }],
        ))
        .unwrap();
        let worker =
            thread::spawn(move || run_inference_worker(jobs, &processor, &output, &events));
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        send.send(job(1, "newer final", Vec::new())).unwrap();
        assert!(matches!(results.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release.send(()).unwrap();
        drop(send);
        let mut pasted = Vec::new();
        let mut last = None;
        for id in [0, 1] {
            let output = results.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(output.sequence(), id);
            let event = finish_output(
                output,
                &mut |text, _, commit| {
                    assert!(commit());
                    pasted.push(text.to_string());
                    Ok(())
                },
                &mut last,
                None,
            );
            assert!(matches!(
                event,
                WorkerEvent::Completed {
                    result: Ok(_),
                    remote: true,
                    ..
                }
            ));
        }
        worker.join().unwrap();
        assert_eq!(pasted, ["older final", "newer final"]);
        assert!(live_text::current().is_none());
    }
    #[test]
    fn a_streamed_service_clip_submits_without_a_local_engine_and_reaches_output() {
        let (jobs, receiver) = mpsc::sync_channel(2);
        let (event_sender, events) = mpsc::channel();
        let mut worker = DictationWorker {
            engine: None,
            inference_jobs: Some(jobs),
            output_jobs: None,
            events,
            event_sender,
            state: Arc::new(Mutex::new(WorkerState {
                next_sequence: 0,
                jobs: BTreeMap::new(),
                pending_pastes: 0,
            })),
            inference_worker: None,
            processor_workers: Vec::new(),
            output_worker: None,
        };
        let session = voice_buddy::Session::scripted(Vec::new(), Ok("final heard text".into()));
        let capture = remote::Capture::new(
            session,
            remote::tests::configuration(),
            voice_buddy::Options::default(),
        );
        let clip = DictationClip::for_test(vec![0.1; 320]).with_voice_buddy_for_test(capture);
        let id = worker
            .transcribe(
                clip,
                TranscriptionTarget::RawPaste,
                ContextSnapshot::default(),
            )
            .unwrap();
        assert!(
            dictation_busy(),
            "an admitted service job is busy before its waiter starts"
        );
        worker.inference_jobs.take();
        let (processor, _processing) = mpsc::sync_channel(2);
        let (output, results) = mpsc::sync_channel(2);
        run_inference_worker(receiver, &processor, &output, &worker.event_sender);
        assert!(
            !dictation_busy(),
            "the completed wait does not keep the engine busy through paste"
        );
        let mut pasted = Vec::new();
        let event = finish_output(
            results.recv().unwrap(),
            &mut |text, _, commit| {
                assert!(commit());
                pasted.push(text.to_string());
                Ok(())
            },
            &mut None,
            None,
        );
        assert!(
            matches!(event, WorkerEvent::Completed { job_id, result: Ok(_), remote: true, .. } if job_id == id)
        );
        assert_eq!(pasted, ["final heard text"]);
    }
    #[test]
    fn a_failed_automatic_retry_holds_one_clip_with_its_last_heard_text() {
        remote::clear_failed();
        let attempts = std::cell::Cell::new(0);
        let result = remote::retry_once(
            Err(voice_buddy::Failed {
                failure: voice_buddy::Failure::TimedOut,
                heard: "heard before disconnect".into(),
            }),
            || false,
            || {
                attempts.set(attempts.get() + 1);
                Err(voice_buddy::Failed {
                    failure: voice_buddy::Failure::Connection("offline".into()),
                    heard: String::new(),
                })
            },
        );
        let failed = result.unwrap_err();
        let clip = RetryClip {
            samples: vec![0.1; 320].into(),
            audio_ms: 20,
            target: TranscriptionTarget::Paste,
            fallback_heard: String::new(),
        };
        failed_clip(clip, remote::failure_reason(&failed.failure), failed.heard);
        let held = remote::failed();
        assert_eq!(attempts.get(), 1);
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].reason, FailureReason::Connection);
        assert_eq!(held[0].heard, "heard before disconnect");
        remote::clear_failed();
    }

    #[test]
    fn job_cancellation_reaches_a_waiting_service_session_and_discards_its_late_final() {
        let pending = voice_buddy::Session::scripted(
            vec![voice_buddy::Update {
                text: "partial".into(),
                polishing: false,
            }],
            Ok("late final".into()),
        )
        .finish();
        let control = JobControl {
            remote_job: true,
            ..Default::default()
        };
        control.set_remote(pending.canceller());
        let result = pending.wait(|_| control.cancel_remote());
        assert!(control.is_cancelled());
        assert_eq!(
            result.unwrap_err().failure,
            voice_buddy::Failure::Internal("cancelled".into())
        );
    }
    #[test]
    fn a_manual_retry_that_hears_nothing_preserves_the_previous_hypothesis() {
        remote::clear_failed();
        let clip = RetryClip {
            samples: vec![0.1; 320].into(),
            audio_ms: 20,
            target: TranscriptionTarget::Paste,
            fallback_heard: "previously heard".into(),
        };
        failed_clip(clip, FailureReason::NotSetUp, String::new());
        assert_eq!(remote::failed()[0].heard, "previously heard");
        remote::clear_failed();
    }
    #[test]
    fn missing_service_setup_without_a_runnable_local_model_is_a_recoverable_failure() {
        remote::clear_failed();
        let clip = RetryClip {
            samples: vec![0.1; 320].into(),
            audio_ms: 20,
            target: TranscriptionTarget::Paste,
            fallback_heard: String::new(),
        };
        let result = Transcript::Unavailable(clip).wait(&JobControl::default());
        assert_eq!(
            result.unwrap_err(),
            remote::failure_message(FailureReason::NotSetUp)
        );
        assert_eq!(remote::failed()[0].reason, FailureReason::NotSetUp);
        remote::clear_failed();
    }
}
