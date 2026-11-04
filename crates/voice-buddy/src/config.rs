use crate::{Config, ConfigError, Options};
use serde_json::{Map, Value};

const MAX_BYTES: usize = 1_048_576;
const STRINGS: &[&str] = &[
    "host",
    "device_code",
    "device_token",
    "device_model_id",
    "user_device_name",
    "version",
    "system_version",
    "sign_key",
    "bootstrap_aes_key",
];

pub(crate) fn parse(bytes: &[u8]) -> Result<Config, ConfigError> {
    if bytes.len() > MAX_BYTES {
        return Err(error("file size"));
    }
    let root: Value = serde_json::from_slice(bytes).map_err(|_| error("JSON"))?;
    // Accept the durable context itself and an exported result's configuration.
    let value = root.get("configuration").unwrap_or(&root);
    let source = value.as_object().ok_or_else(|| error("configuration"))?;
    let mut durable = Map::new();
    for &field in STRINGS {
        let text = source
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| error(field))?;
        if text.chars().any(char::is_control) || text.len() > 16_384 {
            return Err(error(field));
        }
        if ["host", "device_code", "version", "sign_key"].contains(&field) && text.is_empty() {
            return Err(error(field));
        }
        durable.insert(field.into(), text.into());
    }
    let key = durable["bootstrap_aes_key"].as_str().unwrap_or_default();
    if ![16, 24, 32].contains(&key.len()) {
        return Err(error("bootstrap_aes_key"));
    }
    for field in ["uin", "os_type", "package_type", "scene"] {
        let number = source
            .get(field)
            .and_then(unsigned)
            .ok_or_else(|| error(field))?;
        if field == "uin" && number == 0 {
            return Err(error("uin"));
        }
        durable.insert(field.into(), number.into());
    }
    let host = durable["host"].as_str().unwrap_or_default();
    let url = url::Url::parse(&format!("wss://{host}")).map_err(|_| error("host"))?;
    if host.trim() != host
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || host.contains(['/', '?', '#', '@', '\\'])
    {
        return Err(error("host"));
    }
    if let Some(path) = source.get("websocket_path") {
        if !matches!(path.as_str(), Some("" | "/ppp")) {
            return Err(error("websocket_path"));
        }
        durable.insert("websocket_path".into(), path.clone());
    }
    if let Some(debug) = source.get("use_debug_net") {
        let text = debug.as_str().ok_or_else(|| error("use_debug_net"))?;
        if text.chars().any(char::is_control) || text.len() > 1024 {
            return Err(error("use_debug_net"));
        }
        durable.insert("use_debug_net".into(), debug.clone());
    }
    let mut options = Options::default();
    if let Some(voice) = source.get("voice_options") {
        let voice = voice.as_object().ok_or_else(|| error("voice_options"))?;
        let mut kept = Map::new();
        for (name, target) in [
            ("polish", &mut options.polish),
            ("traditional", &mut options.traditional),
            ("paragraph", &mut options.paragraph),
            ("reduceRedundancy", &mut options.reduce_redundancy),
        ] {
            if let Some(value) = voice.get(name) {
                *target = value.as_bool().ok_or_else(|| error(name))?;
                kept.insert(name.into(), value.clone());
            }
        }
        for (name, target) in [
            ("phaseA", &mut options.final_deadline_ms),
            ("phaseB", &mut options.polish_deadline_ms),
        ] {
            if let Some(value) = voice.get(name) {
                let ms = value
                    .as_f64()
                    .filter(|n| n.is_finite() && *n > 0.0 && *n <= 60_000.0)
                    .ok_or_else(|| error(name))?;
                *target = ms.ceil() as u32;
                kept.insert(name.into(), Value::from(*target));
            }
        }
        durable.insert("voice_options".into(), kept.into());
    }
    Ok(Config { durable, options })
}

fn unsigned(value: &Value) -> Option<u64> {
    if let Some(n) = value.as_u64() {
        return Some(n);
    }
    if let Some(s) = value.as_str() {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        return s.parse().ok();
    }
    let n = value.as_f64()?;
    (n.is_finite() && (0.0..18_446_744_073_709_551_616.0).contains(&n) && n.fract() == 0.0)
        .then_some(n as u64)
}

fn error(field: &str) -> ConfigError {
    ConfigError(format!("invalid {field}"))
}

impl Config {
    pub(crate) fn text(&self, field: &str) -> &str {
        self.durable
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
    pub(crate) fn number(&self, field: &str) -> u64 {
        self.durable
            .get(field)
            .and_then(Value::as_u64)
            .unwrap_or_default()
    }
}
