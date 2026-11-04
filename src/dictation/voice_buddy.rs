//! VoiceBuddy configuration, capture sessions and failed dictations on macOS.
mod capture;
mod failed;
mod store;

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use voice_buddy::{Config, Options};

use super::backend::{
    DictationBackend, FailedAction, FailedDictation, FailureReason, FoundConfig, Setup,
    VoiceBuddyPreferences,
};
use crate::app_settings::AppSettings;

#[cfg(test)]
pub(crate) use capture::retry_once;
pub(crate) use capture::{Capture, Pending};
pub(crate) use failed::{Retry, RetryClip};
pub(crate) fn failure_reason(failure: &voice_buddy::Failure) -> FailureReason {
    reason(failure)
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|error| error.into_inner())
}

fn store() -> &'static Mutex<store::Store> {
    static STORE: OnceLock<Mutex<store::Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(store::Store::load(store_path())))
}

// Serializes durable changes without holding the cache lock the audio owner
// reads. A slow fsync must never stall microphone capture.
static STORE_WRITES: Mutex<()> = Mutex::new(());

#[cfg(test)]
fn store_path() -> std::path::PathBuf {
    // Caller tests never read imported credentials, even if their shell has
    // HEX_APPLICATION_SUPPORT_DIR set. Tests install synthetic stores explicitly.
    std::env::temp_dir()
        .join(format!("hex-mouse-runtime-{}", std::process::id()))
        .join("voice-buddy.json")
}

#[cfg(not(test))]
fn store_path() -> std::path::PathBuf {
    crate::app_paths::voice_buddy_file().unwrap_or_default()
}

#[derive(Clone, Copy, Default)]
struct Preferences {
    backend: DictationBackend,
    voice: VoiceBuddyPreferences,
}

fn preferences() -> &'static Mutex<Preferences> {
    static PREFERENCES: OnceLock<Mutex<Preferences>> = OnceLock::new();
    PREFERENCES.get_or_init(Default::default)
}

pub(crate) fn configure(settings: &AppSettings) {
    *lock(preferences()) = Preferences {
        backend: settings.dictation_backend,
        voice: settings.voice_buddy,
    };
}

/// A saved, usable configuration is the person's consent to send audio.
pub(crate) fn effective_backend(settings: &AppSettings) -> DictationBackend {
    effective(settings.dictation_backend, &setup())
}

fn effective(chosen: DictationBackend, setup: &Setup) -> DictationBackend {
    if chosen == DictationBackend::VoiceBuddy && matches!(setup, Setup::Ready { .. }) {
        return DictationBackend::VoiceBuddy;
    }
    DictationBackend::Local
}

pub(crate) fn current_backend() -> DictationBackend {
    effective(lock(preferences()).backend, &setup())
}

pub(crate) fn chosen_backend() -> DictationBackend {
    lock(preferences()).backend
}

pub(crate) fn setup() -> Setup {
    lock(store()).setup.clone()
}

pub(crate) fn found() -> Vec<FoundConfig> {
    crate::app_paths::home_dir()
        .map(|home| store::found_in(&home))
        .unwrap_or_default()
}

/// Failed validation or saving preserves the last usable configuration.
pub(crate) fn import(path: &Path) -> Result<Setup, String> {
    let _write = lock(&STORE_WRITES);
    let previous_backend = current_backend();
    let (config, _) = store::read_config(path)?;
    let options = config.options();
    let previous = AppSettings::load().map_err(|_| "Could not read settings")?;
    let mut settings = previous.clone();
    settings.voice_buddy = VoiceBuddyPreferences {
        polish: options.polish,
        traditional: options.traditional,
        paragraph: options.paragraph,
        reduce_redundancy: options.reduce_redundancy,
    };
    settings
        .save()
        .map_err(|_| "Could not save voice options")?;
    let mut candidate = lock(store()).clone();
    if let Err(error) = candidate.import_config(config) {
        if previous.save().is_err() {
            tracing::warn!("could not restore voice options after a failed import");
        }
        return Err(error);
    }
    *lock(store()) = candidate;
    if previous_backend == DictationBackend::Local
        && current_backend() == DictationBackend::VoiceBuddy
        && let Some(runtime) = crate::macos_rune::runtime()
    {
        let _ = runtime.send(crate::macos_rune::Request::CancelModel);
    }
    Ok(setup())
}

pub(crate) fn forget() -> Result<(), String> {
    let _write = lock(&STORE_WRITES);
    let mut candidate = lock(store()).clone();
    candidate.forget()?;
    *lock(store()) = candidate;
    Ok(())
}

pub(crate) fn check_connection() -> Result<(), FailureReason> {
    let config = config().ok_or(FailureReason::NotSetUp)?;
    voice_buddy::check(&config).map_err(|error| {
        reject(&config, &error);
        persist_rejection(&config, &error);
        reason(&error)
    })
}

pub(crate) fn failed() -> Vec<FailedDictation> {
    failed::list()
}
pub(crate) fn watch_failed(watcher: impl Fn(&[FailedDictation]) + Send + Sync + 'static) {
    failed::watch(watcher);
}
pub(crate) fn act_on_failed(id: u64, action: FailedAction) {
    failed::act(id, action);
}
pub(crate) fn retry_newest() -> bool {
    failed::retry_newest()
}
pub(crate) fn take_retry(ready: bool) -> Option<Retry> {
    failed::take_retry(ready)
}
pub(crate) fn hold(clip: RetryClip, reason: FailureReason, heard: String) {
    failed::hold(clip, reason, heard);
}
pub(crate) fn clear_failed() {
    failed::clear();
}

fn config() -> Option<Arc<Config>> {
    lock(store()).usable()
}
fn options(config: &Config) -> Options {
    let voice = lock(preferences()).voice;
    Options {
        polish: voice.polish,
        traditional: voice.traditional,
        paragraph: voice.paragraph,
        reduce_redundancy: voice.reduce_redundancy,
        ..config.options()
    }
}
fn reject(config: &Arc<Config>, failure: &voice_buddy::Failure) {
    if matches!(failure, voice_buddy::Failure::Rejected(_)) {
        lock(store()).reject(config);
    }
}
fn persist_rejection(config: &Arc<Config>, failure: &voice_buddy::Failure) {
    if matches!(failure, voice_buddy::Failure::Rejected(_)) {
        let _write = lock(&STORE_WRITES);
        let snapshot = lock(store()).clone();
        snapshot.persist_rejection(config);
    }
}
fn reason(failure: &voice_buddy::Failure) -> FailureReason {
    match failure {
        voice_buddy::Failure::Connection(_) => FailureReason::Connection,
        voice_buddy::Failure::TimedOut => FailureReason::TimedOut,
        voice_buddy::Failure::Rejected(_) => FailureReason::Rejected,
        voice_buddy::Failure::Service(_) | voice_buddy::Failure::Internal(_) => {
            FailureReason::Service
        }
    }
}

pub(crate) fn failure_message(reason: FailureReason) -> &'static str {
    match reason {
        FailureReason::Connection => "Could not connect to the speech service",
        FailureReason::TimedOut => "The speech service did not answer in time",
        FailureReason::Rejected => "The speech service refused this configuration",
        FailureReason::Service => "The speech service could not transcribe this dictation",
        FailureReason::NotSetUp => "VoiceBuddy is not set up and no local model is ready",
    }
}

#[cfg(test)]
pub(crate) mod tests;
