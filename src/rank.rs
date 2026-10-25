//! Feed ranking and coordinate prediction.
//!
//! Thread score = `0.72 * proximity + 0.28 * recency`.
//! Proximity is `1 / (1 + weighted distance)` to the nearest interest.
//! Recency is `1 / (1 + age_hours / 18)`.
//! Distance is weighted so a library step (16) dominates a book step (1).
//! A subscribed thread therefore stays above a brand-new far thread.
//!
//! After threads are ordered, an author with at least [`LOUD_AT`] messages
//! may occupy at most [`AUTHOR_CAP`] lines in the whole feed. Everyone else
//! is left intact. The overflow is reported as a lane.

use chrono::{DateTime, Utc};
use libphext::phext::Coordinate;

use crate::coord::{self, thread_key, topic_key};
use crate::model::{self, Message, Thread};

pub const AUTHOR_CAP: usize = 2;
/// Authors with at least this many messages are loud. Only they are capped.
/// A reader with a handful of notes is left intact.
pub const LOUD_AT: usize = 8;
pub const MAX_THREADS: usize = 8;
const FLOOD_MIN_MESSAGES: usize = 10;
const FLOOD_SHARE: f64 = 0.70;

#[derive(Debug, Clone)]
pub struct Feed {
    pub threads: Vec<FeedThread>,
    pub lanes: Vec<Lane>,
}

#[derive(Debug, Clone)]
pub struct FeedThread {
    pub coord: Coordinate,
    pub topic: String,
    pub title: String,
    pub distance: u64,
    pub score: f64,
    pub nearness: &'static str,
    pub admitted: Vec<Message>,
    pub held: Vec<Message>,
}

#[derive(Debug, Clone)]
pub struct Lane {
    pub author: String,
    pub held: usize,
    pub shown: usize,
    pub threads: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub coord: Coordinate,
    pub thread: Coordinate,
    pub title: String,
    pub topic: String,
    pub new_thread: bool,
    pub overlap: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct MapEntry {
    pub coord: Coordinate,
    pub topic: String,
    pub title: String,
    pub distance: u64,
    pub message_count: usize,
    pub latest_author: Option<String>,
    pub latest_ts: Option<DateTime<Utc>>,
    pub preview: String,
    pub majority_author: Option<String>,
    pub majority_count: usize,
    pub majority_share: f64,
    pub flooded: bool,
}

pub fn thread_score(distance: u64, age_secs: u64) -> f64 {
    let proximity = 1.0 / (1.0 + distance as f64);
    let hours = age_secs as f64 / 3600.0;
    let recency = 1.0 / (1.0 + hours / 18.0);
    0.72 * proximity + 0.28 * recency
}

pub fn nearness(distance: u64) -> &'static str {
    match distance {
        0 => "subscribed",
        1..=3 => "next door",
        4..=12 => "same wing",
        _ => "across the library",
    }
}

pub fn age_secs(ts: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    if ts >= now {
        0
    } else {
        (now - ts).num_seconds().max(0) as u64
    }
}

fn newest_age(thread: &Thread, now: DateTime<Utc>) -> u64 {
    thread
        .messages
        .iter()
        .map(|message| age_secs(message.ts, now))
        .min()
        .unwrap_or(u64::MAX / 4)
}

pub fn build_feed(
    threads: &[Thread],
    interests: &[Coordinate],
    now: DateTime<Utc>,
    cap: usize,
    max_threads: usize,
) -> Feed {
    build_feed_for(threads, interests, now, cap, max_threads, None)
}

/// [`build_feed`] for one reader. The reader's own posts are never held:
/// the cap keeps other voices from flooding a feed, and nobody needs
/// protecting from themselves. `reader` is a [`Message::poster_key`].
pub fn build_feed_for(
    threads: &[Thread],
    interests: &[Coordinate],
    now: DateTime<Utc>,
    cap: usize,
    max_threads: usize,
    reader: Option<&str>,
) -> Feed {
    let cap = cap.max(1);
    let mut volume: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for thread in threads {
        if thread.plain {
            continue;
        }
        for message in &thread.messages {
            if message.plain {
                continue;
            }
            *volume.entry(message.poster_key()).or_default() += 1;
        }
    }
    let mut ranked: Vec<&Thread> = threads
        .iter()
        .filter(|thread| !thread.plain && thread.messages.iter().any(|message| !message.plain))
        .collect();
    ranked.sort_by(|a, b| {
        let score_a = thread_score(coord::min_distance(a.coord, interests), newest_age(a, now));
        let score_b = thread_score(coord::min_distance(b.coord, interests), newest_age(b, now));
        score_b
            .total_cmp(&score_a)
            .then_with(|| {
                coord::min_distance(a.coord, interests)
                    .cmp(&coord::min_distance(b.coord, interests))
            })
            .then_with(|| thread_key(a.coord).cmp(&thread_key(b.coord)))
    });

    let mut shown_by_author: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut feed_threads = Vec::new();
    for thread in ranked {
        if feed_threads.len() == max_threads {
            break;
        }
        let distance = coord::min_distance(thread.coord, interests);
        let score = thread_score(distance, newest_age(thread, now));
        let mut messages = thread.messages.clone();
        messages.sort_by(|a, b| b.ts.cmp(&a.ts).then(b.coord.cmp(&a.coord)));
        let mut admitted = Vec::new();
        let mut held = Vec::new();
        for message in messages {
            if message.plain {
                continue;
            }
            let key = message.poster_key().to_string();
            if reader.is_some_and(|reader| reader == key) {
                admitted.push(message);
                continue;
            }
            let limit = if volume.get(key.as_str()).copied().unwrap_or(0) >= LOUD_AT {
                cap
            } else {
                usize::MAX
            };
            let shown = shown_by_author.get(&key).copied().unwrap_or(0);
            if shown < limit {
                shown_by_author.insert(key, shown + 1);
                admitted.push(message);
            } else {
                held.push(message);
            }
        }
        feed_threads.push(FeedThread {
            coord: thread.coord,
            topic: thread.topic.clone(),
            title: thread.title.clone(),
            distance,
            score,
            nearness: nearness(distance),
            admitted,
            held,
        });
    }

    let mut lanes: Vec<Lane> = Vec::new();
    for thread in &feed_threads {
        for message in &thread.admitted {
            let lane = lane_mut(&mut lanes, &message.author);
            lane.shown += 1;
        }
        for message in &thread.held {
            let lane = lane_mut(&mut lanes, &message.author);
            lane.held += 1;
            if !lane.threads.contains(&thread.title) {
                lane.threads.push(thread.title.clone());
            }
        }
    }
    lanes.retain(|lane| lane.held > 0);
    lanes.sort_by(|a, b| b.held.cmp(&a.held).then(a.author.cmp(&b.author)));

    Feed {
        threads: feed_threads,
        lanes,
    }
}

fn lane_mut<'a>(lanes: &'a mut Vec<Lane>, author: &str) -> &'a mut Lane {
    if let Some(index) = lanes.iter().position(|lane| lane.author == author) {
        return &mut lanes[index];
    }
    lanes.push(Lane {
        author: author.to_string(),
        held: 0,
        shown: 0,
        threads: Vec::new(),
    });
    lanes.last_mut().expect("just pushed")
}

pub fn map_of(threads: &[Thread], interests: &[Coordinate]) -> Vec<MapEntry> {
    let mut entries: Vec<MapEntry> = threads
        .iter()
        .map(|thread| {
            let (majority_author, majority_count, majority_share) = majority(&thread.messages);
            let latest = thread.messages.iter().max_by_key(|message| message.ts);
            MapEntry {
                coord: thread.coord,
                topic: thread.topic.clone(),
                title: thread.title.clone(),
                distance: coord::min_distance(thread.coord, interests),
                message_count: thread.messages.len(),
                latest_author: latest.map(|message| message.author.clone()),
                latest_ts: latest.map(|message| message.ts),
                preview: latest
                    .map(|message| model::preview(&message.text, 88))
                    .unwrap_or_default(),
                majority_author,
                majority_count,
                majority_share,
                flooded: is_flooded(thread.messages.len(), majority_share),
            }
        })
        .collect();
    entries.sort_by(|a, b| a.coord.cmp(&b.coord));
    entries
}

pub fn majority(messages: &[Message]) -> (Option<String>, usize, f64) {
    if messages.is_empty() {
        return (None, 0, 0.0);
    }
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for message in messages {
        *counts.entry(message.author.as_str()).or_default() += 1;
    }
    let (author, count) = counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
        .expect("non-empty");
    let share = count as f64 / messages.len() as f64;
    (Some(author.to_string()), count, share)
}

pub fn is_flooded(count: usize, share: f64) -> bool {
    count >= FLOOD_MIN_MESSAGES && share >= FLOOD_SHARE
}

/// A new book on the reader's home shelf. Same placement as a suggestion
/// that shared no words. Used when someone explicitly starts a thread.
pub fn suggest_fresh(
    threads: &[Thread],
    interests: &[Coordinate],
    text: &str,
    _now: DateTime<Utc>,
) -> Suggestion {
    let home = home_thread(threads, interests);
    let fresh = fresh_thread(threads, home);
    let title = model::title_from_text(text);
    let topic = topic_name(threads, fresh);
    Suggestion {
        coord: coord::with_scroll(fresh, 2),
        thread: fresh,
        title,
        topic,
        new_thread: true,
        overlap: Vec::new(),
        reason: "A new thread.".into(),
    }
}

pub fn suggest(
    threads: &[Thread],
    interests: &[Coordinate],
    text: &str,
    now: DateTime<Utc>,
) -> Suggestion {
    let home = home_thread(threads, interests);
    let tokens = model::content_tokens(text);
    if text.trim().is_empty() || tokens.is_empty() && text.trim().is_empty() {
        return existing_or_fresh(threads, home, Vec::new(), empty_reason(), false);
    }

    let mut best: Option<Candidate> = None;
    for thread in threads {
        if thread.plain {
            continue;
        }
        let bag = thread_bag(thread);
        let mut overlap: Vec<String> = tokens.intersection(&bag).cloned().collect();
        overlap.sort();
        if overlap.is_empty() {
            continue;
        }
        let candidate = Candidate {
            overlap,
            distance: coord::min_distance(thread.coord, interests),
            age: newest_age(thread, now),
            thread,
        };
        let replace = match &best {
            None => true,
            Some(current) => candidate.rank_key() < current.rank_key(),
        };
        if replace {
            best = Some(candidate);
        }
    }

    if let Some(candidate) = best {
        let words = candidate.overlap.join(", ");
        let thread_label = thread_key(candidate.thread.coord);
        let reason = format!(
            "Nearest thread is {} ({thread_label}). Shared words: {words}.",
            candidate.thread.title
        );
        let coord = next_on(candidate.thread);
        return Suggestion {
            coord,
            thread: candidate.thread.coord,
            title: candidate.thread.title.clone(),
            topic: candidate.thread.topic.clone(),
            new_thread: false,
            overlap: candidate.overlap,
            reason,
        };
    }

    let fresh = fresh_thread(threads, home);
    let title = model::title_from_text(text);
    let topic = topic_name(threads, fresh);
    let reason = format!(
        "No shared words with an open thread. A new book under {topic} at {}.",
        thread_key(fresh)
    );
    Suggestion {
        coord: coord::with_scroll(fresh, 2),
        thread: fresh,
        title,
        topic,
        new_thread: true,
        overlap: Vec::new(),
        reason,
    }
}

struct Candidate<'a> {
    overlap: Vec<String>,
    distance: u64,
    age: u64,
    thread: &'a Thread,
}

impl Candidate<'_> {
    fn rank_key(&self) -> (std::cmp::Reverse<usize>, u64, u64, String) {
        (
            std::cmp::Reverse(self.overlap.len()),
            self.distance,
            self.age,
            thread_key(self.thread.coord),
        )
    }
}

fn empty_reason() -> String {
    "Type a message. The coordinate is a guess from shared words, not a language model.".into()
}

fn existing_or_fresh(
    threads: &[Thread],
    home: Coordinate,
    overlap: Vec<String>,
    reason: String,
    force_new: bool,
) -> Suggestion {
    if !force_new {
        if let Some(thread) = threads
            .iter()
            .find(|thread| !thread.plain && thread_key(thread.coord) == thread_key(home))
        {
            return Suggestion {
                coord: next_on(thread),
                thread: thread.coord,
                title: thread.title.clone(),
                topic: thread.topic.clone(),
                new_thread: false,
                overlap,
                reason,
            };
        }
    }
    let fresh = fresh_thread(threads, home);
    Suggestion {
        coord: coord::with_scroll(fresh, 2),
        thread: fresh,
        title: model::title_from_text(""),
        topic: topic_name(threads, fresh),
        new_thread: true,
        overlap,
        reason,
    }
}

fn home_thread(threads: &[Thread], interests: &[Coordinate]) -> Coordinate {
    if let Some(coord) = interests.first() {
        return coord::thread_anchor(*coord);
    }
    threads
        .iter()
        .find(|thread| !thread.plain)
        .or_else(|| threads.first())
        .map(|thread| coord::thread_anchor(thread.coord))
        .unwrap_or_else(libphext::phext::default_coordinate)
}

pub fn fresh_thread(threads: &[Thread], home: Coordinate) -> Coordinate {
    let mut volume = home.y.volume.max(1);
    let mut book = 1usize;
    for _ in 0..2000 {
        let mut coord = coord::thread_anchor(home);
        coord.y.volume = volume;
        coord.y.book = book;
        if !threads
            .iter()
            .any(|thread| thread_key(thread.coord) == thread_key(coord))
        {
            return coord;
        }
        book += 1;
        if book > 40 {
            book = 1;
            volume += 1;
        }
    }
    coord::thread_anchor(home)
}

fn topic_name(threads: &[Thread], coord: Coordinate) -> String {
    threads
        .iter()
        .find(|thread| topic_key(thread.coord) == topic_key(coord))
        .map(|thread| thread.topic.clone())
        .unwrap_or_else(|| format!("Shelf {}", topic_key(coord)))
}

fn next_on(thread: &Thread) -> Coordinate {
    let scroll = coord::next_message_scroll(
        thread
            .messages
            .iter()
            .filter(|message| message.coord.x.chapter == 1 && message.coord.x.section == 1)
            .map(|message| message.coord.x.scroll),
    );
    coord::with_scroll(thread.coord, scroll)
}

fn thread_bag(thread: &Thread) -> std::collections::BTreeSet<String> {
    let mut blob = format!("{} {} {}", thread.title, thread.topic, thread.keywords);
    let mut messages = thread.messages.clone();
    messages.sort_by_key(|message| message.ts);
    if messages.len() > 8 {
        messages = messages.split_off(messages.len() - 8);
    }
    for message in messages {
        blob.push(' ');
        blob.push_str(&message.text);
    }
    model::content_tokens(&blob)
}

pub fn admitted_by(feed: &Feed, author: &str) -> usize {
    feed.threads
        .iter()
        .flat_map(|thread| thread.admitted.iter())
        .filter(|message| message.author == author)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coord::{format_coord, parse_coord};
    use crate::model::Message;
    use chrono::Duration;

    fn fixed() -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 10, 22, 18, 0, 0).unwrap()
    }

    fn message(
        coord: &str,
        author: &str,
        minutes_ago: i64,
        text: &str,
        now: DateTime<Utc>,
    ) -> Message {
        Message {
            coord: parse_coord(coord).unwrap(),
            author: author.into(),
            who: String::new(),
            ts: now - Duration::minutes(minutes_ago),
            text: text.into(),
            plain: false,
        }
    }

    fn thread(coord: &str, title: &str, keywords: &str, messages: Vec<Message>) -> Thread {
        Thread {
            coord: coord::thread_anchor(parse_coord(coord).unwrap()),
            topic: "Topic".into(),
            title: title.into(),
            keywords: keywords.into(),
            messages,
            plain: false,
        }
    }

    #[test]
    fn nearer_thread_outranks_a_newer_far_thread() {
        let now = fixed();
        let stale = thread_score(0, 72 * 3600);
        let brand_new_far = thread_score(16, 0);
        assert!(
            stale > brand_new_far,
            "subscribed-but-stale {stale} should beat a new far thread {brand_new_far}"
        );

        let interest = parse_coord("1.2.1/1.1.1/1.1.1").unwrap();
        let near = thread(
            "1.2.1/1.1.1/1.1.1",
            "Fern wall",
            "",
            vec![message(
                "1.2.1/1.1.1/1.1.2",
                "Mira Quill",
                72 * 60,
                "old fern",
                now,
            )],
        );
        let far = thread(
            "4.1.1/1.1.1/1.1.1",
            "Bread hour",
            "",
            vec![message(
                "4.1.1/1.1.1/1.1.2",
                "Ren Sato",
                1,
                "fresh bread",
                now,
            )],
        );
        let feed = build_feed(
            &[far.clone(), near.clone()],
            &[interest],
            now,
            AUTHOR_CAP,
            MAX_THREADS,
        );
        assert_eq!(feed.threads[0].title, "Fern wall");
        assert_eq!(feed.threads[0].nearness, "subscribed");
        assert!(feed.threads[0].score > feed.threads[1].score);
    }

    #[test]
    fn newer_thread_wins_when_distance_ties() {
        let now = fixed();
        let fern = parse_coord("1.2.1/1.1.1/1.1.1").unwrap();
        let swap = parse_coord("1.2.1/1.2.1/1.1.1").unwrap();
        let older = thread(
            "1.2.1/1.1.1/1.1.1",
            "Fern wall",
            "",
            vec![message("1.2.1/1.1.1/1.1.2", "Mira Quill", 600, "old", now)],
        );
        let newer = thread(
            "1.2.1/1.2.1/1.1.1",
            "Seed swap",
            "",
            vec![message("1.2.1/1.2.1/1.1.2", "Mira Quill", 5, "new", now)],
        );
        let feed = build_feed(&[older, newer], &[fern, swap], now, AUTHOR_CAP, MAX_THREADS);
        assert_eq!(feed.threads[0].title, "Seed swap");
        assert_eq!(feed.threads[0].distance, 0);
        assert_eq!(feed.threads[1].distance, 0);
    }

    #[test]
    fn author_cap_collapses_a_hyper_poster_across_the_feed() {
        let now = fixed();
        let interest = parse_coord("1.1.1/1.1.1/1.1.1").unwrap();
        let mut hall = Vec::new();
        for minutes in 1..=5 {
            hall.push(message(
                &format!("1.1.1/1.1.1/1.1.{}", minutes + 1),
                "Casey Horn",
                minutes,
                "again",
                now,
            ));
        }
        let gardens = vec![
            message("1.2.1/1.1.1/1.1.2", "Casey Horn", 10, "drive-by", now),
            message("1.2.1/1.1.1/1.1.3", "Casey Horn", 11, "another", now),
            message("1.2.1/1.1.1/1.1.4", "Casey Horn", 12, "and another", now),
            message("1.2.1/1.1.1/1.1.5", "Casey Horn", 13, "still going", now),
            message("1.2.1/1.1.1/1.1.6", "Mira Quill", 30, "the ferns", now),
        ];
        let hall_thread = thread("1.1.1/1.1.1/1.1.1", "General", "", hall);
        let garden_thread = thread("1.2.1/1.1.1/1.1.1", "Fern wall", "", gardens);
        let feed = build_feed(
            &[hall_thread, garden_thread],
            &[interest],
            now,
            AUTHOR_CAP,
            MAX_THREADS,
        );
        assert_eq!(admitted_by(&feed, "Casey Horn"), AUTHOR_CAP);
        let lane = feed
            .lanes
            .iter()
            .find(|lane| lane.author == "Casey Horn")
            .unwrap();
        assert_eq!(lane.held, 7);
        assert_eq!(lane.shown, AUTHOR_CAP);
        assert!(feed
            .threads
            .iter()
            .flat_map(|thread| thread.admitted.iter())
            .any(|m| m.author == "Mira Quill"));
        let shown: usize = feed
            .threads
            .iter()
            .map(|thread| thread.admitted.len())
            .sum();
        let share = admitted_by(&feed, "Casey Horn") as f64 / shown as f64;
        assert!(share <= AUTHOR_CAP as f64 / shown as f64);
        assert!(share < 0.75, "capped share was {share}");
    }

    #[test]
    fn suggest_uses_keyword_overlap() {
        let now = fixed();
        let interest = parse_coord("1.2.1/1.1.1/1.1.1").unwrap();
        let fern = thread(
            "1.2.1/1.1.1/1.1.2",
            "Fern wall",
            "fern moss shade garden",
            vec![message(
                "1.2.1/1.1.1/1.1.2",
                "Mira Quill",
                40,
                "north wall fronds",
                now,
            )],
        );
        let bread = thread(
            "4.1.1/1.1.1/1.1.2",
            "Bread hour",
            "bread dough starter oven",
            vec![message(
                "4.1.1/1.1.1/1.1.2",
                "Ren Sato",
                10,
                "the loaf needs time",
                now,
            )],
        );
        let suggestion = suggest(
            &[bread, fern],
            &[interest],
            "The ferns on the north wall need more shade",
            now,
        );
        assert_eq!(suggestion.title, "Fern wall");
        assert!(!suggestion.new_thread);
        assert!(suggestion.overlap.iter().any(|word| word == "fern"));
        assert!(suggestion.overlap.iter().any(|word| word == "shade"));
        assert_eq!(format_coord(suggestion.thread), "1.2.1/1.1.1/1.1.1");
        assert_eq!(suggestion.coord.x.scroll, 3);
    }

    #[test]
    fn suggest_opens_a_new_book_when_nothing_overlaps() {
        let now = fixed();
        let interest = parse_coord("1.2.1/1.1.1/1.1.1").unwrap();
        let fern = thread(
            "1.2.1/1.1.1/1.1.1",
            "Fern wall",
            "fern moss shade",
            vec![message(
                "1.2.1/1.1.1/1.1.2",
                "Mira Quill",
                10,
                "fronds",
                now,
            )],
        );
        let swap = thread(
            "1.2.1/1.2.1/1.1.1",
            "Seed swap",
            "seed dill fennel",
            vec![message(
                "1.2.1/1.2.1/1.1.2",
                "Mira Quill",
                20,
                "envelopes",
                now,
            )],
        );
        let suggestion = suggest(&[fern, swap], &[interest], "zzzz quantum badminton", now);
        assert!(suggestion.new_thread);
        assert_eq!(thread_key(suggestion.thread), "1.2.1/1.1.2");
        assert_eq!(format_coord(suggestion.coord), "1.2.1/1.1.2/1.1.2");
        assert!(suggestion.overlap.is_empty());
    }

    #[test]
    fn plain_scrolls_do_not_spend_the_author_cap() {
        let now = fixed();
        let interest = parse_coord("1.1.1/1.1.1/1.1.1").unwrap();
        let mut prose = thread(
            "1.1.1/1.1.1/1.1.1",
            "Incipit (It Begins)",
            "",
            (1..=12)
                .map(|scroll| {
                    message(
                        &format!("1.1.1/1.1.1/1.1.{scroll}"),
                        "phext",
                        scroll,
                        "a scroll",
                        now,
                    )
                })
                .collect(),
        );
        prose.plain = true;
        for message in &mut prose.messages {
            message.plain = true;
        }
        let chat = thread(
            "1.2.1/1.1.1/1.1.1",
            "Fern wall",
            "",
            vec![message("1.2.1/1.1.1/1.1.2", "Mira Quill", 3, "ferns", now)],
        );
        let feed = build_feed(&[prose, chat], &[interest], now, AUTHOR_CAP, MAX_THREADS);
        assert_eq!(feed.threads.len(), 1);
        assert_eq!(feed.threads[0].title, "Fern wall");
        assert!(feed.lanes.is_empty());
        assert_eq!(admitted_by(&feed, "phext"), 0);
    }

    #[test]
    fn a_reader_is_never_held_back_from_their_own_posts() {
        let now = fixed();
        let interest = parse_coord("1.1.1/1.1.1/1.1.1").unwrap();
        let threads: Vec<Thread> = (1..=MAX_THREADS)
            .map(|book| {
                let mut note = message(
                    &format!("1.1.1/1.1.{book}/1.1.2"),
                    "wbic16",
                    book as i64,
                    "solo",
                    now,
                );
                note.who = "google:1".into();
                thread(&format!("1.1.1/1.1.{book}/1.1.1"), "Solo", "", vec![note])
            })
            .collect();
        let others = build_feed(&threads, &[interest], now, AUTHOR_CAP, MAX_THREADS);
        let shown = |feed: &Feed| {
            feed.threads
                .iter()
                .filter(|t| !t.admitted.is_empty())
                .count()
        };
        assert_eq!(shown(&others), AUTHOR_CAP);
        let own = build_feed_for(
            &threads,
            &[interest],
            now,
            AUTHOR_CAP,
            MAX_THREADS,
            Some("google:1"),
        );
        assert_eq!(shown(&own), MAX_THREADS);
        assert!(own.threads.iter().all(|t| t.held.is_empty()));
    }

    #[test]
    fn share_caps_follow_the_oauth_identity() {
        let now = fixed();
        let interest = parse_coord("1.1.1/1.1.1/1.1.1").unwrap();
        let mut same = Vec::new();
        for scroll in 2..=9 {
            let mut note = message(
                &format!("1.1.1/1.1.1/1.1.{scroll}"),
                if scroll % 2 == 0 { "Ann" } else { "Bea" },
                scroll,
                "again",
                now,
            );
            note.who = "github:1".into();
            same.push(note);
        }
        let hall = thread("1.1.1/1.1.1/1.1.1", "Hall", "", same);
        let feed = build_feed(&[hall], &[interest], now, AUTHOR_CAP, MAX_THREADS);
        let admitted = feed.threads[0]
            .admitted
            .iter()
            .filter(|message| message.who == "github:1")
            .count();
        assert_eq!(admitted, AUTHOR_CAP);

        let mut split = Vec::new();
        for (index, who) in ["github:1", "github:2"].iter().enumerate() {
            for scroll in 2..=9 {
                let mut note = message(
                    &format!("1.{}.{}/1.1.1/1.1.{scroll}", index + 1, index + 1),
                    "Ann",
                    scroll,
                    "again",
                    now,
                );
                note.who = (*who).into();
                split.push((index, note));
            }
        }
        let left = thread(
            "1.1.1/1.1.1/1.1.1",
            "Left",
            "",
            split
                .iter()
                .filter(|(index, _)| *index == 0)
                .map(|(_, note)| note.clone())
                .collect(),
        );
        let right = thread(
            "1.2.2/1.1.1/1.1.1",
            "Right",
            "",
            split
                .into_iter()
                .filter(|(index, _)| *index == 1)
                .map(|(_, note)| note)
                .collect(),
        );
        let feed = build_feed(&[left, right], &[interest], now, AUTHOR_CAP, MAX_THREADS);
        let admitted = feed
            .threads
            .iter()
            .flat_map(|thread| thread.admitted.iter())
            .filter(|message| message.author == "Ann")
            .count();
        assert_eq!(admitted, AUTHOR_CAP * 2);
    }
}
