//! Bounded, volatile recovery. No file, History entry or clipboard write here.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::super::backend::{FailedAction, FailedDictation, FailureReason};
use super::lock;
use crate::parakeet::TranscriptionTarget;

const MAX_ENTRIES: usize = 8;
const MAX_AUDIO_BYTES: usize = 128 * 1024 * 1024;
const MAX_HEARD_BYTES: usize = 64 * 1024;
const EXPIRY: Duration = Duration::from_secs(10 * 60);
type Watcher = Arc<dyn Fn(&[FailedDictation]) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct RetryClip {
    pub samples: Arc<[f32]>,
    pub audio_ms: u64,
    pub target: TranscriptionTarget,
    /// A manual retry that hears nothing still offers the previous hypothesis.
    pub fallback_heard: String,
}

pub(crate) struct Retry {
    pub id: u64,
    pub action: FailedAction,
    pub clip: RetryClip,
    pub heard: String,
}

struct Entry {
    summary: FailedDictation,
    clip: RetryClip,
    expires: Instant,
}

#[derive(Default)]
struct Board {
    next_id: u64,
    entries: VecDeque<Entry>,
    requests: VecDeque<(u64, FailedAction)>,
    watchers: Vec<Watcher>,
}

impl Board {
    fn summaries(&self) -> Vec<FailedDictation> {
        self.entries
            .iter()
            .map(|entry| entry.summary.clone())
            .collect()
    }

    fn expire(&mut self, now: Instant) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| now < entry.expires);
        self.requests
            .retain(|(id, _)| self.entries.iter().any(|entry| entry.summary.id == *id));
        before != self.entries.len()
    }

    fn hold(&mut self, clip: RetryClip, reason: FailureReason, mut heard: String, now: Instant) {
        self.expire(now);
        let bytes = std::mem::size_of_val(clip.samples.as_ref());
        if bytes > MAX_AUDIO_BYTES {
            return;
        }
        while self.entries.len() >= MAX_ENTRIES || self.audio_bytes() + bytes > MAX_AUDIO_BYTES {
            self.entries.pop_front();
        }
        self.requests
            .retain(|(id, _)| self.entries.iter().any(|entry| entry.summary.id == *id));
        let mut end = heard.len().min(MAX_HEARD_BYTES);
        while !heard.is_char_boundary(end) {
            end -= 1;
        }
        heard.truncate(end);
        self.next_id += 1;
        self.entries.push_back(Entry {
            summary: FailedDictation {
                id: self.next_id,
                audio_ms: clip.audio_ms,
                reason,
                heard,
            },
            clip,
            expires: now + EXPIRY,
        });
    }

    fn audio_bytes(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| std::mem::size_of_val(entry.clip.samples.as_ref()))
            .sum()
    }

    fn act(&mut self, id: u64, action: FailedAction) -> bool {
        let Some(entry) = self.entries.iter().find(|entry| entry.summary.id == id) else {
            return false;
        };
        if action == FailedAction::Dismiss {
            self.entries.retain(|entry| entry.summary.id != id);
            self.requests.retain(|(requested, _)| *requested != id);
            return true;
        }
        if action == FailedAction::PasteHeard && entry.summary.heard.trim().is_empty() {
            return false;
        }
        if !self.requests.iter().any(|(requested, _)| *requested == id) {
            self.requests.push_back((id, action));
        }
        true
    }

    fn take_retry(&mut self) -> Option<Retry> {
        let (id, action) = self.requests.pop_front()?;
        let entry = self.entries.iter().find(|entry| entry.summary.id == id)?;
        Some(Retry {
            id,
            action,
            clip: RetryClip {
                fallback_heard: entry.summary.heard.clone(),
                ..entry.clip.clone()
            },
            heard: entry.summary.heard.clone(),
        })
    }
}

fn board() -> &'static Mutex<Board> {
    static BOARD: OnceLock<Mutex<Board>> = OnceLock::new();
    BOARD.get_or_init(Default::default)
}

/// Notify outside the store lock: a UI watcher may read the list again.
fn change<T>(f: impl FnOnce(&mut Board) -> (T, bool)) -> T {
    let (result, notification) = {
        let mut board = lock(board());
        let expired = board.expire(Instant::now());
        let (result, changed) = f(&mut board);
        (
            result,
            (changed || expired).then(|| (board.watchers.clone(), board.summaries())),
        )
    };
    if let Some((watchers, summaries)) = notification {
        for watcher in watchers {
            watcher(&summaries);
        }
    }
    result
}

pub(super) fn list() -> Vec<FailedDictation> {
    change(|board| (board.summaries(), false))
}
pub(super) fn watch(watcher: impl Fn(&[FailedDictation]) + Send + Sync + 'static) {
    let watcher: Watcher = Arc::new(watcher);
    let list = change(|board| {
        board.watchers.push(watcher.clone());
        (board.summaries(), false)
    });
    watcher(&list);
}
pub(super) fn hold(clip: RetryClip, reason: FailureReason, heard: String) {
    change(|board| {
        board.hold(clip, reason, heard, Instant::now());
        ((), true)
    });
}
pub(super) fn act(id: u64, action: FailedAction) {
    change(|board| ((), board.act(id, action)));
}
pub(super) fn retry_newest() -> bool {
    change(|board| {
        let accepted = board
            .entries
            .back()
            .map(|entry| entry.summary.id)
            .is_some_and(|id| board.act(id, FailedAction::Retry));
        (accepted, false)
    })
}
pub(super) fn take_retry(ready: bool) -> Option<Retry> {
    change(|board| {
        if !ready {
            return (None, false);
        }
        (board.take_retry(), false)
    })
}
pub(super) fn clear() {
    change(|board| {
        board.entries.clear();
        board.requests.clear();
        ((), true)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn clip() -> RetryClip {
        RetryClip {
            samples: vec![0.1; 160].into(),
            audio_ms: 10,
            target: TranscriptionTarget::Paste,
            fallback_heard: String::new(),
        }
    }
    #[test]
    fn bounds_expiry_and_actions_release_the_held_audio() {
        let now = Instant::now();
        let mut board = Board::default();
        for _ in 0..10 {
            board.hold(clip(), FailureReason::TimedOut, "heard".into(), now);
        }
        assert_eq!(
            board.summaries().iter().map(|e| e.id).collect::<Vec<_>>(),
            (3..=10).collect::<Vec<_>>()
        );
        assert!(board.act(10, FailedAction::RetryLocal));
        assert!(board.act(10, FailedAction::Retry));
        assert_eq!(board.requests.len(), 1);
        let retry = board.take_retry().unwrap();
        assert_eq!(retry.action, FailedAction::RetryLocal);
        assert_eq!(retry.clip.samples.len(), 160);
        assert_eq!(retry.heard, "heard");
        assert!(board.act(10, FailedAction::Dismiss));
        assert_eq!(board.entries.len(), 7);
        assert!(!board.expire(now + EXPIRY - Duration::from_nanos(1)));
        assert!(board.expire(now + EXPIRY));
        assert!(board.entries.is_empty());
        assert!(!board.act(3, FailedAction::Retry));
    }
    #[test]
    fn audio_byte_cap_evicts_oldest_and_empty_heard_cannot_paste() {
        let mut board = Board::default();
        let mut large = clip();
        large.samples = vec![0.; MAX_AUDIO_BYTES / 4 / 2 + 1].into();
        board.hold(
            large.clone(),
            FailureReason::TimedOut,
            "heard".into(),
            Instant::now(),
        );
        board.hold(
            large,
            FailureReason::TimedOut,
            "heard".into(),
            Instant::now(),
        );
        assert_eq!(board.entries.len(), 1);
        assert_eq!(board.entries[0].summary.id, 2);
        board.entries[0].summary.heard.clear();
        assert!(!board.act(2, FailedAction::PasteHeard));
    }
    #[test]
    fn a_full_job_queue_leaves_the_recovery_request_pending() {
        clear();
        hold(clip(), FailureReason::TimedOut, "heard".into());
        assert!(retry_newest());
        assert!(take_retry(false).is_none());
        assert_eq!(list().len(), 1);
        assert_eq!(take_retry(true).unwrap().heard, "heard");
        clear();
    }
}
