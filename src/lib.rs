//! Qobuz as a stelly-mix source. References:
//!
//! - `qobuz:<track id>`
//! - `qobuz:favourites`, the account's favourite tracks
//! - `qobuz:playlist/<id>`
//! - `qobuz:album/<id>`
//!
//! Qobuz itself is stelly-qobuz's gateway's: it holds the account and its
//! one request queue, shared with Stelly. A track is fetched once, as FLAC at
//! CD quality, or MP3 320 with `QOBUZ_FORMAT=mp3`, into stelly-mix's cache
//! for the source, and analysed and mixed from there like any file. A
//! collection brings in up to 2000 tracks, or `QOBUZ_LIMIT`.

pub mod stelly;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use stelly_mix::source::{Item, Source};
use stelly_qobuz_client::blocking::Client;
use stelly_qobuz_client::{api, Config, FileUrl, Page, Priority};

const FLAC_CD: u32 = 6;
const MP3_320: u32 = 5;
/// The most tracks a collection reference brings in, unless `QOBUZ_LIMIT`
/// says otherwise. A track at CD quality is ~30MB, kept: the library plays
/// from it.
const COLLECTION_CAP: usize = 2000;
/// What a fetched file can be, the format asked for first.
const EXTENSIONS: [&str; 2] = ["flac", "mp3"];

/// Every source this build adds to stelly-mix: Qobuz, and Stelly's space,
/// whose tracks are Qobuz's.
pub fn sources() -> Result<Vec<Box<dyn Source>>> {
    Ok(vec![
        Box::new(Qobuz::from_env()?),
        Box::new(stelly::StellySpace::from_env(Qobuz::from_env()?)),
    ])
}

pub struct Qobuz {
    gateway: Client,
    format_id: u32,
    cap: usize,
}

impl Qobuz {
    /// The gateway at `STELLY_QOBUZ_URL`, with `STELLY_QOBUZ_TOKEN`, from the
    /// environment over `~/.config/stelly-mix/qobuz.env`.
    pub fn from_env() -> Result<Self> {
        let format_id = match std::env::var("QOBUZ_FORMAT").as_deref() {
            Ok("mp3") => MP3_320,
            _ => FLAC_CD,
        };
        let cap = match std::env::var("QOBUZ_LIMIT") {
            Ok(limit) => limit.parse().with_context(|| format!("QOBUZ_LIMIT={limit:?} is not a count"))?,
            Err(_) => COLLECTION_CAP,
        };
        Ok(Self {
            gateway: Client::new(Config::from_env(config_file().as_deref())),
            format_id,
            cap,
        })
    }

    /// The gateway, for the bulk of an index: Stelly's browsing goes first.
    fn bulk(&self) -> Client {
        self.gateway.at(Priority::Bulk)
    }

    /// A collection's tracks, saying so when there were more than it took.
    fn collection(&self, what: &str, page: Page) -> Vec<Value> {
        if page.total > page.items.len() {
            eprintln!("qobuz:{what}: taking the first {} of {}; QOBUZ_LIMIT takes more", page.items.len(), page.total);
        }
        page.items
    }
}

fn config_file() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("stelly-mix/qobuz.env"))
}

/// The extension for what Qobuz sends, which may not be what was asked for.
fn extension(file: &FileUrl) -> &'static str {
    if file.format_id == MP3_320 || file.mime_type.as_deref() == Some("audio/mpeg") {
        "mp3"
    } else {
        "flac"
    }
}

impl Source for Qobuz {
    fn name(&self) -> &str {
        "qobuz"
    }

    fn resolve(&self, reference: &str) -> Result<Vec<Item>> {
        let tracks = match reference.split_once('/') {
            None if reference == "favourites" || reference == "favorites" => {
                let page = self.bulk().send(api::favourites("tracks", self.cap))?;
                self.collection(reference, page)
            }
            Some(("playlist", id)) => {
                let page = self.bulk().send(api::playlist_tracks(id, self.cap))?;
                self.collection(reference, page)
            }
            Some(("album", id)) => album_tracks(self.bulk().send(api::album(id))?),
            None if reference.chars().all(|c| c.is_ascii_digit()) => vec![self.gateway.send(api::track(reference))?],
            _ => bail!("qobuz:{reference}: expected a track id, favourites, playlist/<id> or album/<id>"),
        };
        Ok(tracks.iter().filter_map(item).collect())
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<Item>> {
        let found = self.gateway.send(api::search(query, limit, Some("tracks")))?;
        Ok(list(&found["tracks"]["items"]).iter().filter_map(item).collect())
    }

    fn cached(&self, item: &Item, cache: &Path) -> Option<PathBuf> {
        let stem = item.file_stem();
        // Whichever is there, FLAC or a track's MP3-only master, will do.
        EXTENSIONS
            .iter()
            .map(|e| cache.join(format!("{stem}.{e}")))
            .find(|p| p.is_file())
    }

    fn fetch(&self, item: &Item, cache: &Path) -> Result<PathBuf> {
        self.download(item, cache, self.format_id)
    }

    /// MP3 320: the analysis reads 48kHz mono, and loses nothing to it at a
    /// third of FLAC's size.
    fn fetch_for_analysis(&self, item: &Item, cache: &Path) -> Result<PathBuf> {
        self.download(item, cache, MP3_320)
    }
}

impl Qobuz {
    /// The item's audio in `format_id`, unless some is in `cache` already.
    fn download(&self, item: &Item, cache: &Path, format_id: u32) -> Result<PathBuf> {
        if let Some(target) = self.cached(item, cache) {
            return Ok(target);
        }
        std::fs::create_dir_all(cache).with_context(|| format!("creating {}", cache.display()))?;
        let file_url = self.bulk().send(api::file_url(&item.id, format_id))?;
        if file_url.sample {
            bail!("track {}: Qobuz offers only a preview of it to this account", item.id);
        }
        let target = cache.join(format!("{}.{}", item.file_stem(), extension(&file_url)));
        // From Qobuz's CDN, which the account's request budget does not cover.
        let mut response = ureq::get(&file_url.url)
            .call()
            .with_context(|| format!("downloading {}", item.title))?;
        let mut body = response.body_mut().with_config().limit(1 << 31).reader();
        let partial = target.with_extension("part");
        let fetched = (|| -> Result<()> {
            let mut file = std::fs::File::create(&partial)?;
            std::io::copy(&mut body, &mut file)?;
            file.flush()?;
            Ok(std::fs::rename(&partial, &target)?)
        })();
        if fetched.is_err() {
            let _ = std::fs::remove_file(&partial);
        }
        fetched.with_context(|| format!("downloading {}", item.title))?;
        Ok(target)
    }
}

/// An album's tracks, each given its album: the tracklist leaves it out.
fn album_tracks(mut album: Value) -> Vec<Value> {
    let tracks = list(&album["tracks"]["items"]);
    album["tracks"] = Value::Null;
    tracks
        .into_iter()
        .map(|mut track| {
            track["album"] = album.clone();
            track
        })
        .collect()
}

fn list(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}

/// A track's JSON as an item: its own performer, else its album's artist.
fn item(track: &Value) -> Option<Item> {
    let id = track["id"].as_i64()?.to_string();
    let mut title = track["title"].as_str()?.to_string();
    if let Some(version) = track["version"].as_str().filter(|v| !v.is_empty()) {
        title = format!("{title} ({version})");
    }
    let artist = track["performer"]["name"]
        .as_str()
        .or_else(|| track["album"]["artist"]["name"].as_str())
        .unwrap_or("Unknown")
        .to_string();
    Some(Item {
        id,
        artist,
        title,
        seconds: track["duration"].as_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_from_track_json() {
        let track = serde_json::json!({
            "id": 123, "title": "Song", "version": "Extended Mix", "duration": 301,
            "album": { "artist": { "name": "Album Artist" } }
        });
        let item = item(&track).unwrap();
        assert_eq!((item.id.as_str(), item.artist.as_str(), item.title.as_str()), ("123", "Album Artist", "Song (Extended Mix)"));
        assert_eq!(item.seconds, Some(301.0));
        let with_performer = serde_json::json!({ "id": 1, "title": "T", "performer": { "name": "P" } });
        assert_eq!(super::item(&with_performer).unwrap().artist, "P");
    }

    #[test]
    fn album_tracks_carry_their_album_but_not_its_tracklist() {
        let album = serde_json::json!({
            "id": "a1", "artist": { "name": "Band" },
            "tracks": { "total": 2, "items": [{ "id": 1, "title": "One" }, { "id": 2, "title": "Two" }] }
        });
        let tracks = album_tracks(album);
        assert_eq!(tracks.len(), 2);
        assert_eq!(item(&tracks[1]).unwrap().artist, "Band");
        assert!(tracks[0]["album"]["tracks"].is_null());
    }

    #[test]
    fn what_qobuz_sends_names_the_file() {
        let file = |format_id, mime: Option<&str>| FileUrl {
            url: String::new(),
            format_id,
            mime_type: mime.map(str::to_string),
            sample: false,
            duration: None,
        };
        assert_eq!(extension(&file(6, Some("audio/flac"))), "flac");
        assert_eq!(extension(&file(5, None)), "mp3");
        assert_eq!(extension(&file(6, Some("audio/mpeg"))), "mp3");
    }
}
