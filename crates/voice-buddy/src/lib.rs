//! Client for the VoiceBuddy speech service, one streamed recording per
//! [`Session`].
//!
//! Every session connects, performs a fresh key exchange, streams the audio
//! it is fed as Opus, and waits for the service's final (optionally polished)
//! text. Audio fed while the connection is still opening is kept, so a caller
//! starts a session and feeds it at once. Nothing here blocks the caller's
//! audio thread, reads the microphone, or writes to disk.
//!
//! Credentials come only from the [`Config`] the caller supplies. They are
//! never logged, and `Debug` redacts them.

use std::fmt;
use std::sync::Arc;

mod codec;
mod config;
mod crypto;
mod network;
mod session;
mod wire;

#[cfg(test)]
mod tests;

pub use session::{Canceller, Pending, Session};

/// A validated service configuration: what `context.json` holds.
#[derive(Clone)]
pub struct Config {
    durable: serde_json::Map<String, serde_json::Value>,
    options: Options,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Config(redacted)")
    }
}

/// Why a configuration file can't be used. Names the problem and the field,
/// never its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Parses and strictly validates `context.json` bytes (bounded, unknown
    /// keys ignored, `uin` as a string or number).
    pub fn parse(bytes: &[u8]) -> Result<Self, ConfigError> {
        config::parse(bytes)
    }

    /// The durable fields as JSON, for the caller to store privately.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(&self.durable).unwrap_or_default()
    }

    /// The service host, to tell the person where their audio goes.
    pub fn host(&self) -> &str {
        self.text("host")
    }

    /// The voice options the file carried, or the defaults.
    pub fn options(&self) -> Options {
        self.options
    }
}

/// Per-session service options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Ask the service to polish the final text.
    pub polish: bool,
    pub traditional: bool,
    pub paragraph: bool,
    pub reduce_redundancy: bool,
    /// How long, after the audio ends, the service may take to answer before
    /// polishing starts.
    pub final_deadline_ms: u32,
    /// How long polishing may take once it has started.
    pub polish_deadline_ms: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            polish: true,
            traditional: false,
            paragraph: false,
            reduce_redundancy: false,
            final_deadline_ms: 1_500,
            polish_deadline_ms: 6_000,
        }
    }
}

/// The text so far. Provisional until the session's final text.
#[derive(Clone, PartialEq, Eq)]
pub struct Update {
    pub text: String,
    /// The audio has ended and the service is polishing the text.
    pub polishing: bool,
}

/// Why a session produced no final text. Messages name the failure, never
/// configuration values or transcript text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// DNS, TLS, or the socket failed or closed.
    Connection(String),
    /// No final text before the deadline.
    TimedOut,
    /// The service refused this identity: retrying the same configuration
    /// can't help, the person must import another.
    Rejected(i64),
    /// The service reported an error for this session.
    Service(i64),
    /// A malformed response, or a local encoder or buffer failure.
    Internal(String),
}

impl Failure {
    /// Whether the same audio may succeed if sent again.
    pub fn retryable(&self) -> bool {
        !matches!(self, Self::Rejected(_))
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(why) => write!(f, "connection failed: {why}"),
            Self::TimedOut => f.write_str("the service did not answer in time"),
            Self::Rejected(code) => write!(f, "the service refused this configuration ({code})"),
            Self::Service(code) => write!(f, "the service reported error {code}"),
            Self::Internal(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Failure {}

/// A finished session without its final text, and the last text it had
/// shown, which a caller may offer instead.
#[derive(Clone, PartialEq, Eq)]
pub struct Failed {
    pub failure: Failure,
    pub heard: String,
}

/// Sends a whole clip through a fresh session. Uses the fastest upload pace
/// verified against the speech service; cancellation is checked throughout.
pub fn transcribe(
    config: Arc<Config>,
    options: Options,
    samples: &[f32],
    cancel: Option<&Canceller>,
    on_update: impl FnMut(&Update),
) -> Result<String, Failed> {
    session::transcribe(config, options, samples, cancel, on_update)
}

/// Connects and completes the key exchange without sending audio, to check a
/// configuration during setup.
pub fn check(config: &Config) -> Result<(), Failure> {
    session::check(config)
}

impl fmt::Debug for Update {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Update")
            .field("text", &"redacted")
            .field("polishing", &self.polishing)
            .finish()
    }
}
impl fmt::Debug for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Failed")
            .field("failure", &self.failure)
            .field("heard", &"redacted")
            .finish()
    }
}
