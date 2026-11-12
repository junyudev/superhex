//! Programmatic dictations (the local API and `hex` control commands that
//! drive one): ending a capture the microphone cut short, and cancelling one.

use std::collections::VecDeque;

use color_eyre::Result;

use crate::dictation::DictationClip;
use crate::dictation::audio::DictationAudio;
use crate::dictation::service::{
    Completion, MICROPHONE_LOST, TerminalOutcome, remember_completion,
};

use super::{LostProgrammaticDictation, ProgrammaticDictation};

pub(super) fn log_programmatic_ended(active: Option<ProgrammaticDictation>, outcome: &str) {
    if let Some(active) = active {
        tracing::warn!(
            dictation_id = active.id,
            source = active.source,
            outcome,
            "programmatic dictation ended"
        );
    }
}

/// Ends a programmatic capture the microphone cut short. The audio delivered
/// before the loss waits for the owner's `/finish`; without any, `/finish`
/// answers `microphone-lost`.
pub(super) fn programmatic_microphone_lost(
    active: Option<ProgrammaticDictation>,
    kept: Option<DictationClip>,
    outcome: &str,
    lost: &mut Option<LostProgrammaticDictation>,
    completions: &mut VecDeque<Completion>,
) {
    let Some(active) = active else {
        return;
    };
    let Some(mut clip) = kept else {
        remember_completion(
            completions,
            Completion {
                id: active.id,
                owner_token: active.owner_token.clone(),
                outcome: TerminalOutcome::Finished(Err(MICROPHONE_LOST.into())),
            },
        );
        log_programmatic_ended(Some(active), outcome);
        return;
    };
    // A held clip must not defer background work until the owner finishes.
    drop(clip.take_capture_marker());
    tracing::warn!(
        dictation_id = active.id,
        source = active.source,
        outcome,
        duration_ms = clip.duration_ms(),
        "programmatic dictation kept its audio after a microphone loss"
    );
    let previous = lost.replace(LostProgrammaticDictation {
        id: active.id,
        owner_token: active.owner_token,
        source: active.source,
        lease_deadline: active.lease_deadline,
        clip,
    });
    if let Some(previous) = previous {
        tracing::warn!(
            dictation_id = previous.id,
            source = previous.source,
            outcome = "superseded",
            "programmatic dictation ended"
        );
        remember_completion(
            completions,
            Completion {
                id: previous.id,
                owner_token: previous.owner_token,
                outcome: TerminalOutcome::Finished(Err(MICROPHONE_LOST.into())),
            },
        );
    }
}

pub(super) fn cancel_programmatic_capture(
    active: ProgrammaticDictation,
    input: &DictationAudio,
    completions: &mut VecDeque<Completion>,
) -> Result<()> {
    input.cancel()?;
    remember_completion(
        completions,
        Completion {
            id: active.id,
            owner_token: active.owner_token,
            outcome: TerminalOutcome::Cancelled,
        },
    );
    Ok(())
}
