//! Where dictation's speech goes, as plain data every OS's window can draw.
//! Only macOS has VoiceBuddy today (`rune_ui::capability("voice_buddy")`, and
//! [`crate::dictation::voice_buddy`] carries it out), but the panes' scenes
//! render on every OS, so what they show lives here.

use serde::{Deserialize, Serialize};

/// Who turns a dictation's audio into text.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationBackend {
    /// The VoiceBuddy speech service: the audio leaves this computer.
    #[serde(alias = "mouse_voice")]
    VoiceBuddy,
    /// The selected model (`transcription`) on this computer.
    Local,
}

impl Default for DictationBackend {
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            Self::VoiceBuddy
        } else {
            Self::Local
        }
    }
}

/// What the person asks of the service. A configuration's own `voice_options`
/// fill these in once, when it is imported.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct VoiceBuddyPreferences {
    /// The service polishes the final text.
    pub polish: bool,
    /// The text comes back in Traditional Chinese.
    pub traditional: bool,
    /// The service breaks long text into paragraphs.
    pub paragraph: bool,
    /// The service removes repetition.
    pub reduce_redundancy: bool,
}

impl Default for VoiceBuddyPreferences {
    fn default() -> Self {
        Self {
            polish: true,
            traditional: false,
            paragraph: false,
            reduce_redundancy: false,
        }
    }
}

/// Whether VoiceBuddy can run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Setup {
    /// No configuration saved.
    #[default]
    Missing,
    /// A valid configuration is saved; the audio goes to `host`.
    Ready { host: String },
    /// The saved configuration can't be used, or the service refused it.
    Broken { reason: String },
}

/// A valid configuration the MouseVoice app saved. Offering it reads that app's
/// file; only the person's choice copies it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FoundConfig {
    pub path: std::path::PathBuf,
    pub host: String,
}

/// Why a dictation has no text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureReason {
    /// The network or the service could not be reached.
    Connection,
    /// The service did not answer in time.
    TimedOut,
    /// The service refused the configuration: another must be imported.
    Rejected,
    /// The service reported an error for this dictation.
    Service,
    /// VoiceBuddy isn't set up and no local model can take over.
    NotSetUp,
}

impl FailureReason {
    /// Whether sending the same audio again may succeed.
    pub fn retryable(self) -> bool {
        matches!(self, Self::Connection | Self::TimedOut | Self::Service)
    }
}

/// A dictation that failed after its automatic retry. Its audio is held in
/// memory only, until it is retried, dismissed, or expires.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailedDictation {
    pub id: u64,
    pub audio_ms: u64,
    pub reason: FailureReason,
    /// The last text the service showed, which may be pasted instead. Empty
    /// when it showed none.
    pub heard: String,
}

/// What the person chose for a failed dictation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailedAction {
    /// Send it to the service again.
    Retry,
    /// Transcribe it with the local model instead.
    RetryLocal,
    /// Paste the text the service last showed.
    PasteHeard,
    /// Forget it and its audio.
    Dismiss,
}
