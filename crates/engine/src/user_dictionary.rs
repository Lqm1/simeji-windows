//! Persistent user dictionary entries, replayed into the native engine on open.

use crate::EngineError;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

const FILE_NAME: &str = "user-dictionary.json";
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct Entry {
    pub reading: String,
    pub text: String,
}

/// The native user dictionary is not persisted, so the engine registers
/// these entries again every time it starts.
pub(crate) struct UserDictionary {
    path: PathBuf,
    entries: Vec<Entry>,
}

impl UserDictionary {
    pub fn load(user_data: &Path) -> Result<Self, EngineError> {
        let path = user_data.join(FILE_NAME);
        let entries = if path.exists() {
            if std::fs::metadata(&path)?.len() > MAX_BYTES {
                return Err(EngineError::new("User dictionary is too large"));
            }
            let bytes = std::fs::read(&path)?;
            serde_json::from_slice(&bytes)
                .map_err(|e| EngineError::new(format!("Invalid user dictionary: {e}")))?
        } else {
            Vec::new()
        };
        Ok(Self { path, entries })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn insert(&mut self, reading: &str, text: &str) -> Result<(), EngineError> {
        let entry = Entry {
            reading: reading.to_owned(),
            text: text.to_owned(),
        };
        let mut entries = self.entries.clone();
        if !entries.contains(&entry) {
            entries.push(entry);
        }
        let bytes = serde_json::to_vec(&entries).map_err(|e| EngineError::new(e.to_string()))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(EngineError::new("User dictionary is too large"));
        }
        let temporary = self.path.with_extension("json.tmp");
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &self.path)?;
        self.entries = entries;
        Ok(())
    }
}
