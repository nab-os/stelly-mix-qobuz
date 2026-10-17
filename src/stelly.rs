//! Stelly's space as a stelly-mix source: tracks chosen by Stelly's server
//! from the tens of thousands it has analysed, the same CLAP model stelly-mix
//! embeds phrases with, and fetched from Qobuz like `qobuz:` ones.
//!
//! References, each optionally followed by `?` and filters joined by `&`:
//! `n=<count>` (default 100, every track for `all`), `bpm=<min>-<max>`, the
//! tempo Stelly measured folded by octaves into the range, and
//! `genre=<word>,<word>`, any of them in Qobuz's genre:
//!
//! - `stelly:all`, every track in the space
//! - `stelly:around-library`, the tracks nearest each of the library's own
//!   files that the space has: `n` of them each (default 20). Its own files,
//!   not what came from a source: what is near what you play, not near what
//!   was brought in.
//! - `stelly:<track id>`, one track
//! - `stelly:mood/<description>`, the tracks nearest a description
//! - `stelly:radio/<track id>`, a walk from a track, artists not repeated
//! - `stelly:neighbours/<track id>`, the tracks nearest one
//! - `stelly:drift/<track id>/<description>`, from a track towards a description
//! - `stelly:path/<track id>/<track id>`, from one track to another
//!
//! Track ids are Qobuz's, which is what Stelly's space is keyed by:
//! `search stelly <words>` finds them.

use crate::Qobuz;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use stelly_mix::source::{cache_dir, Item, Origin, Source};

const DEFAULT_COUNT: usize = 100;
/// A recipe's longest sequence; `all` has no limit but the space's.
const MAX_COUNT: usize = 2000;
/// Track ids asked about at once when listing the whole space.
const CHUNK: usize = 2000;
/// Neighbours of each library track that `around-library` brings, unless `n`.
const AROUND: usize = 20;

pub const HELP: &str = "\
Stelly's space wants its server and a device token: STELLY_SERVER (e.g.
http://127.0.0.1:7700) and STELLY_TOKEN, what `stelly-server pair --name mix`
printed, in the environment or ~/.config/stelly-mix/qobuz.env.";

pub struct StellySpace {
    server: Option<(String, String)>,
    agent: ureq::Agent,
    /// What fetches the tracks: Stelly's are Qobuz's.
    qobuz: Qobuz,
}

impl StellySpace {
    /// The server at `STELLY_SERVER`, with `STELLY_TOKEN`, from the
    /// environment over `~/.config/stelly-mix/qobuz.env`. Without them the
    /// source is still there, and says what it wants when used.
    pub fn from_env(qobuz: Qobuz) -> Self {
        let file = crate::config_file().map(|p| stelly_qobuz_client::config::parse_env(&p)).unwrap_or_default();
        let get = |key: &str| std::env::var(key).ok().or_else(|| file.get(key).cloned()).filter(|v| !v.is_empty());
        let server = get("STELLY_SERVER").zip(get("STELLY_TOKEN")).map(|(url, token)| (url.trim_end_matches('/').to_string(), token));
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // Stelly embeds a description with its text tower first.
            .timeout_global(Some(std::time::Duration::from_secs(120)))
            .build()
            .new_agent();
        Self { server, agent, qobuz }
    }

    fn server(&self) -> Result<&(String, String)> {
        self.server.as_ref().context(HELP)
    }

    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let (base, token) = self.server()?;
        let mut request = self.agent.get(&format!("{base}{path}")).header("Authorization", &format!("Bearer {token}"));
        for (key, value) in query {
            request = request.query(*key, value);
        }
        answer(path, request.call().with_context(|| format!("reaching Stelly's server at {base}"))?)
    }

    fn post(&self, path: &str, body: Value) -> Result<Value> {
        let (base, token) = self.server()?;
        let response = self
            .agent
            .post(&format!("{base}{path}"))
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .with_context(|| format!("reaching Stelly's server at {base}"))?;
        answer(path, response)
    }

    /// The tracks a recipe gives, in its order.
    fn generate(&self, recipe: Value, count: usize) -> Result<Vec<Value>> {
        let body = json!({ "recipe": recipe, "count": count, "penalty": 0.0, "weights": {} });
        Ok(list(self.post("/api/space/generate", body)?))
    }

    /// The tracks nearest each of the library's own files that the space
    /// has, `each` apiece: what is near what is played, rather than a genre.
    fn around_library(&self, each: usize) -> Result<Vec<Value>> {
        let library = stelly_mix::library::Library::open(stelly_mix::library::Library::default_dir())?.all()?;
        let own: Vec<&stelly_mix::library::Track> = library.iter().filter(|t| t.origin.is_none()).collect();
        let mut seeds: Vec<i64> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for track in &own {
            if let Some(seed) = self.find(track).filter(|s| seen.insert(*s)) {
                seeds.push(seed);
            }
        }
        eprintln!(
            "{} of the library's {} own files are in Stelly's space; {each} neighbours of each",
            seeds.len(),
            own.len()
        );
        let mut tracks = Vec::new();
        let mut kept = std::collections::HashSet::new();
        for seed in seeds {
            // One that the space has not built yet is passed over.
            let Ok(near) = self.generate(json!({ "mode": "neighbours", "seed": seed }), each) else { continue };
            tracks.extend(near.into_iter().filter(|t| t["track_id"].as_i64().is_some_and(|id| kept.insert(id))));
        }
        Ok(tracks)
    }

    /// A library track that is no source's, found in the space by its name.
    fn find(&self, track: &stelly_mix::library::Track) -> Option<i64> {
        let (artist, title) = guess(&track.name());
        let wanted = squash(&title);
        // A bare short title is anyone's: "Intro" finds the wrong one.
        if wanted.len() < 4 || (artist.is_none() && wanted.len() < 8) {
            return None;
        }
        let found = self.get("/api/space/search", &[("q", title.clone()), ("limit", "8".into())]).ok()?;
        found["tracks"].as_array()?.iter().find_map(|hit| {
            let same_title = squash(hit["title"].as_str()?) == wanted;
            let same_artist = artist.as_ref().is_none_or(|a| {
                let (a, b) = (squash(a), squash(hit["artist"].as_str().unwrap_or("")));
                !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a))
            });
            (same_title && same_artist).then(|| hit["track_id"].as_i64()).flatten()
        })
    }

    /// Every track in the space, a chunk of ids at a time.
    fn everything(&self) -> Result<Vec<Value>> {
        let index = self.get("/api/space/index", &[])?;
        let ids: Vec<i64> = index["ids"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect();
        let mut tracks = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            tracks.extend(list(self.post("/api/space/tracks", json!({ "ids": chunk }))?));
        }
        Ok(tracks)
    }
}

impl StellySpace {
    /// Qobuz's cache for what is asked into Stelly's, its own otherwise:
    /// one download for both sources.
    fn where_to(&self, cache: &Path) -> PathBuf {
        if cache == cache_dir(self.name()) {
            cache_dir(self.qobuz.name())
        } else {
            cache.to_path_buf()
        }
    }
}

/// A reply's JSON, or why there is none.
fn answer(path: &str, mut response: ureq::http::Response<ureq::Body>) -> Result<Value> {
    let status = response.status();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    if status.as_u16() == 401 {
        bail!("Stelly's server refused the token\n{HELP}");
    }
    if !status.is_success() {
        let message = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["message"].as_str().or_else(|| v["error"].as_str()).map(str::to_string))
            .unwrap_or_else(|| body[..body.floor_char_boundary(200)].to_string());
        bail!("{path}: {status}: {message}");
    }
    serde_json::from_str(&body).with_context(|| format!("parsing {path}"))
}

fn list(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        _ => Vec::new(),
    }
}

/// A file's name as an artist, if it says one, and a title: what a video's
/// title or a ripped album's file name adds set aside.
fn guess(name: &str) -> (Option<String>, String) {
    let mut text = String::new();
    let mut depth = 0;
    for c in name.chars() {
        match c {
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => depth = (depth - 1).max(0),
            _ if depth == 0 => text.push(if c == '_' { ' ' } else { c }),
            _ => {}
        }
    }
    let lower = text.to_lowercase();
    let cut = ["official video", "official audio", "official visualizer", "official music video", "lyrics", "｜", "|"]
        .iter()
        .filter_map(|noise| lower.find(noise))
        .min()
        .unwrap_or(text.len());
    let text = text[..text.floor_char_boundary(cut)].trim();
    // A leading track number, "01 - " or "03 ", is no artist.
    let text = text.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches([' ', '-', '.']).trim();
    match text.split_once(" - ") {
        Some((artist, title)) if !artist.trim().is_empty() => (Some(artist.trim().to_string()), title.trim().to_string()),
        _ => (None, text.to_string()),
    }
}

/// Lower case letters and digits only, for comparing names.
fn squash(text: &str) -> String {
    text.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// What a reference asks for.
#[derive(Debug, PartialEq)]
enum Ask {
    All,
    AroundLibrary,
    Track(i64),
    Recipe(Value),
}

/// Which of what comes back to keep, and how many.
#[derive(Debug, PartialEq)]
struct Filter {
    count: Option<usize>,
    bpm: Option<(f64, f64)>,
    genres: Vec<String>,
}

impl Filter {
    fn keeps(&self, track: &Value) -> bool {
        let tempo = || {
            let (Some((low, high)), Some(bpm)) = (self.bpm, track["bpm"].as_f64()) else { return false };
            // Stelly's tempo can be an octave off; any octave of it will do.
            [0.5, 1.0, 2.0].iter().any(|k| (low..=high).contains(&(bpm * k)))
        };
        let genre = || {
            let genre = track["genre"].as_str().unwrap_or("").to_lowercase();
            self.genres.iter().any(|g| genre.contains(g.as_str()))
        };
        (self.bpm.is_none() || tempo()) && (self.genres.is_empty() || genre())
    }
}

fn parse(reference: &str) -> Result<(Ask, Filter)> {
    let (what, query) = reference.split_once('?').unwrap_or((reference, ""));
    let mut filter = Filter { count: None, bpm: None, genres: Vec::new() };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').with_context(|| format!("?{pair}: expected key=value"))?;
        match key {
            "n" => filter.count = Some(value.parse::<usize>().with_context(|| format!("n={value} is not a count"))?.max(1)),
            "bpm" => {
                let (low, high) = value.split_once('-').context("bpm=<min>-<max>")?;
                let (low, high) = (low.trim().parse::<f64>()?, high.trim().parse::<f64>()?);
                anyhow::ensure!(0.0 < low && low < high, "bpm={value}: the least first, both above 0");
                filter.bpm = Some((low, high));
            }
            "genre" => filter.genres = value.split(',').map(|g| g.trim().to_lowercase()).filter(|g| !g.is_empty()).collect(),
            _ => bail!("?{key}: the filters are n, bpm and genre"),
        }
    }
    let id = |text: &str| text.trim().parse::<i64>().with_context(|| format!("{text:?} is not a track id"));
    let ask = match what.split_once('/') {
        None if what == "all" => Ask::All,
        None if what == "around-library" => Ask::AroundLibrary,
        None => Ask::Track(id(what)?),
        Some(("mood", phrase)) if !phrase.trim().is_empty() => Ask::Recipe(json!({ "mode": "mood", "phrase": phrase.trim() })),
        Some(("radio", seed)) => Ask::Recipe(json!({ "mode": "radio", "seed": id(seed)? })),
        Some(("neighbours" | "neighbors", seed)) => Ask::Recipe(json!({ "mode": "neighbours", "seed": id(seed)? })),
        Some(("drift", rest)) => {
            let (seed, phrase) = rest.split_once('/').context("drift/<track id>/<description>")?;
            Ask::Recipe(json!({ "mode": "drift", "seed": id(seed)?, "phrase": phrase.trim() }))
        }
        Some(("path", rest)) => {
            let (a, b) = rest.split_once('/').context("path/<track id>/<track id>")?;
            Ask::Recipe(json!({ "mode": "path", "a": id(a)?, "b": id(b)?, "even": false }))
        }
        _ => bail!(
            "stelly:{reference}: expected all, a track id, mood/<description>, radio/<id>, neighbours/<id>, \
             drift/<id>/<description> or path/<id>/<id>"
        ),
    };
    Ok((ask, filter))
}

/// One of Stelly's tracks as an item. Its id is Qobuz's, so it fetches as one.
fn item(track: &Value) -> Option<Item> {
    Some(Item {
        id: track["track_id"].as_i64()?.to_string(),
        artist: track["artist"].as_str().filter(|a| !a.is_empty()).unwrap_or("Unknown").to_string(),
        title: track["title"].as_str()?.to_string(),
        seconds: None,
    })
}

impl Source for StellySpace {
    fn name(&self) -> &str {
        "stelly"
    }

    fn resolve(&self, reference: &str) -> Result<Vec<Item>> {
        let (ask, filter) = parse(reference)?;
        let tracks = match ask {
            Ask::All => self.everything()?,
            // `n` is how many around each, not in all.
            Ask::AroundLibrary => {
                let each = filter.count.unwrap_or(AROUND);
                let around = self.around_library(each)?;
                let items: Vec<Item> = around.iter().filter(|t| filter.keeps(t)).filter_map(item).collect();
                if items.is_empty() {
                    bail!("stelly:{reference}: nothing in Stelly's space is near the library");
                }
                return Ok(items);
            }
            Ask::Track(id) => list(self.post("/api/space/tracks", json!({ "ids": [id] }))?),
            // Asked for more than kept when filtering, so the filter has room.
            Ask::Recipe(recipe) => {
                let wanted = filter.count.unwrap_or(DEFAULT_COUNT);
                let asked = if filter.bpm.is_some() || !filter.genres.is_empty() { wanted * 4 } else { wanted };
                self.generate(recipe, asked.min(MAX_COUNT))?
            }
        };
        let limit = filter.count.unwrap_or(usize::MAX);
        let items: Vec<Item> = tracks.iter().filter(|t| filter.keeps(t)).filter_map(item).take(limit).collect();
        if items.is_empty() {
            bail!("stelly:{reference}: Stelly's space has nothing for it");
        }
        Ok(items)
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<Item>> {
        let found = self.get("/api/space/search", &[("q", query.to_string()), ("limit", limit.to_string())])?;
        Ok(found["tracks"].as_array().into_iter().flatten().filter_map(item).take(limit).collect())
    }

    /// Where `qobuz:` keeps the same track: one download for both.
    fn cached(&self, item: &Item, _cache: &Path) -> Option<PathBuf> {
        self.qobuz.cached(item, &cache_dir(self.qobuz.name()))
    }

    fn fetch(&self, item: &Item, cache: &Path) -> Result<PathBuf> {
        self.qobuz.fetch(item, &self.where_to(cache))
    }

    fn fetch_for_analysis(&self, item: &Item, cache: &Path) -> Result<PathBuf> {
        self.qobuz.fetch_for_analysis(item, &self.where_to(cache))
    }

    /// Qobuz's track: the library knows it as `qobuz:<id>` whichever way it
    /// came, and Qobuz fetches it again.
    fn origin(&self, item: &Item) -> Origin {
        self.qobuz.origin(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_read_as_recipes() {
        assert_eq!(parse("12345").unwrap().0, Ask::Track(12345));
        let (ask, filter) = parse("mood/dark rolling techno?n=200").unwrap();
        assert_eq!(ask, Ask::Recipe(json!({ "mode": "mood", "phrase": "dark rolling techno" })));
        assert_eq!(filter.count, Some(200));
        assert_eq!(parse("radio/42").unwrap().0, Ask::Recipe(json!({ "mode": "radio", "seed": 42 })));
        assert_eq!(parse("neighbors/42?n=5").unwrap().0, Ask::Recipe(json!({ "mode": "neighbours", "seed": 42 })));
        assert_eq!(
            parse("drift/42/warmer, slower").unwrap().0,
            Ask::Recipe(json!({ "mode": "drift", "seed": 42, "phrase": "warmer, slower" }))
        );
        assert_eq!(parse("path/1/2").unwrap().0, Ask::Recipe(json!({ "mode": "path", "a": 1, "b": 2, "even": false })));
        let (ask, filter) = parse("all?bpm=125-150&genre=Techno, House").unwrap();
        assert_eq!(ask, Ask::All);
        assert_eq!(filter, Filter { count: None, bpm: Some((125.0, 150.0)), genres: vec!["techno".into(), "house".into()] });
        for bad in ["mood/", "radio/abc", "path/1", "drift/1", "favourites", "radio/1?n=many", "all?bpm=150-125", "all?year=2020", "all?bpm"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_give_up_their_artist_and_title() {
        assert_eq!(guess("HOEHENANGST - We Damage [Verknipt Records] ｜ Official Visualizer [gPzZMY6bTug]"), (Some("HOEHENANGST".into()), "We Damage".into()));
        assert_eq!(guess("Dennis Lloyd - Nevermind (Official Video) [naW6-WxmMiU]"), (Some("Dennis Lloyd".into()), "Nevermind".into()));
        assert_eq!(guess("01 - Better On Mute"), (None, "Better On Mute".into()));
        assert_eq!(guess("03 THE DRAIN"), (None, "THE DRAIN".into()));
        assert_eq!(guess("Floxytek - Hands Up [8994070]"), (Some("Floxytek".into()), "Hands Up".into()));
        assert_eq!(guess("time_travel"), (None, "time travel".into()));
        assert_eq!(parse("around-library?n=5").unwrap().0, Ask::AroundLibrary);
    }

    #[test]
    fn filters_fold_tempo_by_octaves_and_match_genres_in_part() {
        let (_, filter) = parse("all?bpm=125-150&genre=techno").unwrap();
        let track = |bpm: Option<f64>, genre: &str| json!({ "bpm": bpm, "genre": genre });
        assert!(filter.keeps(&track(Some(140.0), "Techno")));
        assert!(filter.keeps(&track(Some(70.0), "Hard Techno")), "an octave down");
        assert!(!filter.keeps(&track(Some(110.0), "Techno")));
        assert!(!filter.keeps(&track(None, "Techno")), "no tempo is no match for a tempo filter");
        assert!(!filter.keeps(&track(Some(140.0), "Pop/Rock")));
        let (_, none) = parse("all").unwrap();
        assert!(none.keeps(&track(None, "")));
    }

    #[test]
    fn stelly_tracks_become_qobuz_items() {
        let track = json!({ "track_id": 777, "title": "T", "artist": "A", "album": "X", "genre": "", "artist_id": 1,
                            "album_id": "a", "bpm": 128.0, "seed_distance": 0, "isrc": null, "x": null, "y": null });
        let item = item(&track).unwrap();
        assert_eq!((item.id.as_str(), item.artist.as_str(), item.title.as_str()), ("777", "A", "T"));
        assert!(super::item(&json!({ "title": "no id" })).is_none());
    }
}
