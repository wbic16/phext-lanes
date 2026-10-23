//! Stable public ids for a thread (`/l/<id>`) and a message (`/p/<id>`).
//!
//! An id is a pure function of the phext coordinate. Nothing is written
//! beside the scroll, so the same post keeps the same address after a
//! restart, a redeploy, or a reseed of the demo. The encoding is frozen:
//! a leading `k`, then the base-36 form of the address packed in base 1000.
//! Lane ids use the six thread parts. Post ids use all nine, including the
//! scroll. A section of ordinary text uses the same alphabet and radix with
//! an `s` prefix and eight parts (the thread, plus chapter and section), so
//! it never shares an id with a chat lane. Do not change the alphabet, the
//! radix, or the `k` prefix.

use libphext::phext::Coordinate;

use crate::coord::{parse_coord, section_anchor, thread_anchor};

const PREFIX: char = 'k';
const SECTION_PREFIX: char = 's';
const RADIX: u128 = 1000;
const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

pub fn lane_id(coord: Coordinate) -> String {
    encode(&lane_parts(coord)).expect("lane coordinate is in range")
}

pub fn post_id(coord: Coordinate) -> String {
    encode(&post_parts(coord)).expect("post coordinate is in range")
}

/// Stable id for one chapter and section. Prefix `s`, eight coordinate parts.
pub fn section_lane_id(coord: Coordinate) -> String {
    encode_prefixed(SECTION_PREFIX, &section_parts(coord)).expect("section coordinate is in range")
}

pub fn parse_lane_id(id: &str) -> Option<Coordinate> {
    let parts = decode(id, 6)?;
    let coord = coord_from(&parts)?;
    Some(thread_anchor(coord))
}

pub fn parse_section_lane_id(id: &str) -> Option<Coordinate> {
    let parts = decode_prefixed(SECTION_PREFIX, id, 8)?;
    let coord = section_anchor(coord_from(&parts)?);
    if section_lane_id(coord) != id {
        return None;
    }
    Some(coord)
}

pub fn parse_post_id(id: &str) -> Option<Coordinate> {
    let parts = decode(id, 9)?;
    coord_from(&parts)
}

fn lane_parts(coord: Coordinate) -> [usize; 6] {
    [
        coord.z.library,
        coord.z.shelf,
        coord.z.series,
        coord.y.collection,
        coord.y.volume,
        coord.y.book,
    ]
}

fn section_parts(coord: Coordinate) -> [usize; 8] {
    let lane = lane_parts(coord);
    [
        lane[0],
        lane[1],
        lane[2],
        lane[3],
        lane[4],
        lane[5],
        coord.x.chapter,
        coord.x.section,
    ]
}

fn post_parts(coord: Coordinate) -> [usize; 9] {
    let lane = lane_parts(coord);
    [
        lane[0],
        lane[1],
        lane[2],
        lane[3],
        lane[4],
        lane[5],
        coord.x.chapter,
        coord.x.section,
        coord.x.scroll,
    ]
}

fn encode(parts: &[usize]) -> Option<String> {
    encode_prefixed(PREFIX, parts)
}

fn encode_prefixed(prefix: char, parts: &[usize]) -> Option<String> {
    let mut packed = 0u128;
    for part in parts {
        if !(1..=1000).contains(part) {
            return None;
        }
        packed = packed
            .checked_mul(RADIX)?
            .checked_add((*part - 1) as u128)?;
    }
    Some(format!("{prefix}{}", base36(packed)))
}

fn decode(id: &str, len: usize) -> Option<Vec<usize>> {
    decode_prefixed(PREFIX, id, len)
}

fn decode_prefixed(prefix: char, id: &str, len: usize) -> Option<Vec<usize>> {
    let body = id.strip_prefix(prefix)?;
    if body.is_empty() || body.len() > 40 || !canonical(body) {
        return None;
    }
    let mut packed = decode_base36(body)?;
    let mut parts = vec![0usize; len];
    for slot in parts.iter_mut().rev() {
        let digit = (packed % RADIX) as usize;
        if digit > 999 {
            return None;
        }
        *slot = digit + 1;
        packed /= RADIX;
    }
    if packed != 0 {
        return None;
    }
    Some(parts)
}

fn canonical(body: &str) -> bool {
    body == "0" || !body.starts_with('0')
}

fn base36(mut n: u128) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let mut buf = [0u8; 32];
    let mut index = buf.len();
    while n > 0 {
        index -= 1;
        buf[index] = ALPHABET[(n % 36) as usize];
        n /= 36;
    }
    String::from_utf8(buf[index..].to_vec()).expect("base36 alphabet is ascii")
}

fn decode_base36(text: &str) -> Option<u128> {
    let mut n = 0u128;
    for byte in text.bytes() {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'z' => byte - b'a' + 10,
            _ => return None,
        };
        n = n.checked_mul(36)?.checked_add(u128::from(digit))?;
    }
    Some(n)
}

fn coord_from(parts: &[usize]) -> Option<Coordinate> {
    let chapter = parts.get(6).copied().unwrap_or(1);
    let section = parts.get(7).copied().unwrap_or(1);
    let scroll = parts.get(8).copied().unwrap_or(1);
    parse_coord(&format!(
        "{}.{}.{}/{}.{}.{}/{chapter}.{section}.{scroll}",
        parts[0], parts[1], parts[2], parts[3], parts[4], parts[5]
    ))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coord::{parse_coord, section_anchor};
    use crate::model::Message;
    use crate::seed;
    use crate::store::world_from;
    use chrono::Duration;

    fn at(text: &str) -> Coordinate {
        parse_coord(text).unwrap()
    }

    #[test]
    fn known_coordinates_keep_these_ids() {
        assert_eq!(lane_id(at("1.1.1/1.1.1/1.1.1")), "k0");
        assert_eq!(lane_id(at("1.2.1/1.1.1/9.9.9")), "kcre66i9s");
        assert_eq!(lane_id(at("1.2.1/1.2.1/1.1.1")), "kcre66j1k");
        assert_eq!(post_id(at("1.1.1/1.1.1/1.1.2")), "k1");
        assert_eq!(post_id(at("1.2.1/1.1.1/1.1.2")), "k5v1j4f4ds79m9t");
        assert_eq!(section_lane_id(at("1.1.1/1.1.1/1.1.1")), "s0");
    }

    #[test]
    fn section_ids_roundtrip_without_taking_a_chat_lane() {
        let fern = at("1.2.1/1.1.1/1.1.1");
        let section = section_lane_id(fern);
        assert_ne!(section, lane_id(fern));
        assert_eq!(parse_section_lane_id(&section), Some(section_anchor(fern)));
        assert!(parse_section_lane_id("kcre66i9s").is_none());
        assert!(parse_section_lane_id("k0").is_none());
        assert!(parse_lane_id("s0").is_none());
        let chapter = at("1.1.1/1.1.1/2.1.3");
        let id = section_lane_id(chapter);
        let parsed = parse_section_lane_id(&id).unwrap();
        assert_eq!(parsed.x.chapter, 2);
        assert_eq!(parsed.x.section, 1);
        assert_eq!(parsed.x.scroll, 1);
        assert_ne!(id, lane_id(chapter));
    }

    #[test]
    fn ids_roundtrip_and_ignore_nothing_inside_the_scroll() {
        let fern = at("1.2.1/1.1.1/1.1.4");
        assert_eq!(parse_lane_id(&lane_id(fern)), Some(thread_anchor(fern)));
        assert_eq!(parse_post_id(&post_id(fern)), Some(fern));
        assert_eq!(lane_id(fern), lane_id(thread_anchor(fern)));
        assert_ne!(post_id(fern), post_id(at("1.2.1/1.1.1/1.1.5")));
        assert_eq!(lane_id(fern), lane_id(at("1.2.1/1.1.1/1.1.5")));
    }

    #[test]
    fn malformed_ids_do_not_parse() {
        assert!(parse_lane_id("").is_none());
        assert!(parse_lane_id("k").is_none());
        assert!(parse_lane_id("0").is_none());
        assert!(parse_lane_id("K0").is_none());
        assert!(parse_lane_id("k00").is_none());
        assert!(parse_post_id("k5v1j4f4ds79m9t/").is_none());
        assert!(parse_lane_id("nope").is_none());
        assert!(parse_lane_id(&post_id(at("1.2.1/1.1.1/1.1.2"))).is_none());
        assert_eq!(
            parse_post_id("k0").map(|coord| coord.x.scroll),
            Some(1),
            "the card slot shares the lane's packed number"
        );
    }

    #[test]
    fn seeded_posts_get_the_same_ids_at_any_clock() {
        let early = world_from(&seed::document(seed::demo_instant()));
        let later = world_from(&seed::document(seed::demo_instant() + Duration::days(3)));
        let early_ids = ids_of(&early);
        let later_ids = ids_of(&later);
        assert_eq!(early_ids, later_ids);
        assert!(early_ids.len() > 20);
        let unique: std::collections::BTreeSet<_> = early_ids.iter().map(|item| &item.2).collect();
        assert_eq!(unique.len(), early_ids.len());
        let fern = early
            .threads
            .iter()
            .find(|thread| thread.title == seed::FERN)
            .unwrap();
        assert_eq!(lane_id(fern.coord), "kcre66i9s");
        let opening = fern
            .messages
            .iter()
            .find(|message| message.text.contains("north-wall ferns"))
            .unwrap();
        assert_eq!(post_id(opening.coord), "k5v1j4f4ds79m9t");
        assert_eq!(opening.coord.x.scroll, 2);
    }

    fn ids_of(world: &crate::store::World) -> Vec<(String, String, String)> {
        let mut rows = Vec::new();
        for thread in &world.threads {
            for message in &thread.messages {
                rows.push(row(thread.title.as_str(), message));
            }
        }
        rows.sort();
        rows
    }

    fn row(title: &str, message: &Message) -> (String, String, String) {
        (
            title.to_string(),
            message.text.clone(),
            post_id(message.coord),
        )
    }
}
