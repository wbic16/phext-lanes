//! The chat is one phext document.
//!
//! Writes go through [`phext::insert`]. Reads of a single scroll go through
//! [`phext::fetch`]. The table of contents is [`phext::textmap`], and the
//! byte index is [`phext::index`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use libphext::phext::{self, Coordinate};

use crate::coord::{
    self, format_coord, is_card_slot, is_roster, section_anchor, section_key, thread_anchor,
    thread_key, topic_key, Target,
};
use crate::error::Error;
use crate::model::{self, Message, Scroll, Thread, User};
use crate::seed;

#[derive(Debug, Clone)]
pub struct World {
    pub users: Vec<User>,
    pub threads: Vec<Thread>,
}

impl World {
    pub fn find_user(&self, query: &str) -> Option<&User> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lower = trimmed.to_lowercase();
        self.users
            .iter()
            .find(|user| user.id == trimmed || user.id == lower)
            .or_else(|| {
                self.users
                    .iter()
                    .find(|user| user.name.eq_ignore_ascii_case(trimmed))
            })
    }

    pub fn thread(&self, key: &str) -> Option<&Thread> {
        self.threads
            .iter()
            .find(|thread| !thread.plain && thread_key(thread.coord) == key)
            .or_else(|| {
                self.threads
                    .iter()
                    .find(|thread| thread_key(thread.coord) == key)
            })
    }

    pub fn find_lane(&self, id: &str) -> Option<&Thread> {
        let chat = |coord: Coordinate| {
            self.threads
                .iter()
                .find(|thread| !thread.plain && thread_key(thread.coord) == thread_key(coord))
        };
        let plain = |coord: Coordinate| {
            let key = section_key(coord);
            self.threads
                .iter()
                .find(|thread| thread.plain && section_key(thread.coord) == key)
        };
        // Nine parts: a chat thread's card at `…/1.1.1`, otherwise a section.
        if let Some(coord) = crate::link::parse_section_lane_id(id) {
            if coord.x.chapter == 1 && coord.x.section == 1 {
                if let Some(thread) = chat(coord) {
                    return Some(thread);
                }
            }
            return plain(coord);
        }
        let coord = crate::link::parse_lane_id(id)?;
        chat(coord).or_else(|| plain(coord))
    }

    /// The message scroll named by a post id. The thread card is not a post.
    pub fn find_post(&self, id: &str) -> Option<(&Thread, &Message)> {
        let coord = crate::link::parse_post_id(id)?;
        self.threads.iter().find_map(|thread| {
            thread
                .messages
                .iter()
                .find(|message| message.coord == coord)
                .map(|message| (thread, message))
        })
    }

    /// A thread title, or a thread or scroll coordinate.
    pub fn find_thread(&self, query: &str) -> Result<&Thread, String> {
        let query = query.trim();
        if query.is_empty() {
            return Err("name a thread".into());
        }
        if let Ok(target) = crate::coord::parse_target(query) {
            let coord = match target {
                Target::Thread(coord) | Target::Scroll(coord) => coord,
            };
            if matches!(target, Target::Scroll(_)) {
                if let Some(thread) = self
                    .threads
                    .iter()
                    .find(|thread| thread.messages.iter().any(|message| message.coord == coord))
                {
                    return Ok(thread);
                }
            }
            let key = thread_key(coord);
            return self
                .thread(&key)
                .ok_or_else(|| format!("no thread at {key}"));
        }
        let mut found: Option<&Thread> = None;
        for thread in &self.threads {
            if thread.title.eq_ignore_ascii_case(query) {
                if found.is_some() {
                    return Err(format!("more than one thread named {query}"));
                }
                found = Some(thread);
            }
        }
        found.ok_or_else(|| {
            let known = self
                .threads
                .iter()
                .map(|thread| thread.title.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("no thread {query}. known: {known}")
        })
    }
}

pub struct Post {
    pub author: String,
    pub who: String,
    pub text: String,
    pub target: Target,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Placed {
    pub coord: Coordinate,
    pub thread: Coordinate,
    pub title: String,
    pub topic: String,
    pub new_thread: bool,
}

pub struct Store {
    path: PathBuf,
    doc: String,
    /// Parsed once per save, then reused. A read does not scan the phext again.
    world: World,
    /// Coordinates that hold text, from the same pass as `world`.
    filled: BTreeSet<Coordinate>,
}

impl Store {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        if path.exists() {
            let doc = fs::read_to_string(&path)?;
            if !doc.trim().is_empty() {
                return Ok(Store::from_doc(path, doc));
            }
        }
        // A new chat starts with a General thread and the phext primer.
        // The demo cast is only written by `--seed` or the reset button.
        let mut store = Store::from_doc(path, String::new());
        store.persist(seed::blank(Utc::now()))?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn doc(&self) -> &str {
        &self.doc
    }

    /// Load a fabric that already exists. Do not seed over it.
    pub fn load_doc(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let bytes = fs::read(&path)?;
        let doc = String::from_utf8(bytes).map_err(|_| Error::Bad("fabric is not utf-8".into()))?;
        if doc.trim().is_empty() {
            return Err(Error::Bad("fabric is empty".into()));
        }
        Ok(Store::from_doc(path, doc))
    }

    pub fn from_doc(path: impl Into<PathBuf>, doc: String) -> Self {
        let (world, filled) = indexed(&doc);
        Store {
            path: path.into(),
            doc,
            world,
            filled,
        }
    }

    /// The parsed fabric. Rebuilt only when the file changes.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// `phext::fetch`
    pub fn select(&self, coord: Coordinate) -> String {
        phext::fetch(&self.doc, coord)
    }

    /// `phext::textmap`
    pub fn toc(&self) -> String {
        phext::textmap(&self.doc)
    }

    /// Offsets from `phext::index`, one coordinate per line.
    pub fn byte_index(&self) -> String {
        let indexed = phext::index(&self.doc);
        let map = phext::explode(&indexed);
        let mut coords: Vec<Coordinate> = map.keys().copied().collect();
        coords.sort();
        let mut lines = Vec::new();
        for coord in coords {
            let offset = map[&coord].trim();
            if offset.is_empty() {
                continue;
            }
            lines.push(format!("{}  byte {offset}", format_coord(coord)));
        }
        lines.join("\n")
    }

    pub fn reseed(&mut self, now: DateTime<Utc>) -> Result<(), Error> {
        self.persist(seed::document(now))
    }

    /// Create a chat with a General thread and no demo cast.
    ///
    /// A file that already has content is left untouched. `--seed` is the
    /// only path that replaces an existing chat, and it wipes that file.
    pub fn init_empty(path: impl AsRef<Path>) -> Result<(), Error> {
        let path = path.as_ref();
        if path.exists() && fs::metadata(path)?.len() > 0 {
            return Err(Error::Bad(format!(
                "refusing to replace {}. --empty never overwrites a chat that already has content. Move the file aside yourself if you mean to start over. --seed is the only flag that replaces a chat, and it wipes the file back to the demo cast.",
                path.display()
            )));
        }
        let mut store = Store::from_doc(path.to_path_buf(), String::new());
        store.persist(seed::blank(Utc::now()))
    }

    /// Add a reader, or refresh their display name. Interests start empty.
    ///
    /// Returns true when the roster scroll was created.
    pub fn ensure_member(&mut self, id: &str, name: &str) -> Result<bool, Error> {
        let id = model::member_id(id);
        if !crate::oauth::valid_who(&id) {
            return Err(Error::Bad("member id is not a sign-in".into()));
        }
        let mut name = coord::one_line(name);
        if name.chars().count() > 80 {
            name = name.chars().take(80).collect();
        }
        if name.is_empty() {
            name = "reader".to_string();
        }
        if let Some(existing) = self.world.users.iter().find(|user| user.id == id).cloned() {
            if existing.name == name {
                return Ok(false);
            }
            let Some(coord) = self.roster_coord(&id) else {
                return Ok(false);
            };
            let mut user = existing;
            user.name = name;
            let mut doc = self.doc.clone();
            doc = phext::insert(doc, coord, &user.to_scroll());
            self.persist(doc)?;
            return Ok(false);
        }
        let coord = self.next_roster()?;
        let user = User {
            id,
            name,
            blurb: String::new(),
            interests: Vec::new(),
        };
        let mut doc = self.doc.clone();
        doc = phext::insert(doc, coord, &user.to_scroll());
        self.persist(doc)?;
        Ok(true)
    }

    /// Remember a thread this member posted in or opened.
    ///
    /// The feed still uses the existing distance ranking. No interests means
    /// every thread is equally near, so a new member sees recency until this
    /// list grows. The first thread stays their home shelf.
    pub fn note_interest(&mut self, id: &str, thread: Coordinate) -> Result<(), Error> {
        let anchor = thread_anchor(thread);
        let Some(existing) = self.world.users.iter().find(|user| user.id == id).cloned() else {
            return Ok(());
        };
        if existing
            .interests
            .iter()
            .any(|coord| thread_key(*coord) == thread_key(anchor))
        {
            return Ok(());
        }
        let Some(at) = self.roster_coord(id) else {
            return Ok(());
        };
        let mut user = existing;
        user.interests.push(anchor);
        if user.interests.len() > 8 {
            user.interests.remove(1);
        }
        let mut doc = self.doc.clone();
        doc = phext::insert(doc, at, &user.to_scroll());
        self.persist(doc)?;
        Ok(())
    }

    pub fn place(&mut self, post: Post) -> Result<Placed, Error> {
        let author = coord::one_line(&post.author);
        if author.is_empty() || author.chars().count() > 80 {
            return Err(Error::Bad("author name is missing or too long".into()));
        }
        let text = coord::sanitize(&post.text);
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(Error::Bad("message is empty".into()));
        }
        if text.chars().count() > 4000 {
            return Err(Error::Bad("message is longer than 4000 characters".into()));
        }

        let requested = match post.target {
            Target::Thread(coord) => thread_anchor(coord),
            Target::Scroll(coord) => coord,
        };
        if is_roster(requested) {
            return Err(Error::Bad(
                "library 5 is the reader roster, not a room".into(),
            ));
        }
        if !requested.validate_coordinate() {
            return Err(Error::Bad("coordinate is out of range".into()));
        }
        let anchor = thread_anchor(requested);
        let existing = self
            .world
            .threads
            .iter()
            .find(|thread| !thread.plain && thread_key(thread.coord) == thread_key(anchor))
            .cloned();
        let new_thread = existing.is_none();
        let shelf_topic = self
            .world
            .threads
            .iter()
            .find(|thread| topic_key(thread.coord) == topic_key(anchor))
            .map(|thread| thread.topic.clone());
        let anchor_taken = self.filled.contains(&anchor);
        let coord = match post.target {
            Target::Scroll(wanted) if !is_card_slot(wanted) && !self.filled.contains(&wanted) => {
                wanted
            }
            _ => next_free(&self.filled, anchor),
        };
        if !coord.validate_coordinate() {
            return Err(Error::Bad("that thread has no free scroll left".into()));
        }

        let mut doc = self.doc.clone();
        let (title, topic) = if let Some(thread) = existing {
            (thread.title, thread.topic)
        } else {
            let topic = shelf_topic.unwrap_or_else(|| format!("Shelf {}", topic_key(anchor)));
            let title = model::title_from_text(&text);
            if !anchor_taken {
                let card = model::format_card(&topic, &title, &model::keyword_line(&text));
                doc = phext::insert(doc, anchor, &card);
            }
            (title, topic)
        };
        let body = model::format_message_as(&author, &post.who, post.now, &text);
        doc = phext::insert(doc, coord, &body);
        self.persist(doc)?;

        Ok(Placed {
            coord,
            thread: anchor,
            title,
            topic,
            new_thread,
        })
    }

    fn roster_coord(&self, id: &str) -> Option<Coordinate> {
        self.filled.iter().copied().find(|coord| {
            if !is_roster(*coord) {
                return false;
            }
            match model::parse_scroll(&phext::fetch(&self.doc, *coord)) {
                Some(Scroll::User(user)) => user.id == id,
                _ => false,
            }
        })
    }

    fn next_roster(&self) -> Result<Coordinate, Error> {
        let mut coord = phext::default_coordinate();
        coord.z.library = coord::ROSTER_LIBRARY;
        for scroll in 1..=phext::COORDINATE_MAXIMUM {
            coord.x.scroll = scroll;
            if !self.filled.contains(&coord) {
                return Ok(coord);
            }
        }
        Err(Error::Bad("the roster is full".into()))
    }

    fn persist(&mut self, doc: String) -> Result<(), Error> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("chat.phext");
        let tmp = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .join(format!(".{name}.tmp"));
        fs::write(&tmp, &doc)?;
        fs::rename(&tmp, &self.path)?;
        self.doc = doc;
        let (world, filled) = indexed(&self.doc);
        self.world = world;
        self.filled = filled;
        Ok(())
    }
}

/// One scan of the phext: the reader world, and every coordinate that has text.
fn indexed(doc: &str) -> (World, BTreeSet<Coordinate>) {
    if doc.is_empty() {
        return (
            World {
                users: Vec::new(),
                threads: Vec::new(),
            },
            BTreeSet::new(),
        );
    }
    let map = phext::explode(doc);
    let filled = map
        .iter()
        .filter(|(_, raw)| !raw.trim().is_empty())
        .map(|(coord, _)| *coord)
        .collect();
    (world_from_map(&map), filled)
}

pub fn world_from(doc: &str) -> World {
    indexed(doc).0
}

fn world_from_map(map: &std::collections::HashMap<Coordinate, String>) -> World {
    if map.is_empty() {
        return World {
            users: Vec::new(),
            threads: Vec::new(),
        };
    }
    let mut coords: Vec<Coordinate> = map.keys().copied().collect();
    coords.sort();

    let mut users = Vec::new();
    let mut cards: BTreeMap<String, (String, String, String)> = BTreeMap::new();
    let mut messages: BTreeMap<String, Vec<Message>> = BTreeMap::new();
    let mut anchors: BTreeMap<String, Coordinate> = BTreeMap::new();
    let mut plain: BTreeMap<String, Vec<Message>> = BTreeMap::new();
    let mut plain_anchors: BTreeMap<String, Coordinate> = BTreeMap::new();

    for coord in coords {
        let raw = &map[&coord];
        if raw.trim().is_empty() {
            continue;
        }
        if is_roster(coord) {
            if let Some(Scroll::User(user)) = model::parse_scroll(raw) {
                users.push(user);
            }
            continue;
        }
        if let Some(scroll) = model::parse_scroll(raw) {
            match scroll {
                Scroll::Card {
                    topic,
                    title,
                    keywords,
                } if is_card_slot(coord) => {
                    let key = thread_key(coord);
                    anchors
                        .entry(key.clone())
                        .or_insert_with(|| thread_anchor(coord));
                    cards.insert(key, (topic, title, keywords));
                    continue;
                }
                Scroll::Message {
                    author,
                    who,
                    ts,
                    text,
                } => {
                    let key = thread_key(coord);
                    anchors
                        .entry(key.clone())
                        .or_insert_with(|| thread_anchor(coord));
                    messages.entry(key).or_default().push(Message {
                        coord,
                        author,
                        who,
                        ts,
                        text,
                        plain: false,
                    });
                    continue;
                }
                Scroll::Card { .. } | Scroll::User(_) => {}
            }
        }
        let text = raw.replace("\r\n", "\n").trim().to_string();
        if text.is_empty() {
            continue;
        }
        let key = section_key(coord);
        plain_anchors
            .entry(key.clone())
            .or_insert_with(|| section_anchor(coord));
        plain.entry(key).or_default().push(Message {
            coord,
            author: model::PLAIN_AUTHOR.into(),
            who: String::new(),
            ts: DateTime::<Utc>::UNIX_EPOCH,
            text,
            plain: true,
        });
    }

    let mut threads = Vec::new();
    for (key, anchor) in anchors {
        let mut msgs = messages.remove(&key).unwrap_or_default();
        msgs.sort_by(|a, b| a.coord.cmp(&b.coord).then(a.ts.cmp(&b.ts)));
        let (topic, title, keywords) = cards
            .remove(&key)
            .unwrap_or_else(|| (topic_key(anchor), key.clone(), String::new()));
        threads.push(Thread {
            coord: anchor,
            topic,
            title,
            keywords,
            messages: msgs,
            plain: false,
        });
    }
    for (key, anchor) in plain_anchors {
        let mut msgs = plain.remove(&key).unwrap_or_default();
        msgs.sort_by(|a, b| a.coord.cmp(&b.coord));
        let title = msgs
            .first()
            .map(|message| model::opening_line(&message.text, 80))
            .unwrap_or_else(|| "Scroll".into());
        threads.push(Thread {
            coord: anchor,
            topic: topic_key(anchor),
            title,
            keywords: String::new(),
            messages: msgs,
            plain: true,
        });
    }
    threads.sort_by(|a, b| a.coord.cmp(&b.coord).then(a.plain.cmp(&b.plain)));
    World { users, threads }
}

fn next_free(filled: &BTreeSet<Coordinate>, anchor: Coordinate) -> Coordinate {
    let used = filled
        .iter()
        .filter(|coord| {
            thread_key(**coord) == thread_key(anchor)
                && coord.x.chapter == 1
                && coord.x.section == 1
        })
        .map(|coord| coord.x.scroll);
    coord::with_scroll(anchor, coord::next_message_scroll(used))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coord::parse_coord;
    use crate::rank::{self, admitted_by};
    use crate::seed::{self, BREAD, CASEY, FERN, HALL, PLANES, RIVER};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempFile(PathBuf);
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn temp_path() -> PathBuf {
        let id = TEMP.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("lanes-{}-{id}.phext", std::process::id()))
    }

    /// A temp file holding the demo cast.
    fn demo_path() -> PathBuf {
        let path = temp_path();
        fs::write(&path, seed::document(Utc::now())).unwrap();
        path
    }

    #[test]
    fn phext_insert_fetch_and_toc_roundtrip() {
        let alpha = parse_coord("1.1.1/1.1.1/1.1.1").unwrap();
        let beta = parse_coord("1.1.1/1.1.1/1.1.2").unwrap();
        let gamma = parse_coord("1.2.1/1.1.1/1.1.1").unwrap();
        let mut doc = String::new();
        doc = phext::insert(doc, alpha, "alpha");
        doc = phext::insert(doc, beta, "beta");
        doc = phext::insert(doc, gamma, "gamma");
        assert_eq!(
            phext::fetch(&doc, alpha),
            "alpha",
            "bytes {:02x?}",
            doc.as_bytes()
        );
        assert_eq!(
            phext::fetch(&doc, beta),
            "beta",
            "bytes {:02x?}",
            doc.as_bytes()
        );
        assert_eq!(
            phext::fetch(&doc, gamma),
            "gamma",
            "bytes {:02x?}",
            doc.as_bytes()
        );
        let toc = phext::textmap(&doc);
        assert!(toc.contains("1.1.1/1.1.1/1.1.1"), "{toc}");
        assert!(toc.contains("1.1.1/1.1.1/1.1.2"), "{toc}");
        assert!(toc.contains("1.2.1/1.1.1/1.1.1"), "{toc}");
        assert!(
            doc.as_bytes().contains(&0x17),
            "scroll break between messages"
        );
        assert!(doc.as_bytes().contains(&0x1F), "shelf break between topics");
        assert!(!phext::index(&doc).is_empty());
    }

    #[test]
    fn insert_between_existing_coordinates_keeps_each_scroll() {
        let low = parse_coord("1.1.1/1.1.1/1.1.1").unwrap();
        let high = parse_coord("1.2.1/1.1.1/1.1.1").unwrap();
        let mid = parse_coord("1.1.1/1.1.2/1.1.1").unwrap();
        let mut doc = String::new();
        doc = phext::insert(doc, low, "L");
        doc = phext::insert(doc, high, "H");
        doc = phext::insert(doc, mid, "M");
        assert_eq!(
            phext::fetch(&doc, low),
            "L",
            "bytes {:02x?}",
            doc.as_bytes()
        );
        assert_eq!(
            phext::fetch(&doc, mid),
            "M",
            "bytes {:02x?}",
            doc.as_bytes()
        );
        assert_eq!(
            phext::fetch(&doc, high),
            "H",
            "bytes {:02x?}",
            doc.as_bytes()
        );
    }

    #[test]
    fn message_text_cannot_inject_a_scroll_break() {
        let path = demo_path();
        let _file = TempFile(path.clone());
        let mut store = Store::open(&path).unwrap();
        let placed = store
            .place(Post {
                author: seed::MIRA.into(),
                who: String::new(),
                text: "hello\u{0017}hidden".into(),
                target: Target::Thread(parse_coord("1.2.1/1.1.1/1.1.1").unwrap()),
                now: Utc::now(),
            })
            .unwrap();
        let raw = store.select(placed.coord);
        assert!(raw.contains("hellohidden"), "{raw}");
        assert!(!raw.contains('\u{0017}'));
    }

    #[test]
    fn place_persists_and_survives_reopen() {
        let path = demo_path();
        let _file = TempFile(path.clone());
        let mut store = Store::open(&path).unwrap();
        let placed = store
            .place(Post {
                author: "mira".into(),
                who: String::new(),
                text: "A new note about moss on the stone.".into(),
                target: Target::Thread(parse_coord("1.2.1/1.1.1/1.1.1").unwrap()),
                now: Utc::now(),
            })
            .unwrap();
        assert!(store.select(placed.coord).contains("moss on the stone"));
        assert!(store.toc().contains(&format_coord(placed.coord)));
        drop(store);
        let store = Store::open(&path).unwrap();
        assert!(store.select(placed.coord).contains("moss on the stone"));
        assert!(store.byte_index().contains("byte"));
        let world = store.world();
        let again = world
            .find_post(&crate::link::post_id(placed.coord))
            .unwrap();
        assert_eq!(again.1.text, "A new note about moss on the stone.");
        assert_eq!(again.0.title, FERN);
    }

    #[test]
    fn seeded_file_uses_real_dimension_breaks() {
        let now = seed::demo_instant();
        let doc = seed::document(now);
        let bytes = doc.as_bytes();
        assert!(
            bytes.contains(&(phext::SCROLL_BREAK as u8)),
            "0x17 between messages"
        );
        assert!(
            bytes.contains(&(phext::VOLUME_BREAK as u8)),
            "0x1C between volumes"
        );
        assert!(
            bytes.contains(&(phext::SHELF_BREAK as u8)),
            "0x1F between shelves"
        );
        assert!(
            bytes.contains(&(phext::LIBRARY_BREAK as u8)),
            "0x01 between libraries"
        );
        let world = world_from(&doc);
        let fern = world.thread(seed::FERN_THREAD).unwrap();
        assert!(fern.messages.len() >= 2);
        let first = &fern.messages[0];
        let second = &fern.messages[1];
        let raw_first = phext::fetch(&doc, first.coord);
        let raw_second = phext::fetch(&doc, second.coord);
        assert!(raw_first.contains(&first.text), "{raw_first}");
        assert!(!raw_first.contains(&second.text));
        assert!(raw_second.contains(&second.text));
    }

    #[test]
    fn mira_feed_keeps_casey_in_a_lane_while_the_hall_is_flooded() {
        let now = seed::demo_instant();
        let world = world_from(&seed::document(now));
        let mira = world.find_user("mira").unwrap();
        assert_eq!(coord::thread_key(mira.interests[0]), seed::FERN_THREAD);
        let feed = rank::build_feed(
            &world.threads,
            &mira.interests,
            now,
            rank::AUTHOR_CAP,
            rank::MAX_THREADS,
        );
        assert_eq!(feed.threads[0].title, FERN);
        assert_eq!(feed.threads[0].distance, 0);
        let titles: Vec<&str> = feed
            .threads
            .iter()
            .map(|thread| thread.title.as_str())
            .collect();
        let pos = |name: &str| titles.iter().position(|title| *title == name).unwrap();
        assert!(pos(FERN) < pos(seed::SWAP));
        assert!(pos(seed::SWAP) < pos(HALL));
        assert!(admitted_by(&feed, CASEY) <= rank::AUTHOR_CAP);
        let lane = feed.lanes.iter().find(|lane| lane.author == CASEY).unwrap();
        assert!(lane.held >= 15, "held {}", lane.held);
        assert!(lane.held > admitted_by(&feed, CASEY));

        let hall = world.thread(seed::HALL_THREAD).unwrap();
        let casey = hall
            .messages
            .iter()
            .filter(|message| message.author == CASEY)
            .count();
        assert!(casey >= 10);
        assert!(casey * 100 / hall.messages.len() >= 70);
        let (_, _, share) = rank::majority(&hall.messages);
        assert!(rank::is_flooded(hall.messages.len(), share));
        assert_eq!(
            feed.threads
                .iter()
                .find(|t| t.title == HALL)
                .unwrap()
                .nearness,
            "same wing"
        );
    }

    #[test]
    fn each_reader_opens_on_their_interest_and_casey_still_hears_quiet_voices() {
        let now = seed::demo_instant();
        let world = world_from(&seed::document(now));
        let first = |id: &str| {
            let user = world.find_user(id).unwrap();
            let feed = rank::build_feed(
                &world.threads,
                &user.interests,
                now,
                rank::AUTHOR_CAP,
                rank::MAX_THREADS,
            );
            feed.threads[0].title.clone()
        };
        assert_eq!(first("jules"), PLANES);
        assert_eq!(first("ada"), RIVER);
        assert_eq!(first("ren"), BREAD);
        assert_eq!(first("casey"), HALL);

        let casey = world.find_user("casey").unwrap();
        let feed = rank::build_feed(
            &world.threads,
            &casey.interests,
            now,
            rank::AUTHOR_CAP,
            rank::MAX_THREADS,
        );
        assert!(admitted_by(&feed, CASEY) <= rank::AUTHOR_CAP);
        let hall = feed
            .threads
            .iter()
            .find(|thread| thread.title == HALL)
            .unwrap();
        let voices: std::collections::BTreeSet<&str> = hall
            .admitted
            .iter()
            .map(|message| message.author.as_str())
            .collect();
        assert!(voices.len() > 1, "{voices:?}");
        assert!(
            voices.contains(seed::MIRA)
                || voices.contains(seed::JULES)
                || voices.contains(seed::REN)
        );
    }

    #[test]
    fn threads_open_by_title_or_coordinate() {
        let world = world_from(&seed::document(seed::demo_instant()));
        assert_eq!(world.find_thread("fern wall").unwrap().title, FERN);
        assert_eq!(world.find_thread(seed::FERN_THREAD).unwrap().title, FERN);
        assert_eq!(world.find_thread("1.2.1/1.1.1/1.1.4").unwrap().title, FERN);
        let missing = world.find_thread("not a room").unwrap_err();
        assert!(missing.contains("Fern wall"), "{missing}");
        assert!(world.find_thread("9.9.9/9.9.9").is_err());
    }

    #[test]
    fn suggest_on_the_seed_picks_the_fern_wall() {
        let now = seed::demo_instant();
        let world = world_from(&seed::document(now));
        let mira = world.find_user("Mira Quill").unwrap();
        let suggestion = rank::suggest(
            &world.threads,
            &mira.interests,
            "The ferns on the north wall need more shade",
            now,
        );
        assert_eq!(suggestion.title, FERN);
        assert!(!suggestion.new_thread);
        assert!(suggestion.overlap.iter().any(|word| word == "fern"));

        let fresh = rank::suggest(
            &world.threads,
            &mira.interests,
            "zzzz quantum badminton",
            now,
        );
        assert!(fresh.new_thread);
        assert_eq!(thread_key(fresh.thread), "1.2.1/1.1.2");
    }

    #[test]
    fn ordinary_scrolls_become_section_lanes_beside_chat() {
        let mut doc = String::new();
        let put = |doc: &mut String, coord: &str, text: &str| {
            *doc = phext::insert(std::mem::take(doc), parse_coord(coord).unwrap(), text);
        };
        put(
            &mut doc,
            "1.1.1/1.1.1/1.1.1",
            "Incipit (It Begins)\nThe opening stays.",
        );
        put(
            &mut doc,
            "1.1.1/1.1.1/1.1.2",
            "TTSM\nA second scroll in the same section.",
        );
        put(&mut doc, "1.1.1/1.1.1/2.1.1", "TAOP\nAnother chapter.");
        put(&mut doc, "1.1.1/1.2.1/1.1.1", "MOAT\nAnother book.");
        put(
            &mut doc,
            "1.2.1/1.1.1/1.1.1",
            &model::format_card("Gardens", "Fern wall", "fern"),
        );
        let ts = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 10, 22, 18, 0, 0).unwrap();
        put(
            &mut doc,
            "1.2.1/1.1.1/1.1.2",
            &model::format_message("Mira Quill", ts, "The north-wall ferns finally unfurled."),
        );

        let world = world_from(&doc);
        let incipit = world
            .threads
            .iter()
            .find(|thread| thread.title == "Incipit (It Begins)")
            .unwrap();
        assert!(incipit.plain);
        assert_eq!(incipit.messages.len(), 2);
        assert_eq!(incipit.messages[0].author, model::PLAIN_AUTHOR);
        assert!(incipit.messages[1].text.starts_with("TTSM"));
        assert_eq!(
            crate::link::section_lane_id(incipit.coord),
            "1.1.1/1.1.1/1.1.1"
        );
        assert_eq!(section_key(incipit.coord), "1.1.1/1.1.1/1.1");

        let chapter = world
            .threads
            .iter()
            .find(|thread| thread.title == "TAOP")
            .unwrap();
        assert!(chapter.plain);
        assert_eq!(chapter.coord.x.chapter, 2);
        assert_ne!(
            crate::link::section_lane_id(chapter.coord),
            crate::link::section_lane_id(incipit.coord)
        );

        let moat = world
            .threads
            .iter()
            .find(|thread| thread.title == "MOAT")
            .unwrap();
        assert_eq!(thread_key(moat.coord), "1.1.1/1.2.1");

        let fern = world
            .threads
            .iter()
            .find(|thread| thread.title == "Fern wall")
            .unwrap();
        assert!(!fern.plain);
        assert_eq!(fern.messages[0].author, "Mira Quill");
        assert_eq!(crate::link::lane_id(fern.coord), "1.2.1/1.1.1/1.1.1");
        assert_eq!(world.find_lane("1.2.1/1.1.1").unwrap().title, "Fern wall");
        assert_eq!(
            world.find_lane("1.2.1/1.1.1/1.1.1").unwrap().title,
            "Fern wall"
        );
        assert_eq!(world.find_lane("kcre66i9s").unwrap().title, "Fern wall");
        assert_eq!(
            world.find_lane("1.1.1/1.1.1/1.1.1").unwrap().title,
            "Incipit (It Begins)"
        );
        assert_eq!(world.find_lane("s0").unwrap().title, "Incipit (It Begins)");
        assert!(world
            .find_post("1.1.1/1.1.1/1.1.2")
            .unwrap()
            .1
            .text
            .starts_with("TTSM"));
        assert!(world.find_post("k1").unwrap().1.text.starts_with("TTSM"));
        assert_eq!(
            world.find_post("k5v1j4f4ds79m9t").unwrap().0.title,
            "Fern wall"
        );
    }

    #[test]
    fn empty_start_creates_general_and_refuses_to_wipe() {
        let path = temp_path();
        let _file = TempFile(path.clone());
        Store::init_empty(&path).unwrap();
        let store = Store::open(&path).unwrap();
        let world = store.world();
        assert!(world.users.is_empty());
        assert_eq!(world.threads.len(), 2);
        assert_eq!(world.threads[0].title, seed::HALL);
        assert!(world.threads[0].messages.is_empty());
        assert_eq!(world.threads[1].title, seed::PRIMER);
        assert!(!store.doc().contains("Mira"));
        let before = fs::read(&path).unwrap();
        let err = Store::init_empty(&path).unwrap_err();
        assert!(err.to_string().contains("never overwrites"), "{err}");
        assert_eq!(fs::read(&path).unwrap(), before);

        let seeded = Store::open(demo_path()).unwrap();
        let seeded_path = seeded.path().to_path_buf();
        let _seeded = TempFile(seeded_path.clone());
        let demo = fs::read(&seeded_path).unwrap();
        assert!(demo.windows(4).any(|bytes| bytes == b"Mira") || seeded.doc().contains("Mira"));
        let err = Store::init_empty(&seeded_path).unwrap_err();
        assert!(err.to_string().contains("--seed"), "{err}");
        drop(seeded);
        assert_eq!(fs::read(&seeded_path).unwrap(), demo);
    }

    #[test]
    fn a_missing_file_starts_with_the_primer_not_the_cast() {
        let path = temp_path();
        let _file = TempFile(path.clone());
        assert!(!path.exists());
        let store = Store::open(&path).unwrap();
        assert!(path.exists());
        let world = store.world();
        assert!(world.users.is_empty());
        assert!(!store.doc().contains("Mira"));
        assert!(!store.doc().contains("Fern wall"));
        let hall = world.thread(seed::HALL_THREAD).unwrap();
        assert!(hall.messages.is_empty());
        let primer = world.thread(seed::PRIMER_THREAD).unwrap();
        assert_eq!(primer.title, seed::PRIMER);
        assert!(primer.messages.len() >= 4);
        assert!(primer
            .messages
            .iter()
            .all(|m| m.author == seed::PRIMER_AUTHOR));
        // The primer quotes its own addresses; they must be true.
        let here = primer
            .messages
            .iter()
            .find(|m| m.text.contains("This note lives at"))
            .unwrap();
        assert!(
            here.text.contains(&crate::link::post_id(here.coord)),
            "{}",
            here.text
        );
        let links = primer
            .messages
            .iter()
            .find(|m| m.text.starts_with("A link is just an address"))
            .unwrap();
        assert!(links
            .text
            .contains(&format!("/p/{}", crate::link::post_id(links.coord))));
        assert!(links
            .text
            .contains(&format!("/l/{}", crate::link::lane_id(primer.coord))));

        let before = fs::read(&path).unwrap();
        drop(store);
        let again = Store::open(&path).unwrap();
        assert_eq!(
            again.doc().as_bytes(),
            before.as_slice(),
            "reopening rewrote the file"
        );
    }

    #[test]
    fn oauth_profile_becomes_a_roster_scroll_and_interests_follow_use() {
        let path = temp_path();
        let _file = TempFile(path.clone());
        Store::init_empty(&path).unwrap();
        let mut store = Store::open(&path).unwrap();
        assert!(store.ensure_member("github:100", "The Octocat").unwrap());
        assert!(!store.ensure_member("github:100", "The Octocat").unwrap());
        let user = store.world().find_user("github:100").unwrap();
        assert_eq!(user.name, "The Octocat");
        assert!(user.interests.is_empty());
        assert!(user.blurb.is_empty());
        let raw = store.doc().to_string();
        assert!(raw.contains("id: github:100"));
        assert!(raw.contains("name: The Octocat"));

        let general = store.world().threads[0].coord;
        store.note_interest("github:100", general).unwrap();
        store.note_interest("github:100", general).unwrap();
        let user = store.world().find_user("The Octocat").unwrap();
        assert_eq!(user.interests.len(), 1);
        assert_eq!(thread_key(user.interests[0]), seed::HALL_THREAD);

        store.ensure_member("github:100", "Octo Cat").unwrap();
        let user = store.world().find_user("github:100").unwrap();
        assert_eq!(user.name, "Octo Cat");
        assert_eq!(user.interests.len(), 1);
    }
}
