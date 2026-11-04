//! macOS runtime choices for the dictation backend; no window pane code.
use super::*;

impl Pump {
    pub(super) fn select_voice_buddy(&mut self) {
        if !matches!(
            crate::dictation::voice_buddy::setup(),
            crate::dictation::backend::Setup::Ready { .. }
        ) {
            show(&self.window, Destination::Settings);
            return;
        }
        self.runtime_cancel();
        if let Ok(mut settings) = AppSettings::load() {
            settings.dictation_backend = crate::dictation::backend::DictationBackend::VoiceBuddy;
            if settings.save().is_ok() {
                let _ = self.runtime.send(Request::ReloadSettings);
            }
        }
        self.model_menu_at = Instant::now();
    }
}

impl Runtime {
    /// A newer backend choice supersedes a model preparation just like a
    /// newer model does. Selecting Local warms through the listener revision.
    pub(super) fn reconcile_backend(&mut self, settings: &AppSettings) {
        use crate::dictation::backend::DictationBackend;
        if settings.dictation_backend != DictationBackend::VoiceBuddy
            || self.settings.dictation_backend == DictationBackend::VoiceBuddy
        {
            return;
        }
        self.manual_choice();
        self.preparation.cancel();
    }
}
