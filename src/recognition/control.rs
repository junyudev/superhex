//! The `hex` control commands for dictation (`dictation`, `paste-last`):
//! each maps onto the shortcut's own capture path.

use color_eyre::Result;

use crate::control::{CaptureAction, ControlReply, PrintReply};
use crate::desktop::events::{DictationPhase, EventLog};
use crate::dictation::audio::DictationAudio;
use crate::feedback::{self, Tone};
use crate::parakeet::DictationWorker;
use crate::suppression::HotkeyAction;

use super::guards::{capture_boundary, capture_refusal};
use super::hotkey_capture::{finish_hotkey_dictation, handle_hotkey_action};
use super::{ContextWatch, DictationSink, FinishResult, Hotkeys, emit_engine_state};

/// A control start has no press to judge, so its capture counts as
/// intentional from its first sample: the recording environment starts and
/// the live stream opens now. `tone_owed` plays the start tone for a capture
/// that's already open; one still opening the microphone plays it on
/// `ReadyIntentional`.
fn make_control_capture_intentional(input: &DictationAudio, tone_owed: bool) -> Result<()> {
    if input.mark_intentional()? && tone_owed {
        feedback::play(Tone::DictationStart);
    }
    Ok(())
}

/// The reply to a control `stop`: only a submitted clip transcribes.
pub(super) fn finish_reply(finished: Option<FinishResult>, submitted: &str) -> ControlReply {
    match finished {
        Some(FinishResult::Submitted) => ControlReply::done(submitted),
        Some(FinishResult::Discarded) | None => ControlReply::error(
            "discarded",
            "too short, discarded: no audio was recorded yet (the microphone may still have \
             been opening)",
        ),
        Some(FinishResult::Silent) => ControlReply::error(
            "discarded",
            "the recording was silent, discarded; nothing to transcribe",
        ),
        Some(FinishResult::Rejected(error)) => {
            ControlReply::error("failed", format!("couldn't transcribe it: {error}"))
        }
    }
}

/// `superhex dictation`: a start locks the shortcut machine as a double tap
/// does, so the next press or `stop` finishes it. A printing start keeps the
/// command's reply here until that recording ends, however it ends.
#[allow(clippy::too_many_arguments)]
pub(super) fn control_dictation(
    action: CaptureAction,
    print: Option<PrintReply>,
    raw: bool,
    other_recording: bool,
    suspended: bool,
    hotkeys: &mut Hotkeys,
    input: &DictationAudio,
    worker: &DictationWorker,
    context: &mut ContextWatch,
    events: &mut EventLog,
    indicator: DictationSink<'_>,
    release_while_idle: bool,
) -> Result<ControlReply> {
    let recording = hotkeys.any_recording();
    let device = input.device_name();
    let start = match action {
        CaptureAction::Start => true,
        CaptureAction::Stop => false,
        CaptureAction::Toggle => !recording,
        CaptureAction::Cancel if recording => {
            hotkeys.cancel_print();
            hotkeys.suspend_recording();
            handle_hotkey_action(
                HotkeyAction::CancelShortcut,
                false,
                None,
                input.captured_through(),
                input,
                worker,
                context,
                &device,
                events,
                indicator,
                release_while_idle,
            )?;
            return Ok(ControlReply::done("Cancelled the recording"));
        }
        CaptureAction::Cancel => return Ok(ControlReply::unchanged("Dictation isn't recording")),
    };
    Ok(match start {
        true if recording && print.is_some() => ControlReply::error(
            "busy",
            "a dictation is already recording; finish it before starting a print-only one",
        ),
        true if recording => ControlReply::unchanged("Dictation is already recording"),
        true => {
            // A reply still held belongs to a recording that already ended;
            // it must never answer this one.
            hotkeys.fail_print("its recording already ended");
            if let Some(refusal) = capture_refusal(other_recording, suspended) {
                return Ok(refusal);
            }
            if !hotkeys.dictation.lock() {
                return Ok(ControlReply::error(
                    "busy",
                    "the dictation shortcut is held; release it and try again",
                ));
            }
            hotkeys.raw_capture = raw;
            let accepted = handle_hotkey_action(
                HotkeyAction::Start,
                raw,
                None,
                capture_boundary(input, release_while_idle),
                input,
                worker,
                context,
                &device,
                events,
                indicator,
                release_while_idle,
            )?;
            if accepted {
                // A warm start already queued its provisional tone. A control
                // start has no hold threshold to wait for, so its capture is
                // real now and reads its page at once.
                context.intend();
                make_control_capture_intentional(input, release_while_idle)?;
                if let Some(print) = print {
                    // However this recording ends, its text comes back here.
                    hotkeys.keep_print(print);
                    ControlReply::done("Recording; the transcript comes back here when it ends")
                } else {
                    ControlReply::done(
                        "Recording; `superhex dictation stop` or the shortcut finishes",
                    )
                }
            } else {
                hotkeys.dictation.suspend();
                ControlReply::error("busy", "the microphone is busy; try again")
            }
        }
        false if !recording && print.is_some() => ControlReply::error(
            "not-recording",
            "dictation isn't recording; nothing to print",
        ),
        false if !recording => ControlReply::unchanged("Dictation isn't recording"),
        false => {
            // Whether this command prints, read before a print-only
            // recording's start reply joins it: a plain stop answers as a
            // plain stop does.
            let printing = print.is_some();
            let start_printed = hotkeys.print_capture.is_some();
            // A print-only recording answers its start command too: both it
            // and a printing stop hear the one transcript.
            let print = match (hotkeys.print_capture.take(), print) {
                (Some(start), Some(stop)) => Some(start.join(stop)),
                (Some(start), None) => Some(start),
                (None, print) => print,
            };
            hotkeys.suspend_recording();
            // A control capture asked for its page when it started; this
            // waits for that read if it is still in flight.
            let context = context.finish(events)?;
            let finished = finish_hotkey_dictation(
                capture_boundary(input, release_while_idle),
                input,
                worker,
                &context,
                &device,
                events,
                indicator,
                release_while_idle,
                print,
                hotkeys.raw_capture,
            )?;
            match finished {
                FinishResult::Discarded | FinishResult::Silent if printing => {
                    ControlReply::error("no-speech", "no speech was recorded; nothing to print")
                }
                finished if printing => finish_reply(
                    Some(finished),
                    "Finished recording; printing the transcript",
                ),
                finished if start_printed => finish_reply(
                    Some(finished),
                    "Finished recording; the transcript goes to the command that started it",
                ),
                finished => finish_reply(
                    Some(finished),
                    "Finished recording; Superhex pastes the text when it's ready",
                ),
            }
        }
    })
}

/// Pastes the last dictation at the current focus.
pub(super) fn paste_last(
    worker: &DictationWorker,
    device: &str,
    events: &mut EventLog,
) -> Result<ControlReply> {
    let reply = match worker.paste_last() {
        Ok(()) => ControlReply::done("Pasting the last dictation"),
        Err(error) => {
            feedback::play(Tone::Error);
            events.dictation(DictationPhase::Failed(error.into()), "")?;
            ControlReply::error("failed", format!("couldn't paste: {error}"))
        }
    };
    emit_engine_state(events, false, worker, device)?;
    Ok(reply)
}
