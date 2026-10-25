//! Named phext fabrics beside the default chat file.
//!
//! The default fabric is the file Lanes was started with (`chat.phext` on
//! the server). Other fabrics are `<data-dir>/fabrics/<name>.phext`. Names
//! are a single path segment, created exclusively, and never overwritten.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use libphext::phext;

use crate::error::Error;
use crate::model::Thread;
use crate::seed;
use crate::store::{self, Store};

pub const MAX_FABRIC_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_FABRICS: usize = 100;
const CREATE_WINDOW: Duration = Duration::from_secs(600);
const CREATES_PER_WINDOW: usize = 6;

#[derive(Debug)]
pub enum FabricError {
    Bad(String),
    Conflict(String),
    Limited(String),
    Forbidden,
    Missing,
    Io(std::io::Error),
}

impl From<Error> for FabricError {
    fn from(err: Error) -> Self {
        match err {
            Error::Bad(message) => FabricError::Bad(message),
            Error::Io(err) => FabricError::Io(err),
        }
    }
}

impl From<std::io::Error> for FabricError {
    fn from(err: std::io::Error) -> Self {
        FabricError::Io(err)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Home,
    Lane(String),
    Post(String),
    Download { lane: Option<String> },
    Health,
    State,
    ApiLane(String),
    ApiPost(String),
    Room,
    Suggest,
    Toc,
    Scroll,
    PostMessage,
    Reset,
    Limits,
    Access,
    Create,
    Upload,
    Session,
    AuthStart(String),
    AuthCallback(String),
    Logout,
    Join(String),
    JoinBack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRoute {
    pub fabric: Option<String>,
    pub route: Route,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteError {
    Unknown,
    BadFabric,
}

pub fn parse_route(path: &str) -> Result<ParsedRoute, RouteError> {
    let path = path.split('?').next().unwrap_or(path);
    let path = path.trim_matches('/');
    let (fabric, rest) = if path == "f" {
        return Err(RouteError::BadFabric);
    } else if let Some(rest) = path.strip_prefix("f/") {
        let (name, rest) = match rest.split_once('/') {
            Some((name, rest)) => (name, rest),
            None => (rest, ""),
        };
        if name.is_empty() {
            return Err(RouteError::BadFabric);
        }
        let name = normalize_name(name).map_err(|_| RouteError::BadFabric)?;
        (Some(name), rest)
    } else {
        (None, path)
    };
    if fabric.is_some() && (rest.starts_with("auth/") || rest == "api/session") {
        return Err(RouteError::Unknown);
    }
    let route = match rest {
        "" => Route::Home,
        "chat.phext" => Route::Download { lane: None },
        "api/health" => Route::Health,
        "api/state" => Route::State,
        "api/room" => Route::Room,
        "api/suggest" => Route::Suggest,
        "api/toc" => Route::Toc,
        "api/scroll" => Route::Scroll,
        "api/post" => Route::PostMessage,
        "api/reset" => Route::Reset,
        "api/limits" => Route::Limits,
        "api/fabrics/access" => Route::Access,
        "api/fabrics" => Route::Create,
        "api/fabrics/upload" => Route::Upload,
        "api/session" => Route::Session,
        "auth/github" => Route::AuthStart("github".into()),
        "auth/google" => Route::AuthStart("google".into()),
        "auth/github/callback" => Route::AuthCallback("github".into()),
        "auth/google/callback" => Route::AuthCallback("google".into()),
        "auth/logout" => Route::Logout,
        "join/back" => Route::JoinBack,
        other => parse_tail(other)?,
    };
    Ok(ParsedRoute { fabric, route })
}

fn parse_tail(path: &str) -> Result<Route, RouteError> {
    if let Some(id) = path.strip_prefix("l/") {
        if let Some(id) = id.strip_suffix("/chat.phext") {
            return address(id).map(|id| Route::Download { lane: Some(id) });
        }
        return address(id).map(Route::Lane);
    }
    if let Some(id) = path.strip_prefix("p/") {
        return address(id).map(Route::Post);
    }
    if let Some(id) = path.strip_prefix("api/l/") {
        return address(id).map(Route::ApiLane);
    }
    if let Some(id) = path.strip_prefix("api/p/") {
        return address(id).map(Route::ApiPost);
    }
    if let Some(token) = path.strip_prefix("join/") {
        return one_segment(token).map(Route::Join);
    }
    Err(RouteError::Unknown)
}

/// A lane or post address: a coordinate such as `1.2.1/1.1.1/1.1.2`
/// (slashes, semicolons, or their percent escapes), or one legacy id segment.
fn address(id: &str) -> Result<String, RouteError> {
    let id = id
        .replace("%2F", "/")
        .replace("%2f", "/")
        .replace("%3B", ";")
        .replace("%3b", ";");
    let coordinate = !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '/' | ';'));
    if coordinate {
        Ok(id)
    } else {
        one_segment(&id)
    }
}

fn one_segment(id: &str) -> Result<String, RouteError> {
    if id.is_empty() || id.contains('/') {
        Err(RouteError::Unknown)
    } else {
        Ok(id.to_string())
    }
}

pub fn normalize_name(raw: &str) -> Result<String, FabricError> {
    let name = raw.trim().to_lowercase();
    if !(1..=32).contains(&name.len()) {
        return Err(FabricError::Bad(
            "fabric name must be 1–32 letters, numbers, or hyphens".into(),
        ));
    }
    let mut chars = name.chars();
    if !chars.next().is_some_and(|ch| ch.is_ascii_lowercase()) {
        return Err(FabricError::Bad(
            "fabric name must start with a letter".into(),
        ));
    }
    if !chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-') {
        return Err(FabricError::Bad(
            "fabric name must be letters, numbers, or hyphens".into(),
        ));
    }
    if name.ends_with('-') || name.contains("--") {
        return Err(FabricError::Bad(
            "fabric name must be letters, numbers, or hyphens".into(),
        ));
    }
    Ok(name)
}

pub fn fabrics_dir(default_file: &Path) -> PathBuf {
    match default_file.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join("fabrics"),
        _ => PathBuf::from("fabrics"),
    }
}

pub fn download_filename(fabric: Option<&str>, lane_title: Option<&str>, day: &str) -> String {
    let mut parts = vec!["phext-lanes".to_string()];
    if let Some(name) = fabric {
        parts.push(name.to_string());
    }
    if let Some(title) = lane_title {
        parts.push(slug(title));
    }
    parts.push(day.to_string());
    format!("{}.phext", parts.join("-"))
}

fn slug(title: &str) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 24 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "lane".to_string()
    } else {
        out
    }
}

pub fn parse_upload(bytes: &[u8]) -> Result<String, FabricError> {
    if bytes.is_empty() {
        return Err(FabricError::Bad("file is empty".into()));
    }
    if bytes.len() > MAX_FABRIC_BYTES {
        return Err(FabricError::Bad("file is larger than 5 MB".into()));
    }
    let doc = String::from_utf8(bytes.to_vec())
        .map_err(|_| FabricError::Bad("file is not utf-8".into()))?;
    let _ = phext::explode(&doc);
    let _ = store::world_from(&doc);
    Ok(doc)
}

pub fn roster_only() -> String {
    let mut doc = String::new();
    for (index, user) in seed::readers().into_iter().enumerate() {
        let mut coord = phext::default_coordinate();
        coord.z.library = crate::coord::ROSTER_LIBRARY;
        coord.x.scroll = index + 1;
        doc = phext::insert(doc, coord, &user.to_scroll());
    }
    doc
}

pub fn lane_document(doc: &str, thread: &Thread) -> String {
    let mut out = String::new();
    if !thread.plain {
        let anchor = crate::coord::thread_anchor(thread.coord);
        let card = phext::fetch(doc, anchor);
        if !card.trim().is_empty() {
            out = phext::insert(out, anchor, &card);
        }
    }
    for message in &thread.messages {
        let raw = phext::fetch(doc, message.coord);
        if !raw.is_empty() {
            out = phext::insert(out, message.coord, &raw);
        }
    }
    out
}

struct Hit {
    ip: String,
    at: Instant,
}

pub struct Hub {
    fabrics_dir: PathBuf,
    stores: Mutex<std::collections::BTreeMap<String, Arc<Mutex<Store>>>>,
    hits: Mutex<Vec<Hit>>,
}

impl Hub {
    pub fn open(default_path: impl Into<PathBuf>) -> Result<Self, Error> {
        let default_path = default_path.into();
        if let Some(parent) = default_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let fabrics_dir = fabrics_dir(&default_path);
        fs::create_dir_all(&fabrics_dir)?;
        let store = Store::open(&default_path)?;
        let mut stores = std::collections::BTreeMap::new();
        stores.insert(String::new(), Arc::new(Mutex::new(store)));
        Ok(Hub {
            fabrics_dir,
            stores: Mutex::new(stores),
            hits: Mutex::new(Vec::new()),
        })
    }

    pub fn list(&self) -> Result<Vec<String>, FabricError> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.fabrics_dir)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            let Some(stem) = file_name.strip_suffix(".phext") else {
                continue;
            };
            if normalize_name(stem).is_err() {
                continue;
            }
            let meta = fs::symlink_metadata(entry.path())?;
            if meta.file_type().is_symlink() {
                continue;
            }
            names.push(stem.to_string());
        }
        names.sort();
        names.insert(0, String::new());
        Ok(names)
    }

    pub fn with_store<T>(
        &self,
        fabric: Option<&str>,
        body: impl FnOnce(&mut Store) -> T,
    ) -> Result<T, FabricError> {
        let arc = self.arc_for(fabric)?;
        let mut guard = arc
            .lock()
            .map_err(|_| FabricError::Bad("store lock poisoned".into()))?;
        Ok(body(&mut guard))
    }

    pub fn create_empty(&self, requested: Option<&str>, ip: &str) -> Result<String, FabricError> {
        self.create(requested, &roster_only(), ip)
    }

    pub fn create_upload(
        &self,
        requested: Option<&str>,
        bytes: &[u8],
        ip: &str,
    ) -> Result<String, FabricError> {
        let doc = parse_upload(bytes)?;
        self.create(requested, &doc, ip)
    }

    fn create(&self, requested: Option<&str>, doc: &str, ip: &str) -> Result<String, FabricError> {
        let requested = requested
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(normalize_name)
            .transpose()?;
        self.allow(ip)?;
        self.ensure_room()?;
        match requested {
            Some(name) => {
                self.write_exact(&name, doc)?;
                Ok(name)
            }
            None => self.write_generated(doc),
        }
    }

    fn allow(&self, ip: &str) -> Result<(), FabricError> {
        let mut hits = self
            .hits
            .lock()
            .map_err(|_| FabricError::Bad("rate limit lock poisoned".into()))?;
        let now = Instant::now();
        hits.retain(|hit| now.saturating_duration_since(hit.at) < CREATE_WINDOW);
        let count = hits.iter().filter(|hit| hit.ip == ip).count();
        if count >= CREATES_PER_WINDOW {
            return Err(FabricError::Limited(
                "too many new fabrics from this address".into(),
            ));
        }
        hits.push(Hit {
            ip: ip.to_string(),
            at: now,
        });
        Ok(())
    }

    fn ensure_room(&self) -> Result<(), FabricError> {
        let mut count = 0usize;
        for entry in fs::read_dir(&self.fabrics_dir)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".phext"))
            {
                count += 1;
            }
        }
        if count >= MAX_FABRICS {
            return Err(FabricError::Limited(
                "this server has enough fabrics already".into(),
            ));
        }
        Ok(())
    }

    fn write_exact(&self, name: &str, doc: &str) -> Result<(), FabricError> {
        let path = self.safe_path(name)?;
        self.write_exclusive(&path, doc)?;
        self.remember(name, path, doc);
        Ok(())
    }

    fn write_generated(&self, doc: &str) -> Result<String, FabricError> {
        for index in 0..40 {
            let name = if index == 0 {
                "fabric".to_string()
            } else {
                format!("fabric-{}", index + 1)
            };
            let path = self.safe_path(&name)?;
            match self.write_exclusive(&path, doc) {
                Ok(()) => {
                    self.remember(&name, path, doc);
                    return Ok(name);
                }
                Err(FabricError::Conflict(_)) => continue,
                Err(err) => return Err(err),
            }
        }
        Err(FabricError::Limited(
            "could not choose a free fabric name".into(),
        ))
    }

    fn write_exclusive(&self, path: &Path, doc: &str) -> Result<(), FabricError> {
        let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(FabricError::Conflict(
                    "that fabric name is already in use".into(),
                ));
            }
            Err(err) => return Err(err.into()),
        };
        file.write_all(doc.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    fn remember(&self, name: &str, path: PathBuf, doc: &str) {
        if let Ok(mut stores) = self.stores.lock() {
            stores.insert(
                name.to_string(),
                Arc::new(Mutex::new(Store::from_doc(path, doc.to_string()))),
            );
        }
    }

    fn arc_for(&self, fabric: Option<&str>) -> Result<Arc<Mutex<Store>>, FabricError> {
        let key = fabric.unwrap_or("");
        if let Some(found) = self.cached(key)? {
            return Ok(found);
        }
        if key.is_empty() {
            return Err(FabricError::Missing);
        }
        let loaded = self.read_file(key)?;
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| FabricError::Bad("store map poisoned".into()))?;
        if let Some(found) = stores.get(key) {
            return Ok(found.clone());
        }
        let arc = Arc::new(Mutex::new(loaded));
        stores.insert(key.to_string(), arc.clone());
        Ok(arc)
    }

    fn cached(&self, key: &str) -> Result<Option<Arc<Mutex<Store>>>, FabricError> {
        let stores = self
            .stores
            .lock()
            .map_err(|_| FabricError::Bad("store map poisoned".into()))?;
        Ok(stores.get(key).cloned())
    }

    fn read_file(&self, name: &str) -> Result<Store, FabricError> {
        let path = self.safe_path(name)?;
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(FabricError::Missing);
            }
            Err(err) => return Err(err.into()),
        };
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            return Err(FabricError::Missing);
        }
        if meta.len() > MAX_FABRIC_BYTES as u64 {
            return Err(FabricError::Bad("fabric is larger than 5 MB".into()));
        }
        Store::load_doc(path).map_err(FabricError::from)
    }

    fn safe_path(&self, name: &str) -> Result<PathBuf, FabricError> {
        let name = normalize_name(name)?;
        let dir = fs::canonicalize(&self.fabrics_dir)?;
        let path = dir.join(format!("{name}.phext"));
        if path.parent() != Some(dir.as_path()) {
            return Err(FabricError::Bad("fabric path escaped its directory".into()));
        }
        Ok(path)
    }
}

pub fn day_stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%Y%m%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coord::parse_coord;
    use crate::link::{lane_id, post_id};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn hub() -> (Hub, Temp) {
        let dir = std::env::temp_dir().join(format!(
            "lanes-fabric-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let hub = Hub::open(dir.join("chat.phext")).unwrap();
        (hub, Temp(dir))
    }

    #[test]
    fn routes_keep_the_default_permalinks_and_add_a_fabric_prefix() {
        assert_eq!(
            parse_route("/l/1.2.1/1.1.1/1.1.1").unwrap(),
            ParsedRoute {
                fabric: None,
                route: Route::Lane("1.2.1/1.1.1/1.1.1".into()),
            }
        );
        assert_eq!(
            parse_route("/p/1.2.1/1.1.1/1.1.2").unwrap(),
            ParsedRoute {
                fabric: None,
                route: Route::Post("1.2.1/1.1.1/1.1.2".into()),
            }
        );
        assert_eq!(
            parse_route("/f/notes/l/1.2.1/1.1.1/1.1.1").unwrap(),
            ParsedRoute {
                fabric: Some("notes".into()),
                route: Route::Lane("1.2.1/1.1.1/1.1.1".into()),
            }
        );
        assert_eq!(
            parse_route("/chat.phext").unwrap().route,
            Route::Download { lane: None }
        );
        assert!(matches!(
            parse_route("/l/1.2.1/1.1.1/chat.phext").unwrap().route,
            Route::Download { lane: Some(_) }
        ));
        assert_eq!(parse_route("/f/../x"), Err(RouteError::BadFabric));
        assert_eq!(parse_route("/nope"), Err(RouteError::Unknown));
    }

    #[test]
    fn names_reject_traversal_and_creation_never_overwrites() {
        assert!(normalize_name("../etc/passwd").is_err());
        assert!(normalize_name("a/b").is_err());
        assert!(normalize_name("..").is_err());
        assert!(normalize_name(".phext").is_err());
        assert_eq!(normalize_name(" Notes ").unwrap(), "notes");

        let (hub, dir) = hub();
        let outside = dir.0.parent().unwrap().join("lanes-escaped.phext");
        let _ = fs::remove_file(&outside);
        assert!(hub.create_empty(Some("../x"), "203.0.113.9").is_err());
        assert!(!outside.exists());

        let first = hub.create_empty(Some("notes"), "203.0.113.9").unwrap();
        assert_eq!(first, "notes");
        let again = hub.create_empty(Some("notes"), "203.0.113.10");
        assert!(matches!(again, Err(FabricError::Conflict(_))));
        let doc = fs::read_to_string(dir.0.join("fabrics/notes.phext")).unwrap();
        assert!(doc.contains("kind: user"));
        assert!(!doc.contains("Fern wall"));
    }

    #[test]
    fn a_lane_download_is_only_that_lanes_scrolls() {
        let doc = seed::document(seed::demo_instant());
        let world = store::world_from(&doc);
        let fern = world.find_thread("Fern wall").unwrap();
        let lane = lane_document(&doc, fern);
        assert!(lane.contains("north-wall ferns"));
        assert!(!lane.contains("starter is slow"));
        assert_eq!(lane_id(fern.coord), "1.2.1/1.1.1/1.1.1");
        let opening = fern
            .messages
            .iter()
            .find(|message| message.text.contains("north-wall ferns"))
            .unwrap();
        assert_eq!(post_id(opening.coord), "1.2.1/1.1.1/1.1.2");
        let _ = parse_coord("1.2.1/1.1.1/1.1.2").unwrap();
    }

    #[test]
    fn uploads_must_be_utf8_phext_under_the_size_cap() {
        assert!(parse_upload(b"").is_err());
        assert!(parse_upload(&[0xff, 0xfe]).is_err());
        let doc = seed::document(seed::demo_instant());
        let parsed = parse_upload(doc.as_bytes()).unwrap();
        assert!(parsed.contains("Fern wall"));
        assert_eq!(
            download_filename(None, None, "20261023"),
            "phext-lanes-20261023.phext"
        );
        assert_eq!(
            download_filename(Some("notes"), Some("Fern wall"), "20261023"),
            "phext-lanes-notes-fern-wall-20261023.phext"
        );
    }

    #[test]
    fn the_same_address_is_limited() {
        let (hub, _dir) = hub();
        for index in 0..CREATES_PER_WINDOW {
            hub.create_empty(Some(&format!("n{index}")), "198.51.100.4")
                .unwrap();
        }
        let limited = hub.create_empty(Some("overflow"), "198.51.100.4");
        assert!(matches!(limited, Err(FabricError::Limited(_))));
        hub.create_empty(Some("other"), "198.51.100.5").unwrap();
    }
}
