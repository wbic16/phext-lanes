//! The short view of a feed.
//!
//! Ranking and the author cap still happen in [`crate::rank`]. This module
//! only decides what a person sees: one line per thread, and a count of
//! messages the cap held back.

use crate::coord::{format_coord, thread_key};
use crate::model::{preview, Thread};
use crate::rank::{self, Feed, FeedThread};

pub fn format_feed(feed: &Feed, details: bool) -> String {
    if !details {
        let mut lines = Vec::new();
        let mut held = 0usize;
        for thread in &feed.threads {
            held += thread.held.len();
            lines.push(calm_line(thread));
        }
        if held > 0 {
            lines.push(format!("{held} more held"));
        }
        return lines.join("\n");
    }

    let mut out = String::new();
    for thread in &feed.threads {
        out.push_str(&calm_line(thread));
        out.push('\n');
        out.push_str(&format!(
            "  {}  {}  distance {}  score {:.2}\n",
            thread_key(thread.coord),
            thread.nearness,
            thread.distance,
            thread.score
        ));
        if !thread.held.is_empty() {
            out.push_str(&format!(
                "  {} more held in this thread\n",
                thread.held.len()
            ));
        }
        out.push('\n');
    }
    if !feed.lanes.is_empty() {
        out.push_str("held\n");
        for lane in &feed.lanes {
            out.push_str(&format!(
                "  {}  {} not shown, {} shown\n",
                lane.author, lane.held, lane.shown
            ));
        }
    }
    out.trim_end().to_string()
}

pub fn held_count(feed: &Feed) -> usize {
    feed.threads.iter().map(|thread| thread.held.len()).sum()
}

fn calm_line(thread: &FeedThread) -> String {
    match thread.admitted.first() {
        Some(message) => format!(
            "{} — {}: {}",
            thread.title,
            message.author,
            preview(&message.text, 88)
        ),
        None => thread.title.clone(),
    }
}

pub fn format_thread(thread: &Thread, details: bool, limits: &rank::Limits) -> String {
    let mut out = String::new();
    if details {
        out.push_str(&format!("{}  {}\n", thread.topic, thread_key(thread.coord)));
    }
    out.push_str(&thread.title);
    out.push_str("\n\n");
    if details {
        let (author, count, share) = rank::majority(&thread.messages);
        if let Some(author) = author {
            out.push_str(&format!(
                "{author} wrote {count} of {} ({:.0}%)\n",
                thread.messages.len(),
                share * 100.0
            ));
            if limits.is_flooded(thread.messages.len(), share) {
                out.push_str("flooded\n");
            }
            out.push('\n');
        }
    }
    for message in &thread.messages {
        if details {
            out.push_str(&format!(
                "{}  {}\n",
                message.author,
                format_coord(message.coord)
            ));
        } else {
            out.push_str(&message.author);
            out.push('\n');
        }
        out.push_str(&message.text);
        out.push_str("\n\n");
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coord::parse_coord;
    use crate::model::Message;
    use crate::rank::{FeedThread, Lane};
    use chrono::{Duration, TimeZone, Utc};

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 22, 18, 0, 0).unwrap()
    }

    fn message(text: &str) -> Message {
        Message {
            coord: parse_coord("1.2.1/1.1.1/1.1.2").unwrap(),
            author: "Mira Quill".into(),
            who: String::new(),
            ts: now() - Duration::minutes(5),
            text: text.into(),
            plain: false,
        }
    }

    fn sample(held: usize) -> Feed {
        let coord = parse_coord("1.2.1/1.1.1/1.1.1").unwrap();
        Feed {
            threads: vec![FeedThread {
                coord,
                topic: "Gardens".into(),
                title: "Fern wall".into(),
                distance: 0,
                score: 0.9,
                nearness: "subscribed",
                admitted: vec![message("The north-wall ferns finally unfurled.")],
                held: (0..held).map(|_| message("again")).collect(),
            }],
            lanes: if held == 0 {
                Vec::new()
            } else {
                vec![Lane {
                    author: "Casey Horn".into(),
                    held,
                    shown: 1,
                    threads: vec!["Fern wall".into()],
                }]
            },
        }
    }

    #[test]
    fn default_feed_is_one_line_and_a_held_note() {
        let text = format_feed(&sample(1), false);
        assert_eq!(
            text,
            "Fern wall — Mira Quill: The north-wall ferns finally unfurled.\n1 more held"
        );
        assert!(!text.contains("1.2.1"));
        assert!(!text.contains("lane"));
        assert!(!text.contains("score"));
    }

    #[test]
    fn several_held_messages_use_the_same_note() {
        let text = format_feed(&sample(4), false);
        assert!(text.ends_with("\n4 more held"));
        assert_eq!(held_count(&sample(4)), 4);
    }

    #[test]
    fn nothing_held_omits_the_note() {
        let text = format_feed(&sample(0), false);
        assert_eq!(text.lines().count(), 1);
        assert!(!text.contains("held"));
    }

    #[test]
    fn details_show_coordinates_scores_and_who_was_held() {
        let text = format_feed(&sample(4), true);
        assert!(text.contains("1.2.1/1.1.1"));
        assert!(text.contains("subscribed"));
        assert!(text.contains("distance 0"));
        assert!(text.contains("score 0.90"));
        assert!(text.contains("Casey Horn"));
        assert!(text.contains("4 not shown, 1 shown"));
    }

    #[test]
    fn seeded_mira_feed_counts_the_capped_messages() {
        let moment = crate::seed::demo_instant();
        let world = crate::store::world_from(&crate::seed::document(moment));
        let mira = world.find_user("mira").unwrap();
        let feed = rank::build_feed(
            &world.threads,
            &mira.interests,
            moment,
            &rank::Limits::default(),
        );
        let text = format_feed(&feed, false);
        let body: Vec<_> = text
            .lines()
            .filter(|line| !line.ends_with("more held"))
            .collect();
        assert_eq!(body.len(), feed.threads.len());
        assert!(body[0].starts_with("Fern wall — "));
        assert!(!text.contains("1.2.1"));
        assert!(!text.contains("score"));
        assert!(!text.contains("distance"));
        let note = text.lines().last().unwrap();
        let n: usize = note.split_whitespace().next().unwrap().parse().unwrap();
        let from_lanes: usize = feed.lanes.iter().map(|lane| lane.held).sum();
        assert_eq!(n, from_lanes);
        assert_eq!(n, held_count(&feed));
        assert!(n >= 15, "held {n}");
        let detailed = format_feed(&feed, true);
        assert!(detailed.contains("1.2.1/1.1.1"));
        assert!(detailed.contains("Casey Horn"));
    }

    #[test]
    fn reading_a_thread_hides_coordinates_until_details() {
        let moment = crate::seed::demo_instant();
        let world = crate::store::world_from(&crate::seed::document(moment));
        let fern = world.find_thread("Fern wall").unwrap();
        let plain = format_thread(fern, false, &rank::Limits::default());
        assert!(plain.starts_with("Fern wall\n"));
        assert!(plain.contains("Mira Quill"));
        assert!(!plain.contains("1.2.1"));
        assert!(!plain.contains("flooded"));
        let detailed = format_thread(fern, true, &rank::Limits::default());
        assert!(detailed.contains("1.2.1/1.1.1"));
        assert!(detailed.contains("Gardens"));
    }
}
