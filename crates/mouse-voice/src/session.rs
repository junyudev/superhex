use crate::{
    Config, Failed, Failure, Options, Update,
    codec::Audio,
    crypto,
    network::{self, Endpoint, Network, voice_body},
    wire::{bytes, decode, int, text},
};
use crossbeam_queue::ArrayQueue;
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::{
    sync::{Notify, mpsc as queue},
    time::{Instant, sleep, timeout},
};

const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;
const CHUNK: usize = 1920;
struct Control {
    cancelled: AtomicBool,
    ended: AtomicBool,
    queued: AtomicUsize,
    failure: OnceLock<Failure>,
    updates: ArrayQueue<Update>,
    wake: Notify,
    parent: Option<Arc<Control>>,
}
impl Control {
    fn new(parent: Option<Arc<Self>>) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            queued: AtomicUsize::new(0),
            failure: OnceLock::new(),
            updates: ArrayQueue::new(1),
            wake: Notify::new(),
            parent,
        }
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.wake.notify_one();
    }
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self.parent.as_ref().is_some_and(|p| p.is_cancelled())
    }
    fn fail(&self, failure: Failure) {
        let _ = self.failure.set(failure);
        self.wake.notify_one();
    }
    fn check(&self) -> Result<(), Failure> {
        if self.is_cancelled() {
            return Err(cancelled());
        }
        if let Some(failure) = self.failure.get() {
            return Err(failure.clone());
        }
        Ok(())
    }
    async fn interrupted(&self) -> Failure {
        loop {
            if let Err(failure) = self.check() {
                return failure;
            }
            // The timer also observes a linked caller's token without a helper thread.
            tokio::select! { _ = self.wake.notified() => {}, _ = sleep(Duration::from_millis(10)) => {} }
        }
    }
}

/// Cancels its session from any thread, including during connect or final text.
/// Dropping a cancellation handle also cancels; keep it until the work finishes.
#[derive(Clone)]
pub struct Canceller {
    control: Arc<Control>,
}
impl Default for Canceller {
    fn default() -> Self {
        Self::new()
    }
}
impl Canceller {
    /// Creates an independent cancellation handle for whole-clip transcription.
    /// Clones share its state; cancelling or dropping any clone cancels linked work.
    pub fn new() -> Self {
        Self {
            control: Arc::new(Control::new(None)),
        }
    }
    pub fn cancel(&self) {
        self.control.cancel();
    }
}
impl Drop for Canceller {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// One recording. Drop to cancel; feed never waits for the worker.
pub struct Session {
    inner: Option<Inner>,
    sender: Option<queue::Sender<Vec<i16>>>,
}
struct Inner {
    #[cfg(test)]
    worker: Option<std::thread::JoinHandle<()>>,
    control: Arc<Control>,
    result: mpsc::Receiver<Result<String, Failed>>,
    #[cfg(feature = "testing")]
    script: Option<(std::collections::VecDeque<Update>, Result<String, Failed>)>,
}
impl Session {
    /// Starts a fresh connection and key exchange on its own thread immediately.
    pub fn start(config: Arc<Config>, options: Options) -> Self {
        Self::start_linked(config, options, None, None)
    }
    fn start_linked(
        config: Arc<Config>,
        options: Options,
        parent: Option<Arc<Control>>,
        endpoint: Option<Endpoint>,
    ) -> Self {
        let control = Arc::new(Control::new(parent));
        let (sender, receiver) = queue::channel(8192);
        let (reply, result) = mpsc::sync_channel(1);
        let worker_control = control.clone();
        let spawn = std::thread::Builder::new().name("mouse-voice".into()).spawn(move || {
            // Dependency diagnostics can contain endpoint names or message bodies.
            tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
                let mut heard = String::new();
                let result = runtime().and_then(|runtime| runtime.block_on(async {
                    tokio::select! {
                        biased;
                        failure = worker_control.interrupted() => Err(failure),
                        result = run(config, options, receiver, &worker_control, &mut heard, endpoint) => result,
                    }
                }));
                let result = worker_control.check().and(result);
                if let Err(failure) = &result { worker_control.fail(failure.clone()); }
                let _ = reply.send(result.map_err(|failure| Failed { failure, heard }));
            });
        });
        if spawn.is_err() {
            control.fail(Failure::Internal("could not start session".into()));
        }
        let worker = spawn.ok();
        #[cfg(not(test))]
        drop(worker);
        Self {
            inner: Some(Inner {
                #[cfg(test)]
                worker,
                control,
                result,
                #[cfg(feature = "testing")]
                script: None,
            }),
            sender: Some(sender),
        }
    }
    /// Replays caller-test results with no thread and no network.
    #[cfg(feature = "testing")]
    pub fn scripted(updates: Vec<Update>, result: Result<String, Failed>) -> Self {
        let control = Arc::new(Control::new(None));
        if let Err(error) = &result {
            control.fail(error.failure.clone());
        }
        let (_, receiver) = mpsc::channel();
        Self {
            inner: Some(Inner {
                control,
                result: receiver,
                script: Some((updates.into(), result)),
                #[cfg(test)]
                worker: None,
            }),
            sender: None,
        }
    }
    /// Queues normalized 16 kHz mono samples, preserving arbitrary boundaries.
    /// A full queue fails the session, never stalls the caller.
    pub fn feed(&mut self, samples: &[f32]) {
        let Some(inner) = &self.inner else {
            return;
        };
        let Some(sender) = &self.sender else {
            return;
        };
        let control = &inner.control;
        if control.check().is_err() {
            return;
        }
        let Some(bytes) = samples
            .len()
            .checked_mul(2)
            .filter(|n| *n <= MAX_QUEUED_BYTES)
        else {
            control.fail(buffer_full());
            return;
        };
        if control
            .queued
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(bytes).filter(|n| *n <= MAX_QUEUED_BYTES)
            })
            .is_err()
        {
            control.fail(buffer_full());
            return;
        }
        for chunk in samples.chunks(CHUNK) {
            let pcm: Vec<i16> = chunk
                .iter()
                .map(|s| {
                    if s.is_finite() {
                        (s.clamp(-1.0, 1.0) * 32768.0).clamp(-32768.0, 32767.0) as i16
                    } else {
                        0
                    }
                })
                .collect();
            if sender.try_send(pcm).is_err() {
                control.fail(buffer_full());
                return;
            }
        }
        control.wake.notify_one();
    }
    /// The latest update since the last call.
    pub fn take_update(&mut self) -> Option<Update> {
        let inner = self.inner.as_mut()?;
        #[cfg(feature = "testing")]
        if let Some((updates, _)) = &mut inner.script {
            return updates.pop_front();
        }
        inner.control.updates.pop()
    }
    /// A terminal failure already observed while recording.
    pub fn failure(&self) -> Option<Failure> {
        self.inner.as_ref()?.control.failure.get().cloned()
    }
    /// Ends the audio; the returned handle owns the remaining work.
    pub fn finish(mut self) -> Pending {
        if let Some(inner) = &self.inner {
            inner.control.ended.store(true, Ordering::Release);
            inner.control.wake.notify_one();
        }
        self.sender.take();
        Pending {
            inner: self.inner.take(),
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(inner) = &self.inner {
            inner.control.cancel();
        }
    }
}

/// An ended recording, still waiting for its final text. Drop to cancel.
pub struct Pending {
    inner: Option<Inner>,
}
impl Pending {
    pub fn canceller(&self) -> Canceller {
        Canceller {
            control: self
                .inner
                .as_ref()
                .map(|i| i.control.clone())
                .unwrap_or_else(|| Arc::new(Control::new(None))),
        }
    }
    /// Waits within the phase deadlines. Cancellation returns
    /// `Failure::Internal("cancelled")` and never publishes a late final.
    pub fn wait(mut self, mut on_update: impl FnMut(&Update)) -> Result<String, Failed> {
        let Some(inner) = self.inner.take() else {
            return Err(Failed {
                failure: cancelled(),
                heard: String::new(),
            });
        };
        // If the callback unwinds, the worker is still cancelled.
        struct CancelOnDrop(Arc<Control>);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }
        let _guard = CancelOnDrop(inner.control.clone());
        let mut heard = String::new();
        #[cfg(feature = "testing")]
        if let Some((updates, result)) = inner.script {
            for update in updates {
                if inner.control.is_cancelled() {
                    return Err(Failed {
                        failure: cancelled(),
                        heard,
                    });
                }
                heard.clone_from(&update.text);
                on_update(&update);
            }
            if inner.control.is_cancelled() {
                return Err(Failed {
                    failure: cancelled(),
                    heard,
                });
            }
            return result;
        }
        loop {
            if inner.control.is_cancelled() {
                return Err(Failed {
                    failure: cancelled(),
                    heard,
                });
            }
            if let Some(update) = inner.control.updates.pop() {
                if !update.text.is_empty() {
                    heard.clone_from(&update.text);
                }
                on_update(&update);
            }
            match inner.result.recv_timeout(Duration::from_millis(10)) {
                Ok(result) => {
                    if inner.control.is_cancelled() {
                        return Err(Failed {
                            failure: cancelled(),
                            heard,
                        });
                    }
                    if let Some(update) = inner.control.updates.pop() {
                        on_update(&update);
                    }
                    if inner.control.is_cancelled() {
                        return Err(Failed {
                            failure: cancelled(),
                            heard,
                        });
                    }
                    return result;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Failed {
                        failure: inner
                            .control
                            .failure
                            .get()
                            .cloned()
                            .unwrap_or_else(|| Failure::Internal("session worker stopped".into())),
                        heard,
                    });
                }
            }
        }
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(inner) = &self.inner {
            inner.control.cancel();
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, Failure> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Failure::Internal("could not start session runtime".into()))
}
pub(crate) fn check(config: &Config) -> Result<(), Failure> {
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
        runtime()?.block_on(async {
            let mut network = Network::connect(Arc::new(config.clone())).await?;
            network.handshake().await
        })
    })
}
pub(crate) fn transcribe(
    config: Arc<Config>,
    options: Options,
    samples: &[f32],
    cancel: Option<&Canceller>,
    on_update: impl FnMut(&Update),
) -> Result<String, Failed> {
    transcribe_to(config, options, samples, cancel, on_update, None)
}

pub(crate) fn transcribe_to(
    config: Arc<Config>,
    options: Options,
    samples: &[f32],
    cancel: Option<&Canceller>,
    mut on_update: impl FnMut(&Update),
    endpoint: Option<Endpoint>,
) -> Result<String, Failed> {
    let mut session =
        Session::start_linked(config, options, cancel.map(|c| c.control.clone()), endpoint);
    // Burst uploads were verified live. Large clips wait for local capacity
    // here, while feed itself stays nonblocking for the microphone caller.
    for chunk in samples.chunks(CHUNK) {
        let Some(inner) = &session.inner else {
            break;
        };
        while inner.control.queued.load(Ordering::Acquire) > MAX_QUEUED_BYTES - CHUNK * 2 {
            if inner.control.check().is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if inner.control.check().is_err() {
            break;
        }
        session.feed(chunk);
        if let Some(update) = session.take_update() {
            on_update(&update);
        }
    }
    session.finish().wait(on_update)
}

async fn run(
    config: Arc<Config>,
    options: Options,
    mut input: queue::Receiver<Vec<i16>>,
    control: &Control,
    heard: &mut String,
    endpoint: Option<Endpoint>,
) -> Result<String, Failure> {
    if options.final_deadline_ms == 0
        || options.polish_deadline_ms == 0
        || options.final_deadline_ms > 60_000
        || options.polish_deadline_ms > 60_000
    {
        return Err(Failure::Internal("invalid session deadline".into()));
    }
    let mut network = match endpoint {
        Some(url) => Network::connect_to(config.clone(), &url).await?,
        None => Network::connect(config.clone()).await?,
    };
    network.handshake().await?;
    control.check()?;
    let mut audio = Audio::new()?;
    let mut random = [0; 8];
    crypto::random(&mut random)?;
    let id = format!(
        "{}{}{}",
        config.number("uin") % 1_000_000,
        network::wall_time() & 0xffff_ffff,
        u64::from_le_bytes(random)
    );
    let mut recognition = Recognition::new(id);
    let mut interval = Duration::from_millis(120);
    let mut next_upload = Instant::now() + interval;
    let mut ended = None;
    let mut polish_started = None;
    let mut next_poll = Instant::now();
    let mut last_update: Option<Update> = None;
    loop {
        control.check()?;
        // Consume a bounded slice so uploads, cancellation, and responses all
        // make progress even when a whole clip arrived before the handshake.
        for _ in 0..64 {
            let Ok(samples) = input.try_recv() else {
                break;
            };
            control
                .queued
                .fetch_sub(samples.len() * 2, Ordering::AcqRel);
            audio.append(&samples)?;
            tokio::task::yield_now().await;
            control.check()?;
        }
        let now = Instant::now();
        if ended.is_none() && control.ended.load(Ordering::Acquire) && input.is_empty() {
            audio.finish()?;
            let (sequence, encoded) = audio.take();
            ended = Some(now);
            next_poll = now + Duration::from_millis(30);
            timeout(
                Duration::from_millis(options.final_deadline_ms.into()),
                network.send(
                    voice_body(
                        &recognition.id,
                        &encoded,
                        sequence,
                        audio.total,
                        true,
                        options,
                    ),
                    false,
                ),
            )
            .await
            .map_err(|_| Failure::TimedOut)??;
        } else if ended.is_none() && now >= next_upload {
            if audio.encoded.len() >= 17 {
                let (sequence, encoded) = audio.take();
                network
                    .send(
                        voice_body(
                            &recognition.id,
                            &encoded,
                            sequence,
                            audio.total,
                            false,
                            options,
                        ),
                        false,
                    )
                    .await?;
            }
            next_upload = now + interval;
        }
        let response = if let Some(end) = ended {
            let deadline = polish_started
                .map(|start| start + Duration::from_millis(options.polish_deadline_ms.into()))
                .unwrap_or(end + Duration::from_millis(options.final_deadline_ms.into()));
            if Instant::now() >= deadline {
                return Err(Failure::TimedOut);
            }
            if now >= next_poll && network.pending_polls() < 3 {
                timeout(
                    deadline.saturating_duration_since(Instant::now()),
                    network.send(
                        voice_body(&recognition.id, &[], 0, audio.total, true, options),
                        true,
                    ),
                )
                .await
                .map_err(|_| Failure::TimedOut)??;
                next_poll = now + Duration::from_millis(30);
            }
            timeout(
                deadline.saturating_duration_since(Instant::now()),
                network.receive(Duration::from_millis(10)),
            )
            .await
            .map_err(|_| Failure::TimedOut)??
        } else {
            network.receive(Duration::from_millis(10)).await?
        };
        let Some(body) = response else {
            continue;
        };
        let Some(result) = recognition.process(&body, ended.is_none())? else {
            continue;
        };
        control.check()?;
        if let Some(changed) = result.interval {
            interval = changed;
        }
        if ended.is_some() && options.polish && result.polishing && polish_started.is_none() {
            polish_started = Some(Instant::now());
        }
        if !recognition.text.is_empty() {
            heard.clone_from(&recognition.text);
        }
        if result.complete {
            return Ok(recognition.final_text());
        }
        let update = Update {
            text: recognition.text.clone(),
            polishing: ended.is_some() && options.polish && result.polishing,
        };
        if (!update.text.is_empty() || update.polishing) && last_update.as_ref() != Some(&update) {
            control.updates.force_push(update.clone());
            last_update = Some(update);
        }
    }
}
struct SpeechUpdate {
    complete: bool,
    polishing: bool,
    interval: Option<Duration>,
}
struct Recognition {
    id: String,
    text: String,
    refined: String,
}
impl Recognition {
    fn new(id: String) -> Self {
        Self {
            id,
            text: String::new(),
            refined: String::new(),
        }
    }
    fn process(&mut self, body: &[u8], recording: bool) -> Result<Option<SpeechUpdate>, Failure> {
        let outer = decode(body)?;
        let fields = decode(bytes(&outer, 1)?)?;
        let code = int(&decode(bytes(&fields, 1)?)?, 1)? as i64;
        if code != 0 {
            return Err(Failure::Service(code));
        }
        let id = text(&fields, 2)?;
        if !id.is_empty() && id != self.id {
            return Ok(None);
        }
        let ordinary = text(&fields, 4)?;
        let refined = text(&fields, 14)?;
        let complete = int(&fields, 5)? != 0;
        if recording
            && !complete
            && int(&fields, 3)? == 0
            && ordinary.is_empty()
            && !self.text.is_empty()
        {
            return Ok(None);
        }
        if !refined.is_empty() {
            self.refined = refined.into();
        }
        self.text = if refined.is_empty() {
            ordinary.into()
        } else {
            refined.into()
        };
        let interval = int(&fields, 6)?;
        Ok(Some(SpeechUpdate {
            complete,
            polishing: int(&fields, 16)? == 1,
            interval: (121..=239)
                .contains(&interval)
                .then(|| Duration::from_millis(interval)),
        }))
    }
    fn final_text(&self) -> String {
        if self.text.is_empty() {
            self.refined.clone()
        } else {
            self.text.clone()
        }
    }
}
fn cancelled() -> Failure {
    Failure::Internal("cancelled".into())
}
fn buffer_full() -> Failure {
    Failure::Internal("audio buffer full".into())
}

#[cfg(test)]
impl Session {
    pub(crate) fn local(
        config: Arc<Config>,
        options: Options,
        endpoint: impl Into<Endpoint>,
    ) -> Self {
        Self::start_linked(config, options, None, Some(endpoint.into()))
    }
    pub(crate) fn worker(&mut self) -> std::thread::JoinHandle<()> {
        self.inner
            .as_mut()
            .and_then(|inner| inner.worker.take())
            .expect("test session worker")
    }
}

#[cfg(test)]
mod recognition_tests {
    use super::*;
    use crate::wire::{
        Value::{Bytes, Int},
        message,
    };
    fn response(
        id: &str,
        ordinary: &str,
        refined: &str,
        sequence: u64,
        end: bool,
        interval: u64,
        phase: u64,
    ) -> Vec<u8> {
        message(&[(
            1,
            Bytes(&message(&[
                (2, Bytes(id.as_bytes())),
                (3, Int(sequence)),
                (4, Bytes(ordinary.as_bytes())),
                (5, Int(u64::from(end))),
                (6, Int(interval)),
                (14, Bytes(refined.as_bytes())),
                (16, Int(phase)),
            ])),
        )])
    }
    #[test]
    fn refined_text_survives_empty_final_and_recording_heartbeats() {
        let mut state = Recognition::new("test-voice".into());
        state
            .process(
                &response(
                    "test-voice",
                    "rough example",
                    "refined example",
                    1,
                    false,
                    0,
                    0,
                ),
                true,
            )
            .unwrap();
        assert!(
            state
                .process(
                    &response("test-voice", "", "unexpected example", 0, false, 0, 0),
                    true
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(state.final_text(), "refined example");
        assert!(
            state
                .process(
                    &response("other-voice", "other example", "", 1, true, 0, 0),
                    false
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(state.final_text(), "refined example");
        assert!(
            state
                .process(&response("test-voice", "", "", 1, true, 0, 0), false)
                .unwrap()
                .unwrap()
                .complete
        );
        assert_eq!(state.final_text(), "refined example");
    }
    #[test]
    fn service_interval_is_used_only_inside_the_supported_range() {
        let mut state = Recognition::new("test-voice".into());
        for (interval, expected) in [
            (120, None),
            (121, Some(Duration::from_millis(121))),
            (239, Some(Duration::from_millis(239))),
            (240, None),
        ] {
            assert_eq!(
                state
                    .process(&response("test-voice", "", "", 1, false, interval, 0), true)
                    .unwrap()
                    .unwrap()
                    .interval,
                expected
            );
        }
    }
}
