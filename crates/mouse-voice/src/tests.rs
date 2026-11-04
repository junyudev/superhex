use crate::{
    Config, Failure, Options, Session, Update,
    codec::{Audio, MAGIC},
    crypto,
    wire::{
        self,
        Value::{Bytes, Int},
        bytes, decode, int, message, text,
    },
};
use md5::Md5;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    net::{TcpListener, TcpStream},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};
use tungstenite::{
    self, Message, WebSocket,
    handshake::server::{Request, Response},
};

fn configuration() -> Value {
    json!({"host":"speech.example.invalid", "uin":"42", "device_code":"test-device", "device_token":"test-token", "device_model_id":"test-model", "user_device_name":"test computer", "version":"1.0", "system_version":"test OS", "sign_key":"test-signing-key", "bootstrap_aes_key":"0123456789ABCDEF", "os_type":1, "package_type":2, "scene":3})
}
fn config() -> Arc<Config> {
    Arc::new(Config::parse(&serde_json::to_vec(&configuration()).unwrap()).unwrap())
}

#[test]
fn imports_both_shapes_and_only_durable_fields() {
    let mut value = configuration();
    value["session_secret"] = "discard".into();
    value["voice_options"] = json!({"polish":false,"traditional":true,"paragraph":true,"reduceRedundancy":true,"phaseA":2000,"phaseB":4500,"discard":true});
    value["websocket_path"] = "/ppp".into();
    for input in [value.clone(), json!({"configuration":value})] {
        let parsed = Config::parse(&serde_json::to_vec(&input).unwrap()).unwrap();
        assert_eq!(parsed.host(), "speech.example.invalid");
        assert_eq!(
            parsed.options(),
            Options {
                polish: false,
                traditional: true,
                paragraph: true,
                reduce_redundancy: true,
                final_deadline_ms: 2000,
                polish_deadline_ms: 4500
            }
        );
        let durable: Value = serde_json::from_slice(&parsed.to_json()).unwrap();
        assert!(durable.get("session_secret").is_none());
        assert!(durable["voice_options"].get("discard").is_none());
        assert_eq!(
            Config::parse(&parsed.to_json()).unwrap().options(),
            parsed.options()
        );
        assert_eq!(format!("{parsed:?}"), "Config(redacted)");
    }
}
#[test]
fn config_validation_is_bounded_and_redacted() {
    for field in ["uin", "os_type", "scene", "package_type"] {
        for invalid in [
            json!(true),
            json!(-1),
            json!(1.2),
            json!("1e2"),
            json!(""),
            json!("18446744073709551616"),
        ] {
            let mut value = configuration();
            value[field] = invalid;
            assert!(Config::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
    for (field, value) in [
        ("uin", json!(0)),
        ("host", json!("https://private.invalid")),
        ("host", json!("x@private.invalid")),
        ("host", json!("private.invalid/path")),
        ("host", json!("private.invalid?x")),
        ("host", json!(" private.invalid")),
        ("sign_key", json!("")),
        ("bootstrap_aes_key", json!("private")),
        ("websocket_path", json!("/other")),
        ("voice_options", json!({"polish":1})),
        ("voice_options", json!({"phaseA":0})),
        ("voice_options", json!({"phaseB":60001})),
    ] {
        let mut cfg = configuration();
        cfg[field] = value;
        let error = Config::parse(&serde_json::to_vec(&cfg).unwrap()).unwrap_err();
        assert!(!error.to_string().contains("private"));
    }
    assert!(Config::parse(&vec![b' '; 1_048_577]).is_err());
    let mut value = configuration();
    value["uin"] = json!(u64::MAX);
    assert!(Config::parse(&serde_json::to_vec(&value).unwrap()).is_ok());
    assert!(
        Config::parse(b"{\"sensitive text\":")
            .unwrap_err()
            .to_string()
            .contains("JSON")
    );
}
#[test]
fn protobuf_preserves_presence_and_bounds_hostile_inputs() {
    let body = message(&[(1, Int(0)), (2, Bytes(b"")), (3, Int(u64::MAX))]);
    let fields = decode(&body).unwrap();
    assert_eq!(fields.len(), 3);
    assert_eq!(int(&fields, 3).unwrap(), u64::MAX);
    for body in [
        vec![0],
        vec![8, 128],
        vec![10, 3, 1],
        vec![15],
        [vec![8], vec![255; 10]].concat(),
        [vec![10], vec![255; 10]].concat(),
    ] {
        assert!(decode(&body).is_err());
    }
    assert!(decode(&vec![0; wire::MAX_MESSAGE + 1]).is_err());
}
#[test]
fn encryption_matches_python_cryptography_for_all_key_sizes() {
    // PKCS7/AES-ECB, generated independently using Python cryptography.
    include!("encryption_vectors.in");
    assert!(crypto::decrypt(&[1, 2, 3], "0123456789ABCDEF").is_err());
}
#[test]
fn opus_boundaries_preserve_samples_and_pad_only_the_tail() {
    let source: Vec<i16> = (0..769).map(|i| (i * 31) as i16).collect();
    let mut whole = Audio::new().unwrap();
    whole.append(&source).unwrap();
    whole.finish().unwrap();
    let mut parts = Audio::new().unwrap();
    for chunk in source.chunks(7) {
        parts.append(chunk).unwrap();
    }
    parts.finish().unwrap();
    assert_eq!(parts.samples, source.len());
    assert_eq!(parts.encoded, whole.encoded);
    let (sequence, encoded) = parts.take();
    assert_eq!(sequence, 1);
    assert_eq!(parts.total, encoded.len() as u64);
    assert!(parts.encoded.is_empty());
    assert!(encoded.starts_with(MAGIC));
    let mut pos = MAGIC.len();
    let mut count = 0;
    let mut decoder = opus::Decoder::new(16000, opus::Channels::Mono).unwrap();
    while pos < encoded.len() {
        let length = u16::from_le_bytes(encoded[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        assert_eq!(
            decoder
                .decode(&encoded[pos..pos + length], &mut [0i16; 320], false)
                .unwrap(),
            320
        );
        pos += length;
        count += 1;
    }
    assert_eq!(count, 3);
    assert!(Audio::new().unwrap().finish().is_err());
    let mut short = Audio::new().unwrap();
    short.append(&[1]).unwrap();
    short.finish().unwrap();
    assert!(short.encoded.len() > MAGIC.len() + 2);
}
#[test]
fn debug_redacts_text() {
    let update = Update {
        text: "sensitive sentinel".into(),
        polishing: true,
    };
    let failed = crate::Failed {
        failure: Failure::TimedOut,
        heard: update.text.clone(),
    };
    assert!(!format!("{update:?} {failed:?}").contains("sensitive"));
    assert!(!Failure::Rejected(1).retryable());
    assert!(Failure::Service(5004).retryable());
}

struct Incoming {
    task: u64,
    path: String,
    body: Vec<u8>,
    headers: BTreeMap<String, String>,
}
struct Peer {
    socket: WebSocket<TcpStream>,
    key: String,
}
impl Peer {
    fn receive(&mut self) -> Option<Incoming> {
        let data = match self.socket.read().ok()? {
            Message::Binary(data) => data,
            _ => return None,
        };
        let outer = decode(&data).unwrap();
        assert_eq!(int(&outer, 1).unwrap(), 0);
        assert_eq!(int(&outer, 2).unwrap(), 0);
        assert!(outer.iter().any(|(n, _)| *n == 1));
        assert!(outer.iter().any(|(n, _)| *n == 2));
        let task = int(&outer, 3).unwrap();
        let http = decode(bytes(&outer, 5).unwrap()).unwrap();
        assert_eq!(text(&http, 1).unwrap(), "POST");
        assert!(http.iter().any(|(n, _)| *n == 4));
        let path = text(&http, 3).unwrap().to_owned();
        let mut headers = BTreeMap::new();
        for (_, v) in http.iter().filter(|(n, _)| *n == 5) {
            let Bytes(b) = v else {
                panic!("invalid header")
            };
            let h = decode(b).unwrap();
            headers.insert(
                text(&h, 1).unwrap().to_owned(),
                text(&h, 2).unwrap().to_owned(),
            );
        }
        let body = bytes(&http, 6).unwrap();
        assert_eq!(headers["Kb-BodyMd5"], hex::encode_upper(Md5::digest(body)));
        assert_eq!(headers["Content-Length"], body.len().to_string());
        let signed_fields: &[&str] = if path == "/oauth_pubkey_v2" {
            &[
                "Kb-OsType",
                "Kb-Version",
                "Kb-Scene",
                "Kb-TimeStamp",
                "Kb-BodyMd5",
                "Kb-TraceId",
                "Kb-TaskId",
            ]
        } else {
            &[
                "Kb-OsType",
                "Kb-Version",
                "Kb-Scene",
                "Kb-CmdId",
                "Kb-SubCmdId",
                "Kb-TimeStamp",
                "Kb-BodyMd5",
                "Kb-Uin",
                "Kb-TraceId",
                "Kb-TaskId",
            ]
        };
        let mut signed = String::new();
        for &name in signed_fields {
            signed.push_str(&headers[name]);
        }
        signed.push_str("test-signing-key");
        assert_eq!(
            headers["Kb-Sign"],
            hex::encode_upper(Sha256::digest(signed.as_bytes()))
        );
        let key = match path.as_str() {
            "/timestamp" => "",
            "/oauth_pubkey_v2" => "0123456789ABCDEF",
            _ => &self.key,
        };
        let mut body = if key.is_empty() {
            body.to_vec()
        } else {
            crypto::decrypt(body, key).unwrap()
        };
        if headers["Kb-CompressionType"] == "2" {
            body = snap::raw::Decoder::new().decompress_vec(&body).unwrap();
        }
        Some(Incoming {
            task,
            path,
            body,
            headers,
        })
    }
    fn response(
        &mut self,
        request: &Incoming,
        code: i64,
        body: &[u8],
        key: &str,
        compressed: bool,
    ) {
        let mut body = if compressed {
            snap::raw::Encoder::new().compress_vec(body).unwrap()
        } else {
            body.to_vec()
        };
        if !key.is_empty() && !body.is_empty() {
            body = crypto::encrypt(&body, key).unwrap();
        }
        let code = code.to_string();
        let header = message(&[(1, Bytes(b"Error")), (2, Bytes(code.as_bytes()))]);
        let compression = message(&[
            (1, Bytes(b"Kb-CompressionType")),
            (2, Bytes(if compressed { b"2" } else { b"1" })),
        ]);
        let http = message(&[
            (2, Int(200)),
            (4, Bytes(&header)),
            (4, Bytes(&compression)),
            (5, Bytes(&body)),
        ]);
        self.socket
            .send(Message::Binary(
                message(&[(2, Int(request.task)), (4, Bytes(&http))]).into(),
            ))
            .unwrap();
    }
    fn calibrate(&mut self) {
        let request = self.receive().unwrap();
        assert_eq!(request.path, "/timestamp");
        self.response(
            &request,
            0,
            &message(&[(1, Int(1_700_000_000_000))]),
            "",
            false,
        );
    }
    fn handshake(&mut self) -> String {
        self.calibrate();
        let request = self.receive().unwrap();
        assert_eq!(request.path, "/oauth_pubkey_v2");
        assert_eq!(request.headers["Kb-DeviceCodeRestrictionV2"], "1");
        let fields = decode(&request.body).unwrap();
        assert!(
            fields
                .iter()
                .any(|(n, v)| *n == 2 && matches!(v,Bytes(b) if b.is_empty()))
        );
        assert!(fields.iter().any(|(n, v)| *n == 3 && matches!(v, Int(0))));
        assert_eq!(int(&fields, 5).unwrap(), 42);
        let client = text(&fields, 1).unwrap().to_owned();
        let private = crypto::KeyExchange::new().unwrap();
        self.key = private.shared(&client).unwrap().to_string();
        self.response(
            &request,
            0,
            &message(&[
                (1, Int(0)),
                (2, Bytes(private.public().unwrap().as_bytes())),
            ]),
            "0123456789ABCDEF",
            false,
        );
        client
    }
    fn speech(
        &mut self,
        request: &Incoming,
        ordinary: &str,
        refined: &str,
        end: bool,
        phase: u64,
        interval: u64,
    ) {
        let outer = decode(&request.body).unwrap();
        let voice = decode(bytes(&outer, 1).unwrap()).unwrap();
        let body = message(&[(
            1,
            Bytes(&message(&[
                (2, Bytes(bytes(&voice, 2).unwrap())),
                (3, Int(1)),
                (4, Bytes(ordinary.as_bytes())),
                (5, Int(u64::from(end))),
                (6, Int(interval)),
                (14, Bytes(refined.as_bytes())),
                (16, Int(phase)),
            ])),
        )]);
        self.response(request, 0, &body, &self.key.clone(), true);
    }
}
#[allow(
    clippy::result_large_err,
    reason = "The server callback error type is fixed by tungstenite"
)]
fn fake(run: impl FnOnce(&mut Peer) + Send + 'static) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        let tcp = loop {
            match listener.accept() {
                Ok((tcp, _)) => break tcp,
                Err(_) => {
                    assert!(Instant::now() < end, "connection missing");
                    thread::sleep(Duration::from_millis(5));
                }
            }
        };
        tcp.set_nonblocking(false).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        tcp.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
        let socket = tungstenite::accept_hdr(tcp, |request: &Request, mut response: Response| {
            assert_eq!(request.headers()["Sec-WebSocket-Protocol"], "wxws_pb");
            response
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", "wxws_pb".parse().unwrap());
            Ok(response)
        })
        .unwrap();
        run(&mut Peer {
            socket,
            key: String::new(),
        });
    });
    (format!("ws://{address}"), handle)
}
fn voice(request: &Incoming) -> wire::Fields<'_> {
    decode(bytes(&decode(&request.body).unwrap(), 1).unwrap()).unwrap()
}
fn local(endpoint: String, options: Options) -> Session {
    Session::local(config(), options, endpoint)
}

#[test]
fn audio_queued_before_handshake_flushes_tail_and_one_final() {
    let (endpoint, server) = fake(|peer| {
        thread::sleep(Duration::from_millis(50));
        peer.handshake();
        let request = peer.receive().unwrap();
        let fields = voice(&request);
        assert_eq!(int(&fields, 6).unwrap(), 1);
        assert_eq!(int(&fields, 7).unwrap(), 1);
        let audio = bytes(&fields, 4).unwrap();
        assert!(audio.starts_with(MAGIC));
        assert_eq!(int(&fields, 11).unwrap(), audio.len() as u64);
        let mut at = MAGIC.len();
        let mut frames = 0;
        while at < audio.len() {
            let length = u16::from_le_bytes(audio[at..at + 2].try_into().unwrap()) as usize;
            at += 2 + length;
            frames += 1;
        }
        assert_eq!(frames, 2);
        peer.speech(&request, "rough example", "polished example", true, 0, 0);
    });
    let mut session = local(endpoint, Options::default());
    session.feed(&[0.1; 17]);
    session.feed(&[0.2; 304]);
    assert_eq!(session.finish().wait(|_| {}).unwrap(), "polished example");
    server.join().unwrap();
}
#[test]
fn streaming_partials_and_polishing_extend_only_the_second_deadline() {
    let (sent, ready) = mpsc::channel();
    let (endpoint, server) = fake(move |peer| {
        peer.handshake();
        let first = peer.receive().unwrap();
        assert_eq!(int(&voice(&first), 6).unwrap(), 0);
        peer.speech(&first, "partial example", "", false, 0, 180);
        sent.send(()).unwrap();
        loop {
            let request = peer.receive().unwrap();
            if int(&voice(&request), 6).unwrap() != 0 {
                peer.speech(&request, "partial example", "", false, 1, 0);
                break;
            }
            peer.speech(&request, "partial example", "", false, 0, 0);
        }
        thread::sleep(Duration::from_millis(90));
        let poll = peer.receive().unwrap();
        assert_eq!(int(&voice(&poll), 7).unwrap(), 0);
        peer.speech(&poll, "raw example", "polished example", true, 0, 0);
    });
    let mut session = local(
        endpoint,
        Options {
            final_deadline_ms: 60,
            polish_deadline_ms: 250,
            ..Options::default()
        },
    );
    session.feed(&[0.2; 1920]);
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    thread::sleep(Duration::from_millis(20));
    assert_eq!(session.take_update().unwrap().text, "partial example");
    let mut polishing = false;
    let result = session.finish().wait(|u| polishing |= u.polishing).unwrap();
    assert!(polishing);
    assert_eq!(result, "polished example");
    server.join().unwrap();
}
#[test]
fn final_and_polish_deadlines_are_terminal_and_preserve_heard_text() {
    for polish_phase in [false, true] {
        let (endpoint, server) = fake(move |peer| {
            peer.handshake();
            let end = peer.receive().unwrap();
            peer.speech(&end, "heard example", "", false, u64::from(polish_phase), 0);
            while peer.receive().is_some() {}
        });
        let mut session = local(
            endpoint,
            Options {
                final_deadline_ms: 70,
                polish_deadline_ms: 110,
                ..Options::default()
            },
        );
        session.feed(&[0.1; 320]);
        let started = Instant::now();
        let error = session.finish().wait(|_| {}).unwrap_err();
        assert_eq!(error.failure, Failure::TimedOut);
        assert_eq!(error.heard, "heard example");
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }
}
#[test]
fn polish_off_and_empty_audio_contract() {
    let (endpoint, server) = fake(|peer| {
        peer.handshake();
        let end = peer.receive().unwrap();
        assert!(!voice(&end).iter().any(|(n, _)| *n == 24));
        peer.speech(&end, "ordinary example", "", true, 0, 0);
    });
    let mut session = local(
        endpoint,
        Options {
            polish: false,
            ..Options::default()
        },
    );
    session.feed(&[0.0; 1]);
    assert_eq!(session.finish().wait(|_| {}).unwrap(), "ordinary example");
    server.join().unwrap();
    let (endpoint, server) = fake(|peer| {
        peer.handshake();
        assert!(peer.receive().is_none());
    });
    assert_eq!(
        local(endpoint, Options::default())
            .finish()
            .wait(|_| {})
            .unwrap_err()
            .failure,
        Failure::Internal("empty audio".into())
    );
    server.join().unwrap();
}
#[test]
fn refresh_drains_old_key_replies_then_resends_exact_business_body_once() {
    for code in [5004, 5011] {
        let (partial, got_partial) = mpsc::channel();
        let (endpoint, server) = fake(move |peer| {
            let first_public = peer.handshake();
            let first = peer.receive().unwrap();
            partial.send(()).unwrap();
            let second = peer.receive().unwrap();
            peer.response(&first, code, &[], "", false);
            // A successful old-key response must survive the refresh.
            peer.speech(&second, "drained example", "", false, 0, 0);
            if code == 5011 {
                assert_ne!(peer.handshake(), first_public);
            } else {
                peer.calibrate();
            }
            let resent = peer.receive().unwrap();
            assert_eq!(resent.body, first.body);
            assert_ne!(resent.task, first.task);
            peer.speech(&resent, "raw example", "final example", true, 0, 0);
        });
        let mut session = local(endpoint, Options::default());
        session.feed(&[0.1; 1920]);
        got_partial.recv_timeout(Duration::from_secs(2)).unwrap();
        let result = session.finish().wait(|_| {}).unwrap();
        assert_eq!(result, "final example");
        server.join().unwrap();
    }
}
#[test]
fn repeated_refresh_and_business_rejection_are_classified_honestly() {
    let (endpoint, server) = fake(|peer| {
        peer.handshake();
        let first = peer.receive().unwrap();
        peer.response(&first, 5004, &[], "", false);
        peer.calibrate();
        let second = peer.receive().unwrap();
        assert_eq!(first.body, second.body);
        peer.response(&second, 5004, &[], "", false);
    });
    let mut session = local(endpoint, Options::default());
    session.feed(&[0.1; 320]);
    assert_eq!(
        session.finish().wait(|_| {}).unwrap_err().failure,
        Failure::Service(5004)
    );
    server.join().unwrap();
    let (endpoint, server) = fake(|peer| {
        peer.calibrate();
        let dh = peer.receive().unwrap();
        peer.response(&dh, 0, &message(&[(1, Int(77))]), "0123456789ABCDEF", false);
    });
    assert_eq!(
        local(endpoint, Options::default())
            .finish()
            .wait(|_| {})
            .unwrap_err()
            .failure,
        Failure::Rejected(77)
    );
    server.join().unwrap();
}
#[test]
fn overflow_fails_session_without_blocking_feed() {
    let mut session = local("ws://127.0.0.1:1".into(), Options::default());
    let samples = vec![0.0; 8 * 1024 * 1024 + 1];
    let start = Instant::now();
    session.feed(&samples);
    assert!(start.elapsed() < Duration::from_millis(100));
    assert_eq!(
        session.failure(),
        Some(Failure::Internal("audio buffer full".into()))
    );
    assert_eq!(
        session.finish().wait(|_| {}).unwrap_err().failure,
        Failure::Internal("audio buffer full".into())
    );
}
#[test]
fn cancellation_during_handshake_and_final_wait_closes_the_socket_promptly() {
    for stage in 0..4 {
        let (arrived, ready) = mpsc::channel();
        let (endpoint, server) = fake(move |peer| {
            match stage {
                0 => {}
                1 => {
                    let _ = peer.receive().unwrap();
                }
                2 => {
                    peer.calibrate();
                    let _ = peer.receive().unwrap();
                }
                _ => {
                    peer.handshake();
                    let request = peer.receive().unwrap();
                    peer.speech(&request, "heard example", "", false, 1, 0);
                }
            }
            arrived.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_millis(500);
            while peer.receive().is_some() {
                assert!(Instant::now() < deadline);
            }
            assert!(Instant::now() < deadline);
        });
        let mut session = local(endpoint, Options::default());
        session.feed(&[0.1; 320]);
        let pending = session.finish();
        let cancel = pending.canceller();
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let start = Instant::now();
        if stage % 2 == 0 {
            cancel.cancel();
        } else {
            drop(cancel);
        }
        assert_eq!(
            pending
                .wait(|_| panic!("cancelled update escaped"))
                .unwrap_err()
                .failure,
            Failure::Internal("cancelled".into())
        );
        assert!(start.elapsed() < Duration::from_millis(150));
        server.join().unwrap();
    }
}
#[cfg(feature = "testing")]
#[test]
fn scripted_sessions_have_no_transport_and_cancel_late_finals() {
    let mut session = Session::scripted(
        vec![
            Update {
                text: "partial".into(),
                polishing: false,
            },
            Update {
                text: "partial".into(),
                polishing: true,
            },
        ],
        Ok("final".into()),
    );
    assert_eq!(session.take_update().unwrap().text, "partial");
    session.feed(&[0.1; 320]);
    let pending = session.finish();
    let cancel = pending.canceller();
    assert_eq!(
        pending.wait(|_| cancel.cancel()).unwrap_err().failure,
        Failure::Internal("cancelled".into())
    );
    assert_eq!(
        Session::scripted(vec![], Ok("final".into()))
            .finish()
            .wait(|_| {})
            .unwrap(),
        "final"
    );
}

#[test]
fn cancelled_drops_abort_a_stalled_websocket_upgrade() {
    use std::io::Read;
    for finish in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut buffer = [0; 2048];
            assert!(socket.read(&mut buffer).unwrap() > 0);
            ready_tx.send(()).unwrap();
            assert_eq!(socket.read(&mut buffer).unwrap(), 0);
        });
        let session = local(format!("ws://{address}"), Options::default());
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        if finish {
            drop(session.finish());
        } else {
            drop(session);
        }
        server.join().unwrap();
    }
}
#[test]
fn cancellation_when_final_arrives_never_releases_the_final() {
    let (sent, arrived) = mpsc::channel();
    let (endpoint, server) = fake(move |peer| {
        peer.handshake();
        let request = peer.receive().unwrap();
        peer.speech(&request, "partial example", "", false, 1, 0);
        let poll = peer.receive().unwrap();
        peer.speech(&poll, "raw example", "late final example", true, 0, 0);
        sent.send(()).unwrap();
    });
    let mut session = local(endpoint, Options::default());
    session.feed(&[0.1; 320]);
    let pending = session.finish();
    let cancel = pending.canceller();
    arrived.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        pending.wait(|_| cancel.cancel()).unwrap_err().failure,
        Failure::Internal("cancelled".into())
    );
    server.join().unwrap();
}

#[test]
fn cancellation_aborts_a_stalled_tls_handshake() {
    use std::io::Read;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, arrived) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert!(socket.read(&mut [0; 4096]).unwrap() > 0);
        sent.send(()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        assert_eq!(socket.read(&mut [0; 4096]).unwrap(), 0);
    });
    let pending = local(format!("wss://{address}"), Options::default()).finish();
    let cancel = pending.canceller();
    arrived.recv_timeout(Duration::from_secs(3)).unwrap();
    cancel.cancel();
    assert_eq!(
        pending.wait(|_| {}).unwrap_err().failure,
        Failure::Internal("cancelled".into())
    );
    server.join().unwrap();
}

#[test]
fn cancellation_interrupts_the_refresh_drain() {
    let (sent, arrived) = mpsc::channel();
    let (endpoint, server) = fake(move |peer| {
        peer.handshake();
        let first = peer.receive().unwrap();
        let _poll = peer.receive().unwrap();
        peer.response(&first, 5011, &[], "", false);
        sent.send(()).unwrap();
        assert!(peer.receive().is_none());
    });
    let mut session = local(endpoint, Options::default());
    session.feed(&[0.1; 320]);
    let pending = session.finish();
    let cancel = pending.canceller();
    arrived.recv_timeout(Duration::from_secs(2)).unwrap();
    thread::sleep(Duration::from_millis(20));
    let start = Instant::now();
    cancel.cancel();
    assert_eq!(
        pending.wait(|_| {}).unwrap_err().failure,
        Failure::Internal("cancelled".into())
    );
    server.join().unwrap();
    assert!(start.elapsed() < Duration::from_millis(250));
}

#[test]
fn cancellation_during_dns_leaves_no_worker_or_resolver_thread() {
    let dns = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    dns.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let endpoint = crate::network::Endpoint {
        url: "ws://speech.example.com".into(),
        dns: Some(dns.local_addr().unwrap()),
    };
    let mut session = Session::local(config(), Options::default(), endpoint);
    let worker = session.worker();
    let pending = session.finish();
    let cancel = pending.canceller();
    // A real DNS request is in flight; the local resolver never answers it.
    assert!(dns.recv_from(&mut [0; 4096]).unwrap().0 > 0);
    let start = Instant::now();
    cancel.cancel();
    assert_eq!(
        pending.wait(|_| {}).unwrap_err().failure,
        Failure::Internal("cancelled".into())
    );
    while !worker.is_finished() && start.elapsed() < Duration::from_millis(500) {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(worker.is_finished());
    worker.join().unwrap();
}

#[test]
fn independent_canceller_stops_whole_clip_during_connection() {
    use std::io::Read;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, arrived) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert!(socket.read(&mut [0; 2048]).unwrap() > 0);
        sent.send(()).unwrap();
        assert_eq!(socket.read(&mut [0; 2048]).unwrap(), 0);
    });
    let cancel = crate::Canceller::new();
    let linked = cancel.clone();
    let (reply, result) = mpsc::channel();
    let caller = thread::spawn(move || {
        let result = crate::session::transcribe_to(
            config(),
            Options::default(),
            &[0.1; 16000],
            Some(&linked),
            |_| {},
            Some(format!("ws://{address}").into()),
        );
        reply.send(result).unwrap();
    });
    arrived.recv_timeout(Duration::from_secs(2)).unwrap();
    cancel.cancel();
    assert_eq!(
        result
            .recv_timeout(Duration::from_millis(250))
            .unwrap()
            .unwrap_err()
            .failure,
        Failure::Internal("cancelled".into())
    );
    caller.join().unwrap();
    server.join().unwrap();
}

#[test]
fn independent_canceller_stops_whole_clip_during_upload() {
    let (sent, arrived) = mpsc::channel();
    let (endpoint, server) = fake(move |peer| {
        peer.handshake();
        let request = peer.receive().unwrap();
        assert_eq!(int(&voice(&request), 6).unwrap(), 0);
        assert!(!bytes(&voice(&request), 4).unwrap().is_empty());
        sent.send(()).unwrap();
        while peer.receive().is_some() {}
    });
    let cancel = crate::Canceller::default();
    let linked = cancel.clone();
    let (reply, result) = mpsc::channel();
    let caller = thread::spawn(move || {
        // More than the PCM admission cap: the whole-clip caller is still
        // feeding the clip when the first network upload starts.
        let samples = vec![0.1; 16_000_000];
        let result = crate::session::transcribe_to(
            config(),
            Options::default(),
            &samples,
            Some(&linked),
            |_| {},
            Some(endpoint.into()),
        );
        reply.send(result).unwrap();
    });
    arrived.recv_timeout(Duration::from_secs(3)).unwrap();
    cancel.cancel();
    assert_eq!(
        result
            .recv_timeout(Duration::from_millis(300))
            .unwrap()
            .unwrap_err()
            .failure,
        Failure::Internal("cancelled".into())
    );
    caller.join().unwrap();
    server.join().unwrap();
}
