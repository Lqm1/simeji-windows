//! Candidates from Simeji's cloud conversion API, with a persistent cache.

use super::MAX_READING_CHARS;
use crate::user_data::{UserData, write_atomic};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use simeji_protocol::CloudCandidate;
use std::{
    collections::VecDeque,
    error::Error,
    fs,
    io::Read,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const ENDPOINT: &str = "https://cloud.simeji.me/py";
const CACHE_FILE: &str = "cloud.json";
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 800;
const MAX_CANDIDATES: usize = 50;
const MAX_CANDIDATE_BYTES: usize = 4096;
const ATTEMPTS: usize = 3;
const TIMEOUT: Duration = Duration::from_secs(3);
type CloudResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Deserialize)]
struct ApiResponse {
    data: Vec<ApiReading>,
}

#[derive(Deserialize)]
struct ApiReading {
    hira: String,
    #[serde(default)]
    cache_time: u64,
    candidates: Vec<ApiCandidate>,
}

#[derive(Deserialize)]
struct ApiCandidate {
    word: String,
    #[serde(default)]
    pron: String,
    #[serde(default)]
    prop: u32,
    #[serde(default, rename = "type")]
    kind: u32,
    #[serde(default)]
    extra: Option<ApiExtra>,
}

#[derive(Default, Deserialize)]
struct ApiExtra {
    #[serde(default)]
    scene: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct CacheEntry {
    reading: String,
    stored: u64,
    ttl: u64,
    candidates: Vec<CloudCandidate>,
}

impl CacheEntry {
    fn is_fresh(&self, now: u64) -> bool {
        now >= self.stored && now - self.stored < self.ttl
    }
}

/// Keeps the first occurrence of each text, up to [`MAX_CANDIDATES`], and
/// the shortest cache lifetime of the response.
fn parse_response(bytes: &[u8], query: &str, now: u64) -> CloudResult<CacheEntry> {
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("Cloud response exceeds limit".into());
    }
    let response: ApiResponse = serde_json::from_slice(bytes)?;
    let mut candidates = Vec::new();
    let mut ttl = u64::MAX;
    'readings: for reading in response.data {
        ttl = ttl.min(reading.cache_time);
        for candidate in reading.candidates {
            if candidate.word.is_empty()
                || candidate.word.len() > MAX_CANDIDATE_BYTES
                || candidates
                    .iter()
                    .any(|known: &CloudCandidate| known.text == candidate.word)
            {
                continue;
            }
            candidates.push(CloudCandidate {
                text: candidate.word,
                reading: if candidate.pron.is_empty() {
                    reading.hira.clone()
                } else {
                    candidate.pron
                },
                property: candidate.prop,
                kind: candidate.kind,
                scene: candidate.extra.unwrap_or_default().scene,
            });
            if candidates.len() == MAX_CANDIDATES {
                break 'readings;
            }
        }
    }
    Ok(CacheEntry {
        reading: query.into(),
        stored: now,
        ttl: if ttl == u64::MAX { 0 } else { ttl },
        candidates,
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub struct CloudClient {
    user_data: UserData,
    enabled: bool,
    cache: VecDeque<CacheEntry>,
    http: Option<Client>,
}

impl CloudClient {
    pub fn open(user_data: &UserData) -> CloudResult<Self> {
        let cache = match fs::File::open(Self::cache_file(user_data)) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_CACHE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() <= MAX_CACHE_BYTES {
                    serde_json::from_slice(&bytes).unwrap_or_default()
                } else {
                    VecDeque::new()
                }
            }
            Err(_) => VecDeque::new(),
        };
        Ok(Self {
            enabled: user_data.settings().cloud_conversion,
            user_data: user_data.clone(),
            cache,
            http: None,
        })
    }

    fn cache_file(user_data: &UserData) -> PathBuf {
        user_data.cache_directory().join(CACHE_FILE)
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enabling authorises sending readings to [`ENDPOINT`].
    pub fn set_enabled(&mut self, enabled: bool) -> CloudResult<()> {
        let mut settings = self.user_data.settings();
        settings.cloud_conversion = enabled;
        self.user_data.save_settings(&settings)?;
        self.enabled = enabled;
        Ok(())
    }

    /// Cached candidates for `reading`; expired ones only with `allow_stale`.
    pub fn cached(&mut self, reading: &str, allow_stale: bool) -> Vec<CloudCandidate> {
        if !self.enabled {
            return Vec::new();
        }
        let Some(index) = self.cache.iter().position(|entry| entry.reading == reading) else {
            return Vec::new();
        };
        let Some(entry) = self.cache.remove(index) else {
            return Vec::new();
        };
        let candidates = if allow_stale || entry.is_fresh(unix_now()) {
            entry.candidates.clone()
        } else {
            Vec::new()
        };
        self.cache.push_back(entry);
        candidates
    }

    /// Fresh cached candidates, else a lookup with retries, else stale ones.
    pub fn query(&mut self, reading: &str) -> CloudResult<Vec<CloudCandidate>> {
        if !self.enabled {
            return Err(
                "Cloud conversion is disabled; explicitly enable it before querying".into(),
            );
        }
        if reading.is_empty() || reading.chars().count() > MAX_READING_CHARS {
            return Err("Invalid cloud reading length".into());
        }
        if self
            .cache
            .iter()
            .any(|entry| entry.reading == reading && entry.is_fresh(unix_now()))
        {
            return Ok(self.cached(reading, false));
        }
        let http = match &self.http {
            Some(http) => http.clone(),
            None => {
                let http = Client::builder()
                    .https_only(true)
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(TIMEOUT)
                    .timeout(TIMEOUT)
                    .build()?;
                self.http = Some(http.clone());
                http
            }
        };
        let mut last_error = None;
        for _ in 0..ATTEMPTS {
            match fetch(&http, reading)
                .and_then(|bytes| parse_response(&bytes, reading, unix_now()))
            {
                Ok(entry) => {
                    let candidates = entry.candidates.clone();
                    self.store(entry)?;
                    return Ok(candidates);
                }
                Err(error) => last_error = Some(error),
            }
        }
        let stale = self.cached(reading, true);
        if !stale.is_empty() {
            return Ok(stale);
        }
        Err(last_error.unwrap_or_else(|| "Cloud conversion failed".into()))
    }

    /// Adds the entry, evicting the oldest ones beyond the cache limits. A
    /// failure to save the cache does not fail the lookup.
    fn store(&mut self, entry: CacheEntry) -> CloudResult<()> {
        self.cache.retain(|cached| cached.reading != entry.reading);
        self.cache.push_back(entry);
        while self.cache.len() > MAX_CACHE_ENTRIES {
            self.cache.pop_front();
        }
        let mut bytes = serde_json::to_vec(&self.cache)?;
        while bytes.len() > MAX_CACHE_BYTES {
            self.cache.pop_front();
            bytes = serde_json::to_vec(&self.cache)?;
        }
        let _ = write_atomic(&Self::cache_file(&self.user_data), &bytes);
        Ok(())
    }
}

/// The request parameters of Simeji 23.6.1 for Android.
fn parameters(query: &str) -> [(&'static str, &str); 17] {
    [
        ("web", "1"),
        ("ol", "1"),
        ("fmt", "JSON"),
        ("version", "966"),
        ("app", "simeji.windows"),
        ("py", query),
        ("api_version", "2"),
        ("switch", "20"),
        ("os_version", "14"),
        ("section", "1"),
        ("device", "android"),
        ("version_name", "23.6.1"),
        ("version_code", "966"),
        ("system_version", "34"),
        ("app_version", "966"),
        ("channel", "organic"),
        ("product", "Simeji"),
    ]
}

fn fetch(http: &Client, query: &str) -> CloudResult<Vec<u8>> {
    let response = http
        .get(ENDPOINT)
        .query(&parameters(query))
        .header("Connection", "keep-alive")
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("Cloud response exceeds limit".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_mapping_dedup_and_ttl_seconds() -> CloudResult<()> {
        let entry = parse_response(
            br#"{"data":[{"hira":"neko","cache_time":3,"candidates":[{"word":"cat","type":1,"prop":4},{"word":"cat"},{"word":"kitty","pron":"kitty","extra":{"scene":"chat"}}]}]}"#,
            "query",
            100,
        )?;
        assert_eq!(entry.candidates.len(), 2);
        assert_eq!(entry.candidates[0].reading, "neko");
        assert_eq!(entry.candidates[1].scene, "chat");
        assert!(entry.is_fresh(102));
        assert!(!entry.is_fresh(103));
        assert!(!entry.is_fresh(99));
        Ok(())
    }

    #[test]
    fn disabled_blocks_network_and_persists_opt_in() -> CloudResult<()> {
        let user_data = UserData::at(std::env::temp_dir().join(format!(
            "simeji-cloud-test-{}-{}",
            std::process::id(),
            unix_now()
        )));
        let mut cloud = CloudClient::open(&user_data)?;
        assert!(!cloud.is_enabled());
        assert!(cloud.query("synthetic").is_err());
        assert!(cloud.http.is_none());
        cloud.set_enabled(true)?;
        drop(cloud);
        let mut cloud = CloudClient::open(&user_data)?;
        assert!(cloud.is_enabled());
        cloud.cache.push_back(parse_response(
            br#"{"data":[{"hira":"x","cache_time":30,"candidates":[{"word":"cached"}]}]}"#,
            "x",
            0,
        )?);
        assert!(cloud.cached("x", false).is_empty());
        assert_eq!(cloud.cached("x", true)[0].text, "cached");
        cloud.set_enabled(false)?;
        assert!(cloud.cached("x", true).is_empty());
        fs::remove_dir_all(user_data.root())?;
        Ok(())
    }
}
