//! What the dictation being recorded has said so far, for the HUDs (the
//! Labs feature live text) and the first run's practice field, which
//! [`demand`]s it while it shows and reads it with [`current`].
//!
//! The capture loop publishes through its recording's [`Publisher`], which
//! the recording owns: dropping the recording (release, a short tap, a lost
//! microphone) clears the text, so no ending can leave it on screen.
//! A HUD registers a [`watch`]er and is told each change from the capture
//! loop's thread; it must hand the text to its own thread and never wait.
//! The text is provisional: it is only ever shown, never pasted or kept.
//!
//! MouseVoice's text outlives the recording: the [`Publisher`] travels with
//! the clip until the final text is ready, because the service keeps
//! rewriting the words after the audio ends. Its [`Phase`] says so.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::transcription::engine::Partial;

type Watcher = Arc<dyn Fn(Option<&Line>) + Send + Sync>;

/// Where the words on show are in their journey. Only a remote speech service
/// has anything but [`Phase::Hearing`] to say.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Phase {
    /// The recording is going on and the words follow it.
    #[default]
    Hearing,
    /// The connection failed while recording: the audio is kept and sent
    /// again once the recording ends.
    Offline,
    /// The recording ended; the final text is on its way.
    Finishing,
    /// The service is polishing the final text.
    Polishing,
}

/// What a HUD shows: the words and where they are in their journey.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Line {
    pub(crate) partial: Partial,
    pub(crate) phase: Phase,
}

struct Board {
    /// The publisher whose text is current, or 0 when none is.
    owner: u64,
    /// The current publisher's latest line.
    line: Option<Line>,
    watchers: Vec<Watcher>,
}

static BOARD: Mutex<Board> = Mutex::new(Board {
    owner: 0,
    line: None,
    watchers: Vec::new(),
});
static NEXT_PUBLISHER: AtomicU64 = AtomicU64::new(1);
static DEMANDS: AtomicUsize = AtomicUsize::new(0);

fn board() -> MutexGuard<'static, Board> {
    BOARD.lock().unwrap_or_else(|error| error.into_inner())
}

/// Calls `watcher` with each new text, and with `None` once the recording
/// that published it ends.
pub(crate) fn watch(watcher: impl Fn(Option<&Line>) + Send + Sync + 'static) {
    board().watchers.push(Arc::new(watcher));
}

/// The text the recording in progress has shown, if any.
pub(crate) fn current() -> Option<Partial> {
    board().line.as_ref().map(|line| line.partial.clone())
}

/// Asks dictation streams to publish their text while it is held, without
/// the experiment (the first run's practice field). The HUD plate still
/// shows only with the experiment.
pub(crate) struct Demand(());

pub(crate) fn demand() -> Demand {
    DEMANDS.fetch_add(1, Ordering::AcqRel);
    Demand(())
}

impl Drop for Demand {
    fn drop(&mut self) {
        DEMANDS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Whether anything holds a [`Demand`].
pub(crate) fn demanded() -> bool {
    DEMANDS.load(Ordering::Acquire) > 0
}

/// One recording's hold on the live text.
pub(crate) struct Publisher {
    id: u64,
}

impl Publisher {
    pub(crate) fn new() -> Self {
        Self {
            id: NEXT_PUBLISHER.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Shows `partial`, replacing any other recording's text.
    pub(crate) fn show(&self, partial: &Partial) {
        self.show_line(Line {
            partial: partial.clone(),
            phase: Phase::Hearing,
        });
    }

    /// Shows `line`, replacing any other recording's. A line in a phase other
    /// than [`Phase::Hearing`] shows even with no words yet.
    pub(crate) fn show_line(&self, line: Line) {
        let watchers = {
            let mut board = board();
            board.owner = self.id;
            board.line = Some(line.clone());
            board.watchers.clone()
        };
        for watcher in watchers {
            watcher(Some(&line));
        }
    }
}

impl Drop for Publisher {
    /// Clears the text if it is still this recording's.
    fn drop(&mut self) {
        let watchers = {
            let mut board = board();
            if board.owner != self.id {
                return;
            }
            board.owner = 0;
            board.line = None;
            board.watchers.clone()
        };
        for watcher in watchers {
            watcher(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The board is process-wide; these tests take turns with it.
    static TURN: Mutex<()> = Mutex::new(());

    fn turn() -> MutexGuard<'static, ()> {
        TURN.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn said(stable: &str) -> Partial {
        Partial {
            stable: stable.into(),
            tentative: String::new(),
        }
    }

    /// Records every call a watcher gets, as the stable text or `None`.
    fn watched() -> Arc<Mutex<Vec<Option<String>>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        watch(move |line| {
            log.lock()
                .unwrap()
                .push(line.map(|line| line.partial.stable.clone()));
        });
        seen
    }

    #[test]
    fn a_recording_shows_its_text_and_clears_it_when_it_ends() {
        let _turn = turn();
        let seen = watched();
        let recording = Publisher::new();
        recording.show(&said("so the plan"));
        recording.show(&said("so the plan for today"));
        assert_eq!(current().unwrap().stable, "so the plan for today");
        drop(recording);
        assert_eq!(current(), None);
        assert_eq!(
            *seen.lock().unwrap(),
            [
                Some("so the plan".into()),
                Some("so the plan for today".into()),
                None
            ]
        );
    }

    #[test]
    fn an_older_recording_ending_never_clears_a_newer_ones_text() {
        let _turn = turn();
        let seen = watched();
        let older = Publisher::new();
        older.show(&said("old"));
        let newer = Publisher::new();
        newer.show(&said("new"));
        drop(older);
        assert_eq!(
            *seen.lock().unwrap(),
            [Some("old".into()), Some("new".into())]
        );
        drop(newer);
        assert_eq!(seen.lock().unwrap().last(), Some(&None));
    }

    #[test]
    fn a_line_keeps_its_phase_for_the_watchers_and_its_words_for_the_practice_field() {
        let _turn = turn();
        let phases = Arc::new(Mutex::new(Vec::new()));
        let log = phases.clone();
        watch(move |line| log.lock().unwrap().push(line.map(|line| line.phase)));
        let recording = Publisher::new();
        recording.show(&said("so the plan"));
        recording.show_line(Line {
            partial: said("so the plan"),
            phase: Phase::Polishing,
        });
        assert_eq!(current().unwrap().stable, "so the plan");
        drop(recording);
        assert_eq!(
            *phases.lock().unwrap(),
            [Some(Phase::Hearing), Some(Phase::Polishing), None]
        );
    }

    #[test]
    fn a_recording_that_never_showed_text_clears_nothing() {
        let _turn = turn();
        let seen = watched();
        drop(Publisher::new());
        assert!(seen.lock().unwrap().is_empty());
    }
}
