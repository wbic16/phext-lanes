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
        let lower = trimmed.to_lowercase();
        self.users
            .iter()
            .find(|user| user.id == lower || user.name.eq_ignore_ascii_case(trimmed))
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
        if let Some(coord) = crate::link::parse_section_lane_id(id) {
            let key = section_key(coord);
            return self
                .threads
                .iter()
                .find(|thread| thread.plain && section_key(thread.coord) == key);
        }
        let coord = crate::link::parse_lane_id(id)?;
        if let Some(thread) = self
            .threads
            .iter()
            .find(|thread| !thread.plain && thread_key(thread.coord) == thread_key(coord))
        {
            return Some(thread);
        }
        self.threads.iter().find(|thread| {
            thread.plain
                && thread.coord.x.chapter == 1
                && thread.coord.x.section == 1
                && thread_key(thread.coord) == thread_key(coord)
        })
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
        let mut store = Store::from_doc(path, String::new());
        store.reseed(Utc::now())?;
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
        let body = model::format_message(&author, post.now, &text);
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
                Scroll::Message { author, ts, text } => {
                    let key = thread_key(coord);
                    anchors
                        .entry(key.clone())
                        .or_insert_with(|| thread_anchor(coord));
                    messages.entry(key).or_default().push(Message {
                        coord,
                        author,
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
        let path = temp_path();
        let _file = TempFile(path.clone());
        let mut store = Store::open(&path).unwrap();
        let placed = store
            .place(Post {
                author: seed::MIRA.into(),
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
        let path = temp_path();
        let _file = TempFile(path.clone());
        let mut store = Store::open(&path).unwrap();
        let placed = store
            .place(Post {
                author: "mira".into(),
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
        assert_eq!(crate::link::section_lane_id(incipit.coord), "s0");
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
        assert_eq!(crate::link::lane_id(fern.coord), "kcre66i9s");
        assert_eq!(world.find_lane("kcre66i9s").unwrap().title, "Fern wall");
        assert_eq!(world.find_lane("s0").unwrap().title, "Incipit (It Begins)");
        assert!(world.find_post("k1").unwrap().1.text.starts_with("TTSM"));
        assert_eq!(
            world.find_post("k5v1j4f4ds79m9t").unwrap().0.title,
            "Fern wall"
        );
    }
}
