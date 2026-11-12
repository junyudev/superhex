//! Acting on a shortcut's capture: starting, finishing, cancelling, or
//! pasting the last dictation for one shortcut press.

use color_eyre::Result;

use crate::audio::CaptureInstant;
use crate::control::PrintReply;
use crate::desktop::events::{DictationPhase, EventLog};
use crate::dictation::POST_RELEASE_TAIL;
use crate::dictation::audio::DictationAudio;
use crate::dictation::context::ContextSnapshot;
use crate::dictation::indicator::DictationIndicatorEvent;
use crate::feedback::{self, Tone};
use crate::parakeet::{DictationWorker, TranscriptionTarget};
use crate::suppression::HotkeyAction;

use super::output::{finish_dictation, finish_dictation_with};
use super::{
    CaptureEnd, ContextWatch, DictationSink, FinishResult, emit_engine_state, emit_state,
    end_capture,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_hotkey_action(
    action: HotkeyAction,
    raw: bool,
    print: Option<PrintReply>,
    action_at: CaptureInstant,
    dictation: &DictationAudio,
    worker: &DictationWorker,
    context: &mut ContextWatch,
    device: &str,
    events: &mut EventLog,
    indicator: DictationSink<'_>,
    release_while_idle: bool,
) -> Result<bool> {
    match action {
        HotkeyAction::Start => {
            if !dictation.start(action_at)? {
                return Ok(false);
            }
            // Labs › Return to app: this paste goes back to the app in front
            // now, even if another one comes forward while the person talks.
            worker.remember_return_to_app();
            // The press started a capture, but it may still be provisional:
            // no page is read until the capture is held past its threshold.
            context.begin();
            // The tone confirms the press at once; a chord or short tap
            // retracts it. A released microphone confirms only once it opens.
            if release_while_idle {
                feedback::prepare();
            } else {
                feedback::play_provisional_start();
            }
            worker.prepare_paste();
            #[cfg(hex_capture)]
            crate::screenshots::narration::started();
            events.dictation(DictationPhase::Started, "")?;
            indicator.send(DictationIndicatorEvent::Started);
            emit_state(events, true, device)
        }
        HotkeyAction::Finish => {
            // The capture's own page, or none rather than the last
            // dictation's.
            let context = context.finish(events)?;
            finish_hotkey_dictation(
                action_at,
                dictation,
                worker,
                &context,
                device,
                events,
                indicator,
                release_while_idle,
                print,
                raw,
            )
            .map(drop)
        }
        HotkeyAction::Discard(_) | HotkeyAction::Cancel | HotkeyAction::CancelShortcut => {
            context.cancel();
            feedback::retract_start();
            #[cfg(hex_capture)]
            crate::screenshots::narration::ended();
            if action == HotkeyAction::CancelShortcut {
                feedback::play(Tone::Cancel);
            }
            end_capture(
                if matches!(action, HotkeyAction::Cancel | HotkeyAction::CancelShortcut) {
                    CaptureEnd::Cancelled
                } else {
                    CaptureEnd::Discarded(action.discard_cause())
                },
                dictation,
                device,
                events,
                indicator,
            )
        }
        HotkeyAction::PasteLast | HotkeyAction::PasteMeeting => {
            // The paste chord's modifier press started a provisional capture.
            context.cancel();
            feedback::retract_start();
            dictation.cancel()?;
            let paste_result = match action {
                HotkeyAction::PasteLast => worker.paste_last(),
                HotkeyAction::PasteMeeting => worker.paste_meeting(),
                _ => unreachable!(),
            };
            if let Err(error) = paste_result {
                feedback::play(Tone::Error);
                events.dictation(DictationPhase::Failed(error.into()), "")?;
            }
            emit_engine_state(events, false, worker, device)
        }
    }?;
    Ok(true)
}

/// Ends a shortcut dictation at `action_at` and hands it to transcription.
#[allow(clippy::too_many_arguments)]
pub(super) fn finish_hotkey_dictation(
    action_at: CaptureInstant,
    dictation: &DictationAudio,
    worker: &DictationWorker,
    context: &ContextSnapshot,
    device: &str,
    events: &mut EventLog,
    indicator: DictationSink<'_>,
    release_while_idle: bool,
    print: Option<PrintReply>,
    raw: bool,
) -> Result<FinishResult> {
    if dictation.become_intentional(action_at)? && release_while_idle {
        feedback::play(Tone::DictationStart);
    }
    // A dictation over a screenshot lands its words there, never pasted; a
    // printing capture stops that shot's editor waiting instead.
    #[cfg(hex_capture)]
    let print = print
        .inspect(|_| crate::screenshots::narration::ended())
        .or_else(crate::screenshots::narration::claim);
    // A discarded or silent clip never reaches transcription, so its waiter
    // still hears from it.
    let unanswered = print.clone();
    let finished = match print {
        None => finish_dictation(
            dictation,
            action_at,
            POST_RELEASE_TAIL,
            worker,
            if raw {
                TranscriptionTarget::RawPaste
            } else {
                TranscriptionTarget::Paste
            },
            context,
            device,
            events,
            indicator,
        )?,
        Some(print) => finish_dictation_with(
            dictation,
            action_at,
            POST_RELEASE_TAIL,
            // Narration lands on its screenshot either way, never pasted.
            |clip, _| worker.transcribe_printed(clip, print, raw, context.clone()),
            device,
            events,
            indicator,
        )?,
    };
    if matches!(finished, FinishResult::Discarded) {
        feedback::retract_start();
    }
    // A discarded or silent clip submitted no job: say so instead of leaving
    // the printing command to its cap.
    if matches!(finished, FinishResult::Discarded | FinishResult::Silent)
        && let Some(print) = unanswered
    {
        print.send(crate::control::PrintedTranscript::NoSpeech);
    }
    emit_engine_state(events, false, worker, device)?;
    Ok(finished)
}
