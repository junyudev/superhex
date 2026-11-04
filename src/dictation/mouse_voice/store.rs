//! The only durable MouseVoice state. Reading another app is always explicit.
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mouse_voice::Config;

use super::super::backend::{FoundConfig, Setup};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const REJECTED: &str = "MouseVoice refused this configuration";

#[derive(Clone)]
pub(super) struct Store {
    pub config: Option<Arc<Config>>,
    pub setup: Setup,
    path: PathBuf,
}

impl Store {
    pub fn load(path: PathBuf) -> Self {
        let mut store = Self {
            config: None,
            setup: Setup::Missing,
            path,
        };
        if fs::symlink_metadata(&store.path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return store;
        }
        match read_config(&store.path) {
            Ok((config, rejected)) => store.set(config, rejected),
            Err(error) => store.setup = Setup::Broken { reason: error },
        }
        store
    }

    fn set(&mut self, config: Config, rejected: bool) {
        self.setup = if rejected {
            Setup::Broken {
                reason: REJECTED.into(),
            }
        } else {
            Setup::Ready {
                host: config.host().into(),
            }
        };
        self.config = Some(Arc::new(config));
    }

    pub fn usable(&self) -> Option<Arc<Config>> {
        if !matches!(self.setup, Setup::Ready { .. }) {
            return None;
        }
        self.config.clone()
    }

    #[cfg(test)]
    pub fn import(&mut self, source: &Path) -> Result<mouse_voice::Options, String> {
        let (config, _) = read_config(source)?;
        self.import_config(config)
    }

    pub fn import_config(&mut self, config: Config) -> Result<mouse_voice::Options, String> {
        let options = config.options();
        self.save(&config, false)?;
        self.set(config, false);
        Ok(options)
    }

    fn save(&self, config: &Config, rejected: bool) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or("The configuration folder is unavailable")?;
        fs::create_dir_all(parent).map_err(|_| "Could not create the configuration folder")?;
        if fs::symlink_metadata(parent).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err("The configuration folder must not be a symbolic link".into());
        }
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Could not make the configuration folder private")?;
        let mut value: serde_json::Value = serde_json::from_slice(&config.to_json())
            .map_err(|_| "Could not encode the configuration")?;
        value
            .as_object_mut()
            .ok_or("Could not encode the configuration")?
            .insert("hex_rejected".into(), rejected.into());
        let bytes = serde_json::to_vec(&value).map_err(|_| "Could not encode the configuration")?;
        crate::durable::write_private(&self.path, &bytes)
            .map_err(|_| "Could not save the configuration privately".into())
    }

    pub fn reject(&mut self, config: &Arc<Config>) {
        let Some(saved) = &self.config else { return };
        if !Arc::ptr_eq(saved, config) {
            return;
        }
        self.setup = Setup::Broken {
            reason: REJECTED.into(),
        };
    }

    /// Called by the finishing job, never by the authoritative audio owner.
    pub fn persist_rejection(&self, config: &Arc<Config>) {
        if !self
            .config
            .as_ref()
            .is_some_and(|saved| Arc::ptr_eq(saved, config))
            || !matches!(self.setup, Setup::Broken { .. })
        {
            return;
        }
        if self.save(config, true).is_err() {
            tracing::warn!("could not persist the speech service refusal");
        }
    }

    pub fn forget(&mut self) -> Result<(), String> {
        match fs::remove_file(&self.path) {
            Ok(()) => {
                crate::durable::sync_directory(self.path.parent().unwrap())
                    .map_err(|_| "Could not forget the configuration")?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Could not forget the configuration".into()),
        }
        self.config = None;
        self.setup = Setup::Missing;
        Ok(())
    }
}

/// Refuse a symlink as the file itself, and devices/FIFOs, before reading.
/// Folders on the way may be links: macOS's own `/tmp` and `/var` are. The extra byte bounds a file that grows after its metadata read.
pub(super) fn read_config(path: &Path) -> Result<(Config, bool), String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "Could not open a regular configuration file")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Could not read the configuration")?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
        return Err("The configuration must be a regular file no larger than 1 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the configuration")?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("The configuration is larger than 1 MiB".into());
    }
    let config = Config::parse(&bytes).map_err(|error| error.to_string())?;
    let rejected = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value.get("hex_rejected").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    Ok((config, rejected))
}

pub(super) fn found_in(home: &Path) -> Vec<FoundConfig> {
    let path = home.join("Library/Application Support/MouseVoice/context.json");
    let Ok((config, _)) = read_config(&path) else {
        return Vec::new();
    };
    let host = config.host().into();
    vec![FoundConfig { path, host }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    fn directory() -> PathBuf {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("hex-mouse-store-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn rejected_imports_leave_the_existing_file_and_state_alone() {
        let dir = directory();
        let target = dir.join("mouse-voice.json");
        fs::write(&target, b"previous invalid file").unwrap();
        let mut store = Store::load(target.clone());
        let previous = store.setup.clone();
        for (name, bytes) in [
            ("bad.json", b"{}".to_vec()),
            ("large.json", vec![b' '; MAX_CONFIG_BYTES as usize + 1]),
        ] {
            let source = dir.join(name);
            fs::write(&source, &bytes).unwrap();
            assert!(store.import(&source).is_err());
            assert_eq!(fs::read(&source).unwrap(), bytes);
            assert_eq!(fs::read(&target).unwrap(), b"previous invalid file");
            assert_eq!(store.setup, previous);
        }
        store.forget().unwrap();
        assert_eq!(store.setup, Setup::Missing);
        assert!(!target.exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn a_configuration_cannot_be_a_symlink_but_its_folders_may_be() {
        let dir = directory();
        let regular = dir.join("regular.json");
        fs::write(&regular, b"{}").unwrap();
        let link = dir.join("link.json");
        symlink(&regular, &link).unwrap();
        assert_eq!(
            read_config(&link).err().unwrap(),
            "Could not open a regular configuration file"
        );
        let folder_link = dir.with_extension("link");
        symlink(&dir, &folder_link).unwrap();
        // Opened through the linked folder; only its contents are refused.
        assert_ne!(
            read_config(&folder_link.join("regular.json"))
                .err()
                .unwrap(),
            "Could not open a regular configuration file"
        );
        assert!(matches!(
            Store::load(link.clone()).setup,
            Setup::Broken { .. }
        ));
        assert!(crate::durable::write_private(&link, b"changed").is_err());
        assert_eq!(fs::read(&regular).unwrap(), b"{}");
        fs::remove_file(folder_link).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn private_writes_replace_broad_permissions_and_leave_no_staging_file() {
        let dir = directory();
        let target = dir.join("mouse-voice.json");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        crate::durable::write_private(&target, b"new").unwrap();
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn successful_import_is_private_copies_options_and_clears_a_refusal() {
        let dir = directory();
        let config = super::super::tests::configuration();
        let source = dir.join("context.json");
        let bytes = config.to_json();
        fs::write(&source, &bytes).unwrap();
        let target = dir.join("hex/mouse-voice.json");
        let mut store = Store::load(target.clone());
        let options = store.import(&source).unwrap();
        assert!(
            !options.polish
                && options.traditional
                && options.paragraph
                && options.reduce_redundancy
        );
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(target.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(fs::read(&source).unwrap(), bytes);
        let saved = store.usable().unwrap();
        store.reject(&saved);
        store.persist_rejection(&saved);
        assert!(store.usable().is_none());
        assert!(matches!(
            Store::load(target.clone()).setup,
            Setup::Broken { .. }
        ));
        fs::write(&source, b"{}").unwrap();
        assert!(store.import(&source).is_err());
        assert!(store.usable().is_none());
        fs::write(&source, &bytes).unwrap();
        store.import(&source).unwrap();
        // A late refusal from the old session cannot poison a new import.
        store.reject(&saved);
        assert!(matches!(store.setup, Setup::Ready { .. }));
        assert!(Store::load(target).usable().is_some());
        fs::remove_dir_all(dir).unwrap();
    }
}
