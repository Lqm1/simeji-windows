//! Persistent user dictionary entries, replayed into the native engine on open.

use crate::arm::ArmError;
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

/// The saved entries. The native dictionary itself is not persisted, so the
/// engine registers these again every time it starts.
pub(crate) struct UserDictionary {
    path: PathBuf,
    entries: Vec<Entry>,
}

impl UserDictionary {
    pub fn load(learning_directory: &Path) -> Result<Self, ArmError> {
        let path = learning_directory.join(FILE_NAME);
        let entries = if path.exists() {
            if std::fs::metadata(&path)?.len() > MAX_BYTES {
                return Err(ArmError::new("User dictionary is too large"));
            }
            let bytes = std::fs::read(&path)?;
            serde_json::from_slice(&bytes)
                .map_err(|e| ArmError::new(format!("Invalid user dictionary: {e}")))?
        } else {
            Vec::new()
        };
        Ok(Self { path, entries })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Adds an entry unless it already exists and atomically rewrites the file.
    pub fn insert(&mut self, reading: &str, text: &str) -> Result<(), ArmError> {
        let entry = Entry {
            reading: reading.to_owned(),
            text: text.to_owned(),
        };
        let mut entries = self.entries.clone();
        if !entries.contains(&entry) {
            entries.push(entry);
        }
        let bytes = serde_json::to_vec(&entries).map_err(|e| ArmError::new(e.to_string()))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(ArmError::new("User dictionary is too large"));
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
