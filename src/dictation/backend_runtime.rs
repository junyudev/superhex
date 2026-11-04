//! macOS runtime choices for the dictation backend; no window pane code.
use super::*;

impl Pump {
    pub(super) fn select_mouse_voice(&mut self) {
        if !matches!(
            crate::dictation::mouse_voice::setup(),
            crate::dictation::backend::Setup::Ready { .. }
        ) {
            show(&self.window, Destination::Settings);
            return;
        }
        self.runtime_cancel();
        if let Ok(mut settings) = AppSettings::load() {
            settings.dictation_backend = crate::dictation::backend::DictationBackend::MouseVoice;
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
        if settings.dictation_backend != DictationBackend::MouseVoice
            || self.settings.dictation_backend == DictationBackend::MouseVoice
        {
            return;
        }
        self.manual_choice();
        self.preparation.cancel();
    }
}
