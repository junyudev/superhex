use crate::{
    Config, Failure, Options, crypto,
    wire::{
        self,
        Value::{Bytes, Int},
        bytes, decode, int, malformed, message, text,
    },
};
use futures_util::{SinkExt, StreamExt};
use md5::Md5;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpStream,
    sync::oneshot,
    time::{Instant, timeout},
};
use tokio_websockets::{
    ClientBuilder, Connector, Limits, MaybeTlsStream, Message, WebSocketStream,
};
use zeroize::Zeroizing;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_PENDING: usize = 128;
const MAX_PENDING_BYTES: usize = 16 * 1024 * 1024;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[cfg(test)]
type Lookup = Box<dyn FnOnce() -> Result<Vec<SocketAddr>, Failure> + Send>;

pub(crate) struct Endpoint {
    pub url: String,
    #[cfg(test)]
    pub lookup: Option<Lookup>,
}
impl From<String> for Endpoint {
    fn from(url: String) -> Self {
        Self {
            url,
            #[cfg(test)]
            lookup: None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Mode {
    Ordinary,
    Dh,
    GenUin(Zeroizing<String>),
    Timestamp,
}
pub(crate) struct Handshake {
    server_public_key: String,
    gen_uin_token: Zeroizing<Vec<u8>>,
}
struct Request {
    body: Vec<u8>,
    key: Zeroizing<String>,
    mode: Mode,
    attempt: u8,
    poll: bool,
}
pub(crate) struct Network {
    socket: Socket,
    config: Arc<Config>,
    key: Zeroizing<String>,
    counter: u64,
    clock: Option<(u64, Instant)>,
    pending: BTreeMap<u64, Request>,
    ready: VecDeque<Vec<u8>>,
    ready_bytes: usize,
}
enum Response {
    Body(Vec<u8>),
    Error(i64, Request),
    Unknown,
}

impl Network {
    pub async fn connect(config: Arc<Config>) -> Result<Self, Failure> {
        let endpoint = format!("wss://{}{}", config.host(), config.text("websocket_path"));
        Self::connect_to(config, endpoint.into()).await
    }
    pub(crate) async fn connect_to(
        config: Arc<Config>,
        endpoint: Endpoint,
    ) -> Result<Self, Failure> {
        let socket = timeout(CONNECT_TIMEOUT, open(endpoint))
            .await
            .map_err(|_| connection("connect timed out"))??;
        let mut random = [0; 4];
        crypto::random(&mut random)?;
        Ok(Self {
            socket,
            config,
            key: Zeroizing::new(String::new()),
            counter: 1_000_000 + u64::from(u32::from_le_bytes(random) % 1_000_000),
            clock: None,
            pending: BTreeMap::new(),
            ready: VecDeque::new(),
            ready_bytes: 0,
        })
    }
    fn timestamp(&self) -> u64 {
        self.clock
            .map(|(time, start)| time.saturating_add(start.elapsed().as_millis() as u64))
            .unwrap_or_else(wall_time)
    }
    pub fn pending_polls(&self) -> usize {
        self.pending.values().filter(|r| r.poll).count()
    }
    pub async fn handshake(&mut self) -> Result<(), Failure> {
        self.handshake_with(false).await.map(|_| ())
    }
    pub async fn handshake_with(&mut self, need_gen_uin: bool) -> Result<Handshake, Failure> {
        if !self.pending.is_empty() {
            return Err(Failure::Internal("undrained handshake".into()));
        }
        self.calibrate().await?;
        let private = crypto::KeyExchange::new()?;
        let public = private.public()?;
        let cfg = &self.config;
        let identity = if cfg.number("uin") != 0 {
            (5, Int(cfg.number("uin")))
        } else {
            (6, Bytes(b""))
        };
        let body = message(&[
            (1, Bytes(public.as_bytes())),
            (2, Bytes(b"")),
            (3, Int(u64::from(need_gen_uin))),
            (4, Bytes(cfg.text("device_code").as_bytes())),
            identity,
            (7, Bytes(cfg.text("device_token").as_bytes())),
            (8, Bytes(cfg.text("device_model_id").as_bytes())),
            (9, Bytes(cfg.text("user_device_name").as_bytes())),
        ]);
        let request = Request {
            body,
            key: Zeroizing::new(cfg.text("bootstrap_aes_key").into()),
            mode: Mode::Dh,
            attempt: 0,
            poll: false,
        };
        let body = self.request(request).await?;
        let fields = decode(&body)?;
        let code = int(&fields, 1)? as i64;
        if code != 0 {
            return Err(Failure::Rejected(code));
        }
        // A device binding mismatch cannot be repaired by retrying this identity.
        if int(&fields, 5)? != 0 {
            return Err(Failure::Rejected(-1));
        }
        let server_public_key = text(&fields, 2)?.to_owned();
        self.key = private.shared(&server_public_key)?;
        Ok(Handshake {
            server_public_key,
            gen_uin_token: Zeroizing::new(bytes(&fields, 4)?.to_vec()),
        })
    }
    pub async fn register(&mut self, handshake: Handshake) -> Result<Config, Failure> {
        let token = std::str::from_utf8(&handshake.gen_uin_token).map_err(|_| malformed())?;
        if self.key.is_empty()
            || token.is_empty()
            || token.len() > 16_384
            || !token.is_ascii()
            || token.bytes().any(|b| b.is_ascii_control())
        {
            return Err(malformed());
        }
        let uuid = initialization_uuid()?;
        let cfg = &self.config;
        let mut fields = vec![
            (1, Bytes(cfg.text("device_code").as_bytes())),
            (2, Bytes(handshake.server_public_key.as_bytes())),
        ];
        if cfg.number("uin") != 0 {
            fields.push((3, Int(cfg.number("uin"))));
        }
        fields.extend([
            (4, Bytes(uuid.as_bytes())),
            (5, Bytes(cfg.text("device_token").as_bytes())),
        ]);
        let request = Request {
            body: message(&fields),
            key: self.key.clone(),
            mode: Mode::GenUin(Zeroizing::new(token.into())),
            attempt: 0,
            poll: false,
        };
        let body = self.request(request).await?;
        let fields = decode(&body)?;
        let code = int(&fields, 1)? as i64;
        if code != 0 {
            return Err(Failure::Rejected(code));
        }
        let uin = int(&fields, 2)?;
        if uin == 0 {
            return Err(Failure::Internal(
                "registration returned an invalid identity".into(),
            ));
        }
        let mut config = (*self.config).clone();
        config.durable.insert("uin".into(), uin.into());
        let config = Config::parse(&config.to_json()).map_err(|_| malformed())?;
        self.config = Arc::new(config.clone());
        Ok(config)
    }
    async fn calibrate(&mut self) -> Result<(), Failure> {
        let body = self
            .request(Request {
                body: Vec::new(),
                key: Zeroizing::new(String::new()),
                mode: Mode::Timestamp,
                attempt: 0,
                poll: false,
            })
            .await?;
        let stamp = int(&decode(&body)?, 1)?;
        if stamp == 0 {
            return Err(malformed());
        }
        self.clock = Some((stamp, Instant::now()));
        Ok(())
    }
    async fn request(&mut self, request: Request) -> Result<Vec<u8>, Failure> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let task = self.send_request(request).await?;
        loop {
            let (id, response) = timeout(
                deadline.saturating_duration_since(Instant::now()),
                self.receive_one(),
            )
            .await
            .map_err(|_| Failure::TimedOut)??;
            match response {
                Response::Body(body) if id == task => return Ok(body),
                Response::Body(body) => self.keep_ready(body)?,
                Response::Error(code, _) => return Err(Failure::Service(code)),
                Response::Unknown => {}
            }
        }
    }
    pub async fn send(&mut self, body: Vec<u8>, poll: bool) -> Result<(), Failure> {
        let request = Request {
            body,
            key: self.key.clone(),
            mode: Mode::Ordinary,
            attempt: 0,
            poll,
        };
        self.send_request(request).await?;
        Ok(())
    }
    async fn send_request(&mut self, request: Request) -> Result<u64, Failure> {
        if self.pending.len() >= MAX_PENDING
            || self.pending.values().map(|r| r.body.len()).sum::<usize>() + request.body.len()
                > MAX_PENDING_BYTES
        {
            return Err(Failure::Internal("service request buffer full".into()));
        }
        self.counter += 1;
        let task = match &request.mode {
            Mode::Dh => 2_147_483_646,
            Mode::GenUin(_) => 2_147_483_645,
            _ => self.counter,
        };
        let envelope = encode_request(&self.config, &request, task, self.timestamp())?;
        timeout(REQUEST_TIMEOUT, self.socket.send(Message::binary(envelope)))
            .await
            .map_err(|_| connection("write timed out"))?
            .map_err(|_| connection("write failed"))?;
        self.pending.insert(task, request);
        Ok(task)
    }
    fn keep_ready(&mut self, body: Vec<u8>) -> Result<(), Failure> {
        if self.ready.len() >= MAX_PENDING || self.ready_bytes + body.len() > MAX_PENDING_BYTES {
            return Err(Failure::Internal("service response buffer full".into()));
        }
        self.ready_bytes += body.len();
        self.ready.push_back(body);
        Ok(())
    }
    pub async fn receive(&mut self, wait: Duration) -> Result<Option<Vec<u8>>, Failure> {
        if let Some(body) = self.ready.pop_front() {
            self.ready_bytes -= body.len();
            return Ok(Some(body));
        }
        let (_, response) = match timeout(wait, self.receive_one()).await {
            Ok(result) => result?,
            Err(_) => return Ok(None),
        };
        match response {
            Response::Body(body) => Ok(Some(body)),
            Response::Unknown => Ok(None),
            Response::Error(code @ (5004 | 5011), request)
                if request.mode == Mode::Ordinary && request.attempt == 0 =>
            {
                self.recover(code, request).await?;
                Ok(None)
            }
            Response::Error(code, _) => Err(Failure::Service(code)),
        }
    }
    async fn receive_one(&mut self) -> Result<(u64, Response), Failure> {
        loop {
            let incoming = self
                .socket
                .next()
                .await
                .ok_or_else(|| connection("socket closed"))?
                .map_err(|_| connection("read failed"))?;
            if incoming.is_ping() || incoming.is_pong() {
                self.socket
                    .flush()
                    .await
                    .map_err(|_| connection("write failed"))?;
                continue;
            }
            if incoming.is_close() {
                return Err(connection("socket closed"));
            }
            if !incoming.is_binary() {
                return Err(malformed());
            }
            let data = incoming.into_payload();
            let outer = decode(&data)?;
            if !outer.iter().any(|(n, _)| *n == 4) {
                return Err(malformed());
            }
            let task = int(&outer, 2)?;
            let Some(request) = self.pending.remove(&task) else {
                return Ok((task, Response::Unknown));
            };
            let http = decode(bytes(&outer, 4)?)?;
            let status = int(&http, 2)? as i64;
            let mut headers = BTreeMap::new();
            for (_, field) in http.iter().filter(|(n, _)| *n == 4) {
                let Bytes(data) = field else {
                    return Err(malformed());
                };
                let h = decode(data)?;
                headers.insert(text(&h, 1)?.to_ascii_lowercase(), text(&h, 2)?.to_owned());
            }
            let mut code = if status == 200 {
                headers
                    .get("error")
                    .map(|v| v.parse::<i64>().map_err(|_| malformed()))
                    .transpose()?
                    .unwrap_or(0)
            } else {
                status
            };
            if code == 200 {
                code = 0;
            }
            if headers.contains_key("kb-noretry") {
                code = -503;
            }
            if code != 0 && status == 200 && matches!(request.mode, Mode::GenUin(_)) {
                return Err(Failure::Rejected(code));
            }
            if code != 0 {
                return Ok((task, Response::Error(code, request)));
            }
            let mut body = bytes(&http, 5)?.to_vec();
            if !request.key.is_empty() && !body.is_empty() {
                body = crypto::decrypt(&body, &request.key)?;
            }
            if !request.key.is_empty()
                && request.mode == Mode::Ordinary
                && headers.get("kb-compressiontype").is_some_and(|v| v == "2")
            {
                let length = snap::raw::decompress_len(&body).map_err(|_| malformed())?;
                if length > wire::MAX_MESSAGE {
                    return Err(malformed());
                }
                body = snap::raw::Decoder::new()
                    .decompress_vec(&body)
                    .map_err(|_| malformed())?;
            }
            return Ok((task, Response::Body(body)));
        }
    }
    async fn recover(&mut self, code: i64, first: Request) -> Result<(), Failure> {
        let mut retries = vec![first];
        let mut needs_dh = code == 5011;
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        while !self.pending.is_empty() {
            match timeout(
                deadline.saturating_duration_since(Instant::now()),
                self.receive_one(),
            )
            .await
            {
                Err(_) => break,
                Ok(Err(failure)) => return Err(failure),
                Ok(Ok((_, Response::Error(error @ (5004 | 5011), request)))) => {
                    needs_dh |= error == 5011;
                    retries.push(request);
                }
                Ok(Ok((_, Response::Error(error, _)))) => return Err(Failure::Service(error)),
                Ok(Ok((_, Response::Body(body)))) => self.keep_ready(body)?,
                Ok(Ok((_, Response::Unknown))) => {}
            }
        }
        retries.extend(std::mem::take(&mut self.pending).into_values());
        if retries
            .iter()
            .any(|r| r.attempt >= 1 || r.mode != Mode::Ordinary)
        {
            return Err(Failure::Service(code));
        }
        if needs_dh {
            self.handshake().await?;
        } else {
            self.calibrate().await?;
        }
        for mut request in retries {
            request.attempt += 1;
            request.key = self.key.clone();
            self.send_request(request).await?;
        }
        Ok(())
    }
}

async fn open(endpoint: Endpoint) -> Result<Socket, Failure> {
    let url = url::Url::parse(&endpoint.url).map_err(|_| connection("invalid endpoint"))?;
    let host = url
        .host_str()
        .ok_or_else(|| connection("invalid endpoint"))?
        .trim_matches(['[', ']']);
    let port = url
        .port_or_known_default()
        .ok_or_else(|| connection("invalid endpoint"))?;
    let name = host.to_owned();
    let lookup = move || {
        (name.as_str(), port)
            .to_socket_addrs()
            .map(|addresses| addresses.collect())
            .map_err(|_| connection("DNS failed"))
    };
    #[cfg(test)]
    let lookup = endpoint.lookup.unwrap_or_else(|| Box::new(lookup));
    let addresses = resolve(lookup).await?;
    let mut stream = None;
    for address in addresses {
        if let Ok(Ok(tcp)) = timeout(Duration::from_secs(2), TcpStream::connect(address)).await {
            stream = Some(tcp);
            break;
        }
    }
    let tcp = stream.ok_or_else(|| connection("TCP failed"))?;
    tcp.set_nodelay(true)
        .map_err(|_| connection("socket setup failed"))?;
    let connector = if url.scheme() == "wss" {
        Connector::new().map_err(|_| connection("TLS setup failed"))?
    } else {
        Connector::Plain
    };
    let stream = connector
        .wrap(host, tcp)
        .await
        .map_err(|_| connection("TLS handshake failed"))?;
    // This transport has no body/URL logging, including through the log facade.
    let builder = ClientBuilder::new()
        .uri(&endpoint.url)
        .map_err(|_| connection("invalid endpoint"))?
        .limits(Limits::default().max_payload_len(Some(wire::MAX_MESSAGE)))
        .add_header(
            "Sec-WebSocket-Protocol".parse().map_err(|_| malformed())?,
            "wxws_pb".parse().map_err(|_| malformed())?,
        )
        .map_err(|_| malformed())?;
    let (socket, reply) = builder
        .connect_on(stream)
        .await
        .map_err(|_| connection("WebSocket handshake failed"))?;
    if reply
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        != Some("wxws_pb")
    {
        return Err(connection("WebSocket protocol mismatch"));
    }
    Ok(socket)
}
/// Keep system DNS outside Tokio's blocking pool: cancellation may abandon a
/// lookup without delaying runtime shutdown. The lookup captures no identity.
async fn resolve(
    lookup: impl FnOnce() -> Result<Vec<SocketAddr>, Failure> + Send + 'static,
) -> Result<Vec<SocketAddr>, Failure> {
    let (reply, result) = oneshot::channel();
    std::thread::Builder::new()
        .name("voice-buddy-dns".into())
        .spawn(move || {
            let _ = reply.send(lookup());
        })
        .map_err(|_| connection("DNS worker unavailable"))?;
    result.await.map_err(|_| connection("DNS worker stopped"))?
}
fn encode_request(
    cfg: &Config,
    request: &Request,
    task: u64,
    stamp: u64,
) -> Result<Vec<u8>, Failure> {
    let compression = if request.mode == Mode::Ordinary && request.body.len() >= 301 {
        2
    } else {
        1
    };
    let mut body = if compression == 2 {
        snap::raw::Encoder::new()
            .compress_vec(&request.body)
            .map_err(|_| malformed())?
    } else {
        request.body.clone()
    };
    if !request.key.is_empty() {
        body = crypto::encrypt(&body, &request.key)?;
    }
    let mut random = [0; 16];
    crypto::random(&mut random)?;
    let trace: String = random
        .into_iter()
        .map(|b| char::from(b'a' + b % 26))
        .collect();
    let command = if request.mode == Mode::Ordinary {
        "4548"
    } else {
        "0"
    };
    let mut values: BTreeMap<&str, String> = BTreeMap::from([
        ("Kb-Uin", cfg.number("uin").to_string()),
        ("Kb-CmdId", command.into()),
        ("Kb-SubCmdId", "0".into()),
        ("Kb-OsType", cfg.number("os_type").to_string()),
        ("Kb-Version", cfg.text("version").into()),
        ("Kb-SystemVersion", cfg.text("system_version").into()),
        ("Kb-PackageType", cfg.number("package_type").to_string()),
        ("Kb-TimeStamp", stamp.to_string()),
        ("Kb-BodyMd5", hex::encode_upper(Md5::digest(&body))),
        ("Kb-TraceId", trace),
        ("Kb-TaskId", task.to_string()),
        ("Kb-Scene", cfg.number("scene").to_string()),
        ("Kb-CompressionType", compression.to_string()),
        ("Content-Type", "application/octet-stream".into()),
        ("Content-Length", body.len().to_string()),
        ("HOST", cfg.host().into()),
    ]);
    let fields: &[&str] = if request.mode == Mode::Dh {
        // The evolved client omits CmdId for DH, but still signs SubCmdId nowhere.
        values.remove("Kb-CmdId");
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
        values.insert(
            "Kb-SharedKeySuffix",
            request
                .key
                .chars()
                .rev()
                .take(4)
                .collect::<String>()
                .chars()
                .rev()
                .collect(),
        );
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
    // Registration signs the token between the body hash and trace ID;
    // CmdId/SubCmdId/UIN remain headers but are excluded from its signature.
    let fields = if let Mode::GenUin(token) = &request.mode {
        values.insert("Kb-GenUinToken", token.to_string());
        &[
            "Kb-OsType",
            "Kb-Version",
            "Kb-Scene",
            "Kb-TimeStamp",
            "Kb-BodyMd5",
            "Kb-GenUinToken",
            "Kb-TraceId",
            "Kb-TaskId",
        ][..]
    } else {
        fields
    };
    let mut signed = Zeroizing::new(String::new());
    for field in fields {
        signed.push_str(&values[*field]);
    }
    signed.push_str(cfg.text("sign_key"));
    values.insert(
        "Kb-Sign",
        hex::encode_upper(Sha256::digest(signed.as_bytes())),
    );
    if request.mode == Mode::Dh {
        values.insert("Kb-DeviceCodeRestrictionV2", "1".into());
    }
    if cfg.durable.contains_key("use_debug_net") {
        values.insert("Use_DebugNet", cfg.text("use_debug_net").into());
    }
    let path = match &request.mode {
        Mode::Ordinary => "/api_v2",
        Mode::Dh => "/oauth_pubkey_v2",
        Mode::GenUin(_) => "/gen_uin_v2",
        Mode::Timestamp => "/timestamp",
    };
    let mut http = message(&[
        (1, Bytes(b"POST")),
        (3, Bytes(path.as_bytes())),
        (4, Bytes(b"")),
    ]);
    for (name, value) in values {
        let header = message(&[(1, Bytes(name.as_bytes())), (2, Bytes(value.as_bytes()))]);
        http.extend(message(&[(5, Bytes(&header))]));
    }
    http.extend(message(&[(6, Bytes(&body))]));
    Ok(message(&[
        (1, Int(0)),
        (2, Int(0)),
        (3, Int(task)),
        (5, Bytes(&http)),
    ]))
}
pub(crate) fn voice_body(
    id: &str,
    audio: &[u8],
    sequence: u64,
    total: u64,
    end: bool,
    options: Options,
) -> Vec<u8> {
    let mut fields = vec![(2, Bytes(id.as_bytes()))];
    if !audio.is_empty() {
        fields.push((4, Bytes(audio)));
    }
    fields.push((5, Int(5)));
    for (n, v) in [
        (6, u64::from(end)),
        (7, sequence),
        (9, u64::from(options.traditional)),
        (11, total),
        (22, u64::from(options.paragraph)),
        (23, u64::from(options.reduce_redundancy)),
        (24, u64::from(options.polish)),
    ] {
        if v != 0 {
            fields.push((n, Int(v)));
        }
    }
    message(&[(1, Bytes(&message(&fields)))])
}
#[allow(
    clippy::disallowed_methods,
    reason = "The wire protocol needs Unix milliseconds before clock calibration"
)]
pub(crate) fn wall_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn connection(why: &str) -> Failure {
    Failure::Connection(why.into())
}
fn initialization_uuid() -> Result<Zeroizing<String>, Failure> {
    let mut random = Zeroizing::new([0; 16]);
    crypto::random(random.as_mut())?;
    random[6] = (random[6] & 0x0f) | 0x40;
    random[8] = (random[8] & 0x3f) | 0x80;
    let hex = Zeroizing::new(hex::encode_upper(random.as_slice()));
    Ok(Zeroizing::new(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )))
}
