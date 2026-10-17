//! Qobuz as a stelly-mix source. References:
//!
//! - `qobuz:<track id>`
//! - `qobuz:favourites`, the account's favourite tracks
//! - `qobuz:playlist/<id>`
//! - `qobuz:album/<id>`
//!
//! A track is fetched once, as FLAC at CD quality, or MP3 320 with
//! `QOBUZ_FORMAT=mp3`, into stelly-mix's cache for the source, and analysed
//! and mixed from there like any file.

pub mod client;

use anyhow::{bail, Context, Result};
use client::{Client, Credentials};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use stelly_mix::source::{Item, Source};

const FLAC_CD: u32 = 6;
const MP3_320: u32 = 5;
/// The most tracks a collection reference brings in.
const COLLECTION_CAP: usize = 2000;

pub struct Qobuz {
    client: Client,
    format: (u32, &'static str),
}

impl Qobuz {
    pub fn from_env() -> Result<Self> {
        let format = match std::env::var("QOBUZ_FORMAT").as_deref() {
            Ok("mp3") => (MP3_320, "mp3"),
            _ => (FLAC_CD, "flac"),
        };
        Ok(Self {
            client: Client::new(Credentials::from_env()?),
            format,
        })
    }
}

impl Source for Qobuz {
    fn name(&self) -> &str {
        "qobuz"
    }

    fn resolve(&self, reference: &str) -> Result<Vec<Item>> {
        let tracks = match reference.split_once('/') {
            None if reference == "favourites" || reference == "favorites" => {
                self.client.favourite_tracks(COLLECTION_CAP)?
            }
            Some(("playlist", id)) => self.client.playlist_tracks(id, COLLECTION_CAP)?,
            Some(("album", id)) => self.client.album_tracks(id)?,
            None if reference.chars().all(|c| c.is_ascii_digit()) => vec![self.client.track(reference)?],
            _ => bail!("qobuz:{reference}: expected a track id, favourites, playlist/<id> or album/<id>"),
        };
        Ok(tracks.iter().filter_map(item).collect())
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<Item>> {
        Ok(self.client.search_tracks(query, limit)?.iter().filter_map(item).collect())
    }

    fn fetch(&self, item: &Item, cache: &Path) -> Result<PathBuf> {
        let (format_id, extension) = self.format;
        let target = cache.join(format!("{}.{extension}", item.file_stem()));
        if target.is_file() {
            return Ok(target);
        }
        std::fs::create_dir_all(cache).with_context(|| format!("creating {}", cache.display()))?;
        let url = self.client.file_url(&item.id, format_id)?;
        let mut response = ureq::get(&url).call().with_context(|| format!("downloading {}", item.title))?;
        let mut body = response.body_mut().with_config().limit(1 << 31).reader();
        let partial = target.with_extension("part");
        let mut file = std::fs::File::create(&partial)?;
        std::io::copy(&mut body, &mut file)?;
        file.flush()?;
        std::fs::rename(&partial, &target)?;
        Ok(target)
    }
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
}
