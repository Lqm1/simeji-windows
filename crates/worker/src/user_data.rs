//! The user's files under `%LOCALAPPDATA%\SimejiWindows`.

use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const PRODUCT_DIRECTORY: &str = "SimejiWindows";
const SETTINGS_FILE: &str = "settings.json";
const USER_DICTIONARY_FILE: &str = "user-dictionary.json";
const LEARNING_DIRECTORY: &str = "Learning";
const CACHE_DIRECTORY: &str = "Cache";
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug)]
pub struct UserData {
    root: PathBuf,
}

impl UserData {
    /// The signed-in user's data directory.
    pub fn locate() -> Option<Self> {
        std::env::var_os("LOCALAPPDATA")
            .map(|local| Self::at(Path::new(&local).join(PRODUCT_DIRECTORY)))
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn settings_file(&self) -> PathBuf {
        self.root.join(SETTINGS_FILE)
    }

    pub fn user_dictionary_file(&self) -> PathBuf {
        self.root.join(USER_DICTIONARY_FILE)
    }

    pub fn learning_directory(&self) -> PathBuf {
        self.root.join(LEARNING_DIRECTORY)
    }

    /// Data that can be deleted at any time.
    pub fn cache_directory(&self) -> PathBuf {
        self.root.join(CACHE_DIRECTORY)
    }

    /// The saved settings, or the defaults if there are none or they are unreadable.
    pub fn settings(&self) -> Settings {
        fs::metadata(self.settings_file())
            .ok()
            .filter(|metadata| metadata.len() <= MAX_SETTINGS_BYTES)
            .and_then(|_| fs::read(self.settings_file()).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), Box<dyn Error + Send + Sync>> {
        write_atomic(&self.settings_file(), &serde_json::to_vec_pretty(settings)?)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Sends readings to Simeji's cloud conversion. Off unless the user enables it.
    pub cloud_conversion: bool,
}

/// Replaces `path` with `bytes` so that readers never see a partial file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error + Send + Sync>> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    let temporary = path.with_extension("tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_to_offline_and_round_trip() {
        let data = UserData::at(
            std::env::temp_dir().join(format!("simeji-settings-test-{}", std::process::id())),
        );
        assert_eq!(data.settings(), Settings::default());
        assert!(!data.settings().cloud_conversion);
        data.save_settings(&Settings {
            cloud_conversion: true,
        })
        .unwrap();
        assert!(data.settings().cloud_conversion);
        fs::write(data.settings_file(), b"{\"cloud_conversion\": \"yes\"}").unwrap();
        assert!(!data.settings().cloud_conversion);
        fs::remove_dir_all(data.root()).unwrap();
    }
}
