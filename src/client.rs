//! The Qobuz client, blocking, ported from Stelly's server: credentials from
//! a .env, request signing for stream URLs, one rate limit for the account,
//! retries on 429 and server faults. Only what a source needs: a track,
//! search, favourites, playlists, albums, and stream URLs.

use anyhow::{bail, Context, Result};
use md5::{Digest, Md5};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const API_BASE: &str = "https://www.qobuz.com/api.json/0.2";

/// Requests per second, and how many may be spent at once: one budget for
/// the account, as Stelly keeps it.
const RATE_PER_SEC: f64 = 2.0;
const BURST: f64 = 4.0;

pub const TOKEN_HELP: &str = "\
Qobuz credentials are needed: QOBUZ_APP_ID, QOBUZ_APP_SECRETS and
QOBUZ_USER_AUTH_TOKEN, in $STELLY_MIX_QOBUZ_ENV, ~/.config/stelly-mix/qobuz.env,
or the environment. Stelly's `stelly-server login` writes all three to its
.env; point STELLY_MIX_QOBUZ_ENV at it. The token alone comes from a logged-in
browser: https://play.qobuz.com/ -> devtools -> Local Storage ->
play.qobuz.com -> localuser.token.";

#[derive(Debug, Clone, Default)]
pub struct Credentials {
    pub app_id: String,
    pub secrets: Vec<String>,
    pub auth_token: String,
}

impl Credentials {
    /// The .env file, then the environment over it.
    pub fn from_env() -> Result<Self> {
        let mut merged = env_file().map(|p| parse_env(&p)).unwrap_or_default();
        for key in ["QOBUZ_APP_ID", "QOBUZ_APP_SECRET", "QOBUZ_APP_SECRETS", "QOBUZ_USER_AUTH_TOKEN"] {
            if let Ok(value) = std::env::var(key) {
                if !value.is_empty() {
                    merged.insert(key.to_string(), value);
                }
            }
        }
        let (Some(app_id), Some(auth_token)) = (merged.get("QOBUZ_APP_ID"), merged.get("QOBUZ_USER_AUTH_TOKEN")) else {
            bail!("{TOKEN_HELP}");
        };
        let mut secrets: Vec<String> = merged
            .get("QOBUZ_APP_SECRETS")
            .map(|s| s.split(',').map(|v| v.trim().to_string()).collect())
            .unwrap_or_default();
        secrets.extend(merged.get("QOBUZ_APP_SECRET").cloned());
        secrets.retain(|s| !s.is_empty());
        Ok(Self {
            app_id: app_id.clone(),
            secrets,
            auth_token: auth_token.clone(),
        })
    }
}

fn env_file() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("STELLY_MIX_QOBUZ_ENV") {
        return Some(path.into());
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("stelly-mix/qobuz.env"))
}

fn parse_env(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches(['\'', '"']).to_string()))
        .collect()
}

fn md5_hex(input: &str) -> String {
    Md5::digest(input.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// md5(endpoint without slashes + sorted key/value pairs + timestamp + secret).
fn sign(endpoint: &str, params: &BTreeMap<String, String>, timestamp: u64, secret: &str) -> String {
    let mut to_hash = endpoint.replace('/', "");
    for (key, value) in params {
        if key == "app_id" || key == "user_auth_token" {
            continue;
        }
        to_hash.push_str(key);
        to_hash.push_str(value);
    }
    to_hash.push_str(&timestamp.to_string());
    to_hash.push_str(secret);
    md5_hex(&to_hash)
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

pub struct Client {
    credentials: Credentials,
    bucket: Mutex<Bucket>,
    working_secret: Mutex<Option<String>>,
}

impl Client {
    pub fn new(credentials: Credentials) -> Self {
        Self {
            credentials,
            bucket: Mutex::new(Bucket {
                tokens: BURST,
                last: Instant::now(),
            }),
            working_secret: Mutex::new(None),
        }
    }

    /// Wait for a request slot.
    fn acquire(&self) {
        let wait = {
            let mut bucket = self.bucket.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            bucket.tokens = (bucket.tokens + now.duration_since(bucket.last).as_secs_f64() * RATE_PER_SEC).min(BURST);
            bucket.last = now;
            if bucket.tokens >= 1.0 {
                bucket.tokens -= 1.0;
                None
            } else {
                let deficit = (1.0 - bucket.tokens) / RATE_PER_SEC;
                bucket.tokens = 0.0;
                bucket.last = now + Duration::from_secs_f64(deficit);
                Some(Duration::from_secs_f64(deficit))
            }
        };
        if let Some(wait) = wait {
            std::thread::sleep(wait);
        }
    }

    fn request(&self, endpoint: &str, params: &BTreeMap<String, String>, secret: Option<&str>) -> Result<Value> {
        const ATTEMPTS: u32 = 4;
        let mut last_error = String::new();
        for attempt in 0..ATTEMPTS {
            let mut request = ureq::get(&format!("{API_BASE}/{endpoint}"))
                .header("X-App-Id", &self.credentials.app_id)
                .header("X-User-Auth-Token", &self.credentials.auth_token)
                .query("app_id", &self.credentials.app_id)
                .config()
                .http_status_as_error(false)
                .build();
            for (key, value) in params {
                request = request.query(key, value);
            }
            if let Some(secret) = secret {
                // Re-signed each attempt: the signature carries a timestamp.
                let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                request = request
                    .query("request_ts", timestamp.to_string())
                    .query("request_sig", sign(endpoint, params, timestamp, secret));
            }
            self.acquire();
            let mut response = match request.call() {
                Ok(response) => response,
                Err(err) => {
                    last_error = err.to_string();
                    std::thread::sleep(Duration::from_secs(1 << attempt));
                    continue;
                }
            };
            let status = response.status();
            let retry_after = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let body = response.body_mut().read_to_string().unwrap_or_default();
            if status.is_success() {
                return serde_json::from_str(&body).with_context(|| format!("parsing {endpoint}"));
            }
            last_error = format!("HTTP {status}: {}", &body[..body.len().min(200)]);
            let transient = status.as_u16() == 429 || status.is_server_error();
            if !transient {
                bail!("{endpoint} -> {last_error}");
            }
            std::thread::sleep(Duration::from_secs(retry_after.unwrap_or(1 << (attempt + 1))));
        }
        bail!("{endpoint} -> exhausted retries: {last_error}")
    }

    pub fn track(&self, track_id: &str) -> Result<Value> {
        self.request("track/get", &BTreeMap::from([("track_id".into(), track_id.into())]), None)
    }

    pub fn search_tracks(&self, query: &str, limit: usize) -> Result<Vec<Value>> {
        let params = BTreeMap::from([
            ("query".into(), query.into()),
            ("limit".into(), limit.to_string()),
            ("type".into(), "tracks".into()),
        ]);
        let data = self.request("catalog/search", &params, None)?;
        Ok(items(&data, "tracks"))
    }

    pub fn favourite_tracks(&self, cap: usize) -> Result<Vec<Value>> {
        self.paginate("favorite/getUserFavorites", BTreeMap::from([("type".into(), "tracks".into())]), "tracks", cap)
    }

    pub fn playlist_tracks(&self, playlist_id: &str, cap: usize) -> Result<Vec<Value>> {
        let params = BTreeMap::from([("playlist_id".into(), playlist_id.into()), ("extra".into(), "tracks".into())]);
        self.paginate("playlist/get", params, "tracks", cap)
    }

    /// An album's tracks, each given its album: the tracklist leaves it out.
    pub fn album_tracks(&self, album_id: &str) -> Result<Vec<Value>> {
        let data = self.request("album/get", &BTreeMap::from([("album_id".into(), album_id.into())]), None)?;
        let album = data.clone();
        Ok(items(&data, "tracks")
            .into_iter()
            .map(|mut t| {
                t["album"] = album.clone();
                t
            })
            .collect())
    }

    fn paginate(&self, endpoint: &str, params: BTreeMap<String, String>, container: &str, cap: usize) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        while out.len() < cap {
            let mut page = params.clone();
            page.insert("limit".into(), 100.min(cap - out.len()).to_string());
            page.insert("offset".into(), out.len().to_string());
            let data = self.request(endpoint, &page, None)?;
            let received = items(&data, container);
            let total = data[container]["total"].as_u64().unwrap_or(0) as usize;
            let done = received.is_empty();
            out.extend(received);
            if done || out.len() >= total {
                break;
            }
        }
        out.truncate(cap);
        Ok(out)
    }

    /// A signed stream URL, trying each secret until one is accepted.
    pub fn file_url(&self, track_id: &str, format_id: u32) -> Result<String> {
        let params = BTreeMap::from([
            ("track_id".into(), track_id.into()),
            ("format_id".into(), format_id.to_string()),
            ("intent".into(), "stream".into()),
        ]);
        let known = self.working_secret.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let candidates = known.clone().map_or_else(|| self.credentials.secrets.clone(), |s| vec![s]);
        if candidates.is_empty() {
            bail!("no QOBUZ_APP_SECRETS to sign with\n{TOKEN_HELP}");
        }
        let mut last_error = String::new();
        for secret in candidates {
            match self.request("track/getFileUrl", &params, Some(&secret)) {
                Ok(data) => {
                    if let Some(url) = data["url"].as_str() {
                        *self.working_secret.lock().unwrap_or_else(|e| e.into_inner()) = Some(secret);
                        return Ok(url.to_string());
                    }
                    last_error = format!("no url in response: {data}");
                }
                Err(err) => last_error = err.to_string(),
            }
        }
        // Delisted tracks fail as a signature error too: once a secret is
        // known to work, blame the track.
        if known.is_some() {
            bail!("track {track_id} is not streamable, likely delisted: {last_error}");
        }
        bail!("no app secret produced a valid signature; they rotate with the web player. Last error: {last_error}")
    }
}

fn items(data: &Value, container: &str) -> Vec<Value> {
    data[container]["items"].as_array().cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_skips_identity_keys_and_sorts_the_rest() {
        let params = BTreeMap::from([
            ("track_id".to_string(), "42".to_string()),
            ("format_id".to_string(), "6".to_string()),
            ("app_id".to_string(), "ignored".to_string()),
        ]);
        assert_eq!(
            sign("track/getFileUrl", &params, 1_700_000_000, "secret"),
            md5_hex("trackgetFileUrlformat_id6track_id421700000000secret")
        );
    }
}
