//! Text candidates from the application's cloud conversion protocol.
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    error::Error,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type CloudResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;

pub use simeji_protocol::CloudWord;

#[derive(Deserialize)]
struct Reply {
    data: Vec<Group>,
}
#[derive(Deserialize)]
struct Group {
    hira: String,
    #[serde(default)]
    cache_time: u64,
    candidates: Vec<Word>,
}
#[derive(Deserialize)]
struct Word {
    word: String,
    #[serde(default)]
    pron: String,
    #[serde(default)]
    prop: u32,
    #[serde(default, rename = "type")]
    kind: u32,
    #[serde(default)]
    extra: Option<Extra>,
}
#[derive(Default, Deserialize)]
struct Extra {
    #[serde(default)]
    scene: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    query: String,
    stored: u64,
    ttl: u64,
    words: Vec<CloudWord>,
}
impl Entry {
    fn fresh(&self, now: u64) -> bool {
        now >= self.stored && now - self.stored < self.ttl
    }
}

fn decode(bytes: &[u8], query: &str, now: u64) -> CloudResult<Entry> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Cloud response exceeds limit".into());
    }
    let reply: Reply = serde_json::from_slice(bytes)?;
    let mut words = Vec::new();
    let mut ttl = u64::MAX;
    for group in reply.data {
        ttl = ttl.min(group.cache_time);
        for word in group.candidates {
            if word.word.is_empty()
                || word.word.len() > 4096
                || words.iter().any(|w: &CloudWord| w.text == word.word)
            {
                continue;
            }
            words.push(CloudWord {
                text: word.word,
                reading: if word.pron.is_empty() {
                    group.hira.clone()
                } else {
                    word.pron
                },
                property: word.prop,
                kind: word.kind,
                scene: word.extra.unwrap_or_default().scene,
            });
            if words.len() == 50 {
                break;
            }
        }
        if words.len() == 50 {
            break;
        }
    }
    Ok(Entry {
        query: query.into(),
        stored: now,
        ttl: if ttl == u64::MAX { 0 } else { ttl },
        words,
    })
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn write_atomic(path: &Path, bytes: &[u8]) -> CloudResult<()> {
    use std::io::Write;
    let temporary = path.with_extension("tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

pub struct CloudClient {
    directory: PathBuf,
    enabled: bool,
    cache: VecDeque<Entry>,
    client: Option<Client>,
}
impl CloudClient {
    pub fn open(directory: &Path) -> CloudResult<Self> {
        fs::create_dir_all(directory)?;
        let enabled = match fs::read(directory.join("cloud-enabled.json")) {
            Ok(bytes) if bytes.len() <= 64 => {
                serde_json::from_slice::<bool>(&bytes).unwrap_or(false)
            }
            _ => false,
        };
        let cache = match fs::File::open(directory.join("cloud-cache.json")) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_CACHE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() <= MAX_CACHE_BYTES {
                    serde_json::from_slice::<VecDeque<Entry>>(&bytes).unwrap_or_default()
                } else {
                    VecDeque::new()
                }
            }
            Err(_) => VecDeque::new(),
        };
        Ok(Self {
            directory: directory.into(),
            enabled,
            cache,
            client: None,
        })
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    /// Enabling authorizes transmission of readings to cloud.simeji.me.
    pub fn set_enabled(&mut self, enabled: bool) -> CloudResult<()> {
        write_atomic(
            &self.directory.join("cloud-enabled.json"),
            if enabled { b"true" } else { b"false" },
        )?;
        self.enabled = enabled;
        Ok(())
    }
    pub fn cached(&mut self, query: &str, allow_stale: bool) -> Vec<CloudWord> {
        if !self.enabled {
            return Vec::new();
        }
        let Some(index) = self.cache.iter().position(|entry| entry.query == query) else {
            return Vec::new();
        };
        let entry = self.cache.remove(index).expect("entry position is valid");
        let words = if allow_stale || entry.fresh(now()) {
            entry.words.clone()
        } else {
            Vec::new()
        };
        self.cache.push_back(entry);
        words
    }
    pub fn query(&mut self, query: &str) -> CloudResult<Vec<CloudWord>> {
        if !self.enabled {
            return Err(
                "Cloud conversion is disabled; explicitly enable it before querying".into(),
            );
        }
        if query.is_empty() || query.chars().count() > 256 {
            return Err("Invalid cloud reading length".into());
        }
        if self
            .cache
            .iter()
            .any(|entry| entry.query == query && entry.fresh(now()))
        {
            return Ok(self.cached(query, false));
        }
        if self.client.is_none() {
            self.client = Some(
                Client::builder()
                    .https_only(true)
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(Duration::from_secs(3))
                    .timeout(Duration::from_secs(3))
                    .build()?,
            );
        }
        let client = self.client.as_ref().expect("client initialized");
        let mut last_error = None;
        for _ in 0..3 {
            match fetch(client, query).and_then(|bytes| decode(&bytes, query, now())) {
                Ok(entry) => {
                    let words = entry.words.clone();
                    self.cache.retain(|e| e.query != query);
                    self.cache.push_back(entry);
                    while self.cache.len() > 800 {
                        self.cache.pop_front();
                    }
                    let mut bytes = serde_json::to_vec(&self.cache)?;
                    while bytes.len() > MAX_CACHE_BYTES {
                        self.cache.pop_front();
                        bytes = serde_json::to_vec(&self.cache)?;
                    }
                    // Conversion still works if persistence becomes unavailable.
                    let _ = write_atomic(&self.directory.join("cloud-cache.json"), &bytes);
                    return Ok(words);
                }
                Err(error) => last_error = Some(error),
            }
        }
        let stale = self.cached(query, true);
        if !stale.is_empty() {
            return Ok(stale);
        }
        Err(last_error.unwrap_or_else(|| "Cloud conversion failed".into()))
    }
}

fn fetch(client: &Client, query: &str) -> CloudResult<Vec<u8>> {
    let response = client
        .get("https://cloud.simeji.me/py")
        .query(&[
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
        ])
        .header("Connection", "keep-alive")
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Cloud response exceeds limit".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_mapping_dedup_and_ttl_seconds() -> CloudResult<()> {
        let entry = decode(br#"{"data":[{"hira":"neko","cache_time":3,"candidates":[{"word":"cat","type":1,"prop":4},{"word":"cat"},{"word":"kitty","pron":"kitty","extra":{"scene":"chat"}}]}]}"#, "query", 100)?;
        assert_eq!(entry.words.len(), 2);
        assert_eq!(entry.words[0].reading, "neko");
        assert_eq!(entry.words[1].scene, "chat");
        assert!(entry.fresh(102));
        assert!(!entry.fresh(103));
        assert!(!entry.fresh(99));
        Ok(())
    }
    #[test]
    fn disabled_blocks_network_and_persists_opt_in() -> CloudResult<()> {
        let directory = std::env::temp_dir().join(format!(
            "simeji-cloud-test-{}-{}",
            std::process::id(),
            now()
        ));
        let mut cloud = CloudClient::open(&directory)?;
        assert!(!cloud.enabled());
        assert!(cloud.query("synthetic").is_err());
        assert!(cloud.client.is_none());
        cloud.set_enabled(true)?;
        drop(cloud);
        let mut cloud = CloudClient::open(&directory)?;
        assert!(cloud.enabled());
        cloud.cache.push_back(decode(
            br#"{"data":[{"hira":"x","cache_time":30,"candidates":[{"word":"cached"}]}]}"#,
            "x",
            0,
        )?);
        assert!(cloud.cached("x", false).is_empty());
        assert_eq!(cloud.cached("x", true)[0].text, "cached");
        cloud.set_enabled(false)?;
        assert!(cloud.cached("x", true).is_empty());
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
