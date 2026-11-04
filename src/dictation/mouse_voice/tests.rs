use super::*;

#[test]
fn only_a_chosen_usable_configuration_can_send_audio() {
    let missing = Setup::Missing;
    let broken = Setup::Broken {
        reason: "refused".into(),
    };
    let ready = Setup::Ready {
        host: "speech.example.invalid".into(),
    };
    for state in [&missing, &broken, &ready] {
        assert_eq!(
            effective(DictationBackend::Local, state),
            DictationBackend::Local
        );
    }
    assert_eq!(
        effective(DictationBackend::MouseVoice, &ready),
        DictationBackend::MouseVoice
    );
    assert_eq!(
        effective(DictationBackend::MouseVoice, &missing),
        DictationBackend::Local
    );
    assert_eq!(
        effective(DictationBackend::MouseVoice, &broken),
        DictationBackend::Local
    );
    assert_eq!(DictationBackend::default(), DictationBackend::MouseVoice);
}

#[test]
fn failures_have_safe_codes_and_rejected_never_retries() {
    let cases = [
        (
            mouse_voice::Failure::Connection("private transport detail".into()),
            FailureReason::Connection,
        ),
        (mouse_voice::Failure::TimedOut, FailureReason::TimedOut),
        (mouse_voice::Failure::Rejected(401), FailureReason::Rejected),
        (mouse_voice::Failure::Service(503), FailureReason::Service),
        (
            mouse_voice::Failure::Internal("private implementation detail".into()),
            FailureReason::Service,
        ),
    ];
    for (failure, expected) in cases {
        assert_eq!(reason(&failure), expected);
        assert_eq!(expected.retryable(), failure.retryable());
        assert!(!failure_message(expected).contains("private"));
    }
}

/// Synthetic identity for protocol-free caller tests. It is never used by a
/// network session; every session in these tests is `Session::scripted`.
pub(crate) fn configuration() -> Arc<Config> {
    Arc::new(Config::parse(&serde_json::to_vec(&serde_json::json!({
        "host": "speech.example.invalid", "uin": "42", "device_code": "test-device",
        "device_token": "test-token", "device_model_id": "test-model", "user_device_name": "test computer",
        "version": "1.0", "system_version": "test OS", "sign_key": "test-signing-key",
        "bootstrap_aes_key": "0123456789ABCDEF", "os_type": 1, "package_type": 2, "scene": 3,
        "voice_options": {"polish":false,"traditional":true,"paragraph":true,"reduceRedundancy":true}
    })).unwrap()).expect("the client validates this synthetic configuration"))
}

#[test]
fn a_rejected_session_marks_saved_setup_broken_without_retrying_and_import_clears_it() {
    let dir = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("hex-rejected-session-{}", std::process::id()));
    let target = dir.join("mouse-voice.json");
    let mut saved = store::Store::load(target.clone());
    saved.import_config((*configuration()).clone()).unwrap();
    let config = saved.usable().unwrap();
    let previous = std::mem::replace(&mut *lock(store()), saved);
    let status = crate::control::dictation_report(&AppSettings::default());
    assert_eq!(status.backend, DictationBackend::MouseVoice);
    assert_eq!(status.mouse_voice_setup, "ready");
    let session = mouse_voice::Session::scripted(
        Vec::new(),
        Err(mouse_voice::Failed {
            failure: mouse_voice::Failure::Rejected(401),
            heard: "heard".into(),
        }),
    );
    let capture = Capture::new(session, config, mouse_voice::Options::default());
    let pending = capture.finish(&[]);
    let cancel = pending.canceller();
    let failed = pending.wait(&[], &cancel, || false).unwrap_err();
    assert_eq!(failed.failure, mouse_voice::Failure::Rejected(401));
    assert!(matches!(setup(), Setup::Broken { .. }));
    assert_eq!(current_backend(), DictationBackend::Local);
    assert_eq!(
        crate::control::dictation_report(&AppSettings::default()).mouse_voice_setup,
        "broken"
    );
    assert!(matches!(
        store::Store::load(target).setup,
        Setup::Broken { .. }
    ));
    lock(store())
        .import_config((*configuration()).clone())
        .unwrap();
    assert!(matches!(setup(), Setup::Ready { .. }));
    *lock(store()) = previous;
    std::fs::remove_dir_all(dir).unwrap();
}
