//! MouseVoice as macOS dictation's backend: the saved configuration, the
//! choice of backend in effect, and the dictations that failed.

use std::path::Path;

use super::backend::{
    DictationBackend, FailedAction, FailedDictation, FailureReason, FoundConfig, Setup,
};
use crate::app_settings::AppSettings;

/// The backend a dictation uses now: MouseVoice only when it is chosen and a
/// usable configuration is saved. A saved configuration is the person's
/// consent that audio leaves this computer, so nothing here ever saves one
/// unasked.
pub(crate) fn effective_backend(_settings: &AppSettings) -> DictationBackend {
    DictationBackend::Local
}

/// The saved configuration's state, cheap enough to read from the window.
pub(crate) fn setup() -> Setup {
    Setup::Missing
}

/// Valid configurations the person's other apps saved. Reads only MouseVoice's
/// and Iris's own files, and only when setup asks.
pub(crate) fn found() -> Vec<FoundConfig> {
    Vec::new()
}

/// Validates the file at `path` and saves a private copy, leaving the original
/// alone. A failed import leaves what was saved.
pub(crate) fn import(_path: &Path) -> Result<Setup, String> {
    Err("not implemented".into())
}

/// Forgets the saved configuration.
pub(crate) fn forget() -> Result<(), String> {
    Ok(())
}

/// Connects and completes the key exchange, sending no audio. Blocks; call it
/// off the window's thread.
pub(crate) fn check_connection() -> Result<(), FailureReason> {
    Err(FailureReason::NotSetUp)
}

/// The failed dictations still held, oldest first.
pub(crate) fn failed() -> Vec<FailedDictation> {
    Vec::new()
}

/// Calls `watcher` with the list on each change, from any thread; it must hand
/// the list to its own thread and never wait.
pub(crate) fn watch_failed(_watcher: impl Fn(&[FailedDictation]) + Send + Sync + 'static) {}

/// Carries out the person's choice for the failed dictation `id`.
pub(crate) fn act_on_failed(_id: u64, _action: FailedAction) {}
