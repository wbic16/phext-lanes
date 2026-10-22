//! Scroll formats stored inside the phext.
//!
//! Thread card (scroll `…/1.1.1`):
//! ```text
//! kind: thread
//! topic: Gardens
//! title: Fern wall
//! keywords: fern moss shade
//! ```
//!
//! Message (scroll `…/1.1.N`, N ≥ 2):
//! ```text
//! kind: message
//! author: Mira Quill
//! ts: 2026-10-22T18:00:00Z
//! ---
//! The north-wall ferns finally unfurled.
//! ```
//!
//! Reader (library 5):
//! ```text
//! kind: user
//! id: mira
//! name: Mira Quill
//! blurb: Reads the gardens and answers slowly.
//! interests: 1.2.1/1.1.1/1.1.1 1.2.1/1.2.1/1.1.1
//! ```

use chrono::{DateTime, SecondsFormat, Utc};
use libphext::phext::Coordinate;

use crate::coord::{self, format_coord, parse_coord};

#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub name: String,
    pub blurb: String,
    pub interests: Vec<Coordinate>,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub coord: Coordinate,
    pub author: String,
    pub ts: DateTime<Utc>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub coord: Coordinate,
    pub topic: String,
    pub title: String,
    pub keywords: String,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone)]
pub enum Scroll {
    User(User),
    Card {
        topic: String,
        title: String,
        keywords: String,
    },
    Message {
        author: String,
        ts: DateTime<Utc>,
        text: String,
    },
}

impl User {
    pub fn to_scroll(&self) -> String {
        let interests = self
            .interests
            .iter()
            .map(|coord| format_coord(*coord))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "kind: user\nid: {}\nname: {}\nblurb: {}\ninterests: {interests}",
            coord::one_line(&self.id),
            coord::one_line(&self.name),
            coord::one_line(&self.blurb),
        )
    }
}

pub fn format_card(topic: &str, title: &str, keywords: &str) -> String {
    format!(
        "kind: thread\ntopic: {}\ntitle: {}\nkeywords: {}",
        coord::one_line(topic),
        coord::one_line(title),
        coord::one_line(keywords),
    )
}

pub fn format_message(author: &str, ts: DateTime<Utc>, text: &str) -> String {
    let stamp = ts.to_rfc3339_opts(SecondsFormat::Secs, true);
    format!(
        "kind: message\nauthor: {}\nts: {stamp}\n---\n{}",
        coord::one_line(author),
        coord::sanitize(text).trim_end_matches(['\n', '\r'])
    )
}

pub fn title_from_text(text: &str) -> String {
    let line = coord::one_line(text);
    let mut title = line
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    if title.chars().count() > 48 {
        title = title.chars().take(48).collect();
    }
    if title.is_empty() {
        "New thread".to_string()
    } else {
        title
    }
}

pub fn keyword_line(text: &str) -> String {
    let mut words: Vec<String> = content_tokens(text).into_iter().collect();
    words.sort();
    words.truncate(8);
    words.join(" ")
}

pub fn parse_scroll(raw: &str) -> Option<Scroll> {
    let raw = raw.replace("\r\n", "\n");
    if raw.trim().is_empty() {
        return None;
    }
    let mut headers = std::collections::BTreeMap::<String, String>::new();
    let mut body = String::new();
    let mut in_body = false;
    for line in raw.split('\n') {
        if !in_body && line.trim() == "---" {
            in_body = true;
            continue;
        }
        if in_body {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(line);
        } else if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_lowercase(), value.trim().to_string());
        }
    }
    while body.ends_with('\n') {
        body.pop();
    }

    let kind = headers.get("kind").map(String::as_str).unwrap_or("");
    match kind {
        "user" => {
            let id = headers.get("id")?.trim().to_lowercase();
            let name = headers.get("name")?.trim().to_string();
            if id.is_empty() || name.is_empty() {
                return None;
            }
            let blurb = headers.get("blurb").cloned().unwrap_or_default();
            let interests = headers
                .get("interests")
                .map(String::as_str)
                .unwrap_or("")
                .split_whitespace()
                .filter_map(|token| parse_coord(token).ok())
                .collect();
            Some(Scroll::User(User {
                id,
                name,
                blurb,
                interests,
            }))
        }
        "thread" => {
            let title = headers.get("title")?.trim().to_string();
            if title.is_empty() {
                return None;
            }
            Some(Scroll::Card {
                topic: headers.get("topic").cloned().unwrap_or_default(),
                title,
                keywords: headers.get("keywords").cloned().unwrap_or_default(),
            })
        }
        "message" => {
            let author = headers.get("author")?.trim().to_string();
            let ts = DateTime::parse_from_rfc3339(headers.get("ts")?.trim())
                .ok()?
                .with_timezone(&Utc);
            if author.is_empty() {
                return None;
            }
            Some(Scroll::Message {
                author,
                ts,
                text: body,
            })
        }
        _ => None,
    }
}

const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "you", "your", "are", "was", "were",
    "have", "has", "had", "but", "not", "its", "just", "about", "into", "than", "then", "them",
    "they", "she", "his", "her", "our", "out", "any", "all", "can", "will", "what", "when", "who",
    "how", "why", "more", "need", "needs", "dont", "doesnt", "its", "ive", "youre", "theyre",
];

/// Lowercased content words. A trailing "s" is stripped so "ferns" meets "fern".
pub fn content_tokens(text: &str) -> std::collections::BTreeSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.len() >= 3)
        .map(stem)
        .filter(|word| word.len() >= 3 && !STOP.contains(&word.as_str()))
        .collect()
}

fn stem(word: &str) -> String {
    let mut word = word.to_lowercase();
    if word.len() > 4 && word.ends_with("ies") {
        word.truncate(word.len() - 3);
        word.push('y');
        return word;
    }
    if word.len() > 4 && word.ends_with('s') && !word.ends_with("ss") {
        word.pop();
    }
    word
}

pub fn preview(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= limit {
        flat
    } else {
        let mut cut: String = chars.into_iter().take(limit).collect();
        cut.push('…');
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coord::parse_coord;

    #[test]
    fn message_scroll_roundtrips_including_newlines() {
        let ts = DateTime::parse_from_rfc3339("2026-10-22T18:04:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let raw = format_message("Mira Quill", ts, "line one\nline two");
        match parse_scroll(&raw) {
            Some(Scroll::Message {
                author,
                text,
                ts: parsed,
            }) => {
                assert_eq!(author, "Mira Quill");
                assert_eq!(text, "line one\nline two");
                assert_eq!(parsed, ts);
            }
            other => panic!("parsed {other:?}"),
        }
    }

    #[test]
    fn user_scroll_keeps_interest_order() {
        let user = User {
            id: "mira".into(),
            name: "Mira Quill".into(),
            blurb: "Reads the gardens.".into(),
            interests: vec![
                parse_coord("1.2.1/1.1.1/1.1.1").unwrap(),
                parse_coord("1.2.1/1.2.1/1.1.1").unwrap(),
            ],
        };
        match parse_scroll(&user.to_scroll()) {
            Some(Scroll::User(parsed)) => {
                assert_eq!(parsed.id, "mira");
                assert_eq!(parsed.interests, user.interests);
            }
            other => panic!("parsed {other:?}"),
        }
    }

    #[test]
    fn ferns_stem_to_fern() {
        let tokens = content_tokens("The ferns on the north wall need more shade");
        assert!(tokens.contains("fern"));
        assert!(tokens.contains("shade"));
        assert!(tokens.contains("north"));
        assert!(!tokens.contains("need"));
        assert!(!tokens.contains("the"));
    }
}
