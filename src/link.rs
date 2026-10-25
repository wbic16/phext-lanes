//! Public addresses for a thread (`/l/…`) and a message (`/p/…`).
//!
//! The address is the phext coordinate itself, in canonical form. A chat
//! thread is its six topic and thread parts (`/l/1.2.1/1.1.1`). A section of
//! ordinary text is the full coordinate of its first scroll
//! (`/l/1.1.1/1.1.1/2.1.1`), so it never shares an address with a chat lane.
//! A message is its full coordinate (`/p/1.2.1/1.1.1/1.1.2`). Semicolons
//! parse in place of slashes.
//!
//! Earlier builds packed the coordinate into a base-36 id (`k…`, `s…`).
//! Those still parse, and [`canonical_lane`] / [`canonical_post`] turn them
//! into the coordinate form so old links can redirect.

use libphext::phext::Coordinate;

use crate::coord::{format_coord, parse_coord, section_anchor, thread_anchor, thread_key};

const PREFIX: char = 'k';
const SECTION_PREFIX: char = 's';
const RADIX: u128 = 1000;
const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

pub fn lane_id(coord: Coordinate) -> String {
    thread_key(coord)
}

pub fn post_id(coord: Coordinate) -> String {
    format_coord(coord)
}

/// Address of one chapter and section: the coordinate of its first scroll.
pub fn section_lane_id(coord: Coordinate) -> String {
    format_coord(section_anchor(coord))
}

pub fn parse_lane_id(id: &str) -> Option<Coordinate> {
    if let Some(parts) = arms(id) {
        if parts.len() != 2 {
            return None;
        }
        return parse_coord(&format!("{}/{}/1.1.1", parts[0], parts[1]))
            .ok()
            .map(thread_anchor);
    }
    let parts = decode(id, 6)?;
    let coord = coord_from(&parts)?;
    Some(thread_anchor(coord))
}

pub fn parse_section_lane_id(id: &str) -> Option<Coordinate> {
    if let Some(parts) = arms(id) {
        if parts.len() != 3 {
            return None;
        }
        return parse_coord(&parts.join("/")).ok().map(section_anchor);
    }
    let parts = decode_prefixed(SECTION_PREFIX, id, 8)?;
    let coord = section_anchor(coord_from(&parts)?);
    if legacy_section_id(coord)? != id {
        return None;
    }
    Some(coord)
}

pub fn parse_post_id(id: &str) -> Option<Coordinate> {
    if let Some(parts) = arms(id) {
        if parts.len() != 3 {
            return None;
        }
        return parse_coord(&parts.join("/")).ok();
    }
    let parts = decode(id, 9)?;
    coord_from(&parts)
}

/// The canonical address for a lane id in any accepted form.
pub fn canonical_lane(id: &str) -> Option<String> {
    if let Some(coord) = parse_section_lane_id(id) {
        return Some(section_lane_id(coord));
    }
    parse_lane_id(id).map(lane_id)
}

/// The canonical address for a post id in any accepted form.
pub fn canonical_post(id: &str) -> Option<String> {
    parse_post_id(id).map(post_id)
}

/// `1.2.1/1.1.1` or `1.2.1;1.1.1;1.1.2` split into its dotted arms.
/// None when the text is not shaped like a coordinate at all.
fn arms(id: &str) -> Option<Vec<&str>> {
    if id.is_empty()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '/' | ';'))
    {
        return None;
    }
    let parts: Vec<&str> = id.split(['/', ';']).collect();
    let shaped = parts.iter().all(|arm| {
        let nums: Vec<&str> = arm.split('.').collect();
        nums.len() == 3 && nums.iter().all(|n| !n.is_empty())
    });
    if shaped {
        Some(parts)
    } else {
        Some(Vec::new())
    }
}

fn legacy_section_id(coord: Coordinate) -> Option<String> {
    encode_prefixed(SECTION_PREFIX, &section_parts(coord))
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

#[cfg(test)]
fn post_parts(coord: Coordinate) -> [usize; 9] {
    let section = section_parts(coord);
    [
        section[0],
        section[1],
        section[2],
        section[3],
        section[4],
        section[5],
        section[6],
        section[7],
        coord.x.scroll,
    ]
}

#[cfg(test)]
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
    fn addresses_are_canonical_coordinates() {
        assert_eq!(lane_id(at("1.1.1/1.1.1/1.1.1")), "1.1.1/1.1.1");
        assert_eq!(lane_id(at("1.2.1/1.1.1/9.9.9")), "1.2.1/1.1.1");
        assert_eq!(lane_id(at("1.2.1/1.2.1/1.1.1")), "1.2.1/1.2.1");
        assert_eq!(post_id(at("1.1.1/1.1.1/1.1.2")), "1.1.1/1.1.1/1.1.2");
        assert_eq!(post_id(at("1.2.1/1.1.1/1.1.2")), "1.2.1/1.1.1/1.1.2");
        assert_eq!(
            section_lane_id(at("1.1.1/1.1.1/1.1.1")),
            "1.1.1/1.1.1/1.1.1"
        );
        assert_eq!(
            section_lane_id(at("1.1.1/1.1.1/2.3.7")),
            "1.1.1/1.1.1/2.3.1"
        );
    }

    #[test]
    fn semicolons_parse_and_canonicalize() {
        assert_eq!(
            parse_post_id("1.2.1;1.1.1;1.1.2"),
            Some(at("1.2.1/1.1.1/1.1.2"))
        );
        assert_eq!(
            canonical_lane("1.2.1;1.1.1").as_deref(),
            Some("1.2.1/1.1.1")
        );
        assert_eq!(
            canonical_post("1.2.1;1.1.1;1.1.2").as_deref(),
            Some("1.2.1/1.1.1/1.1.2")
        );
        assert_eq!(
            canonical_lane("1.1.1/1.1.1/2.1.5").as_deref(),
            Some("1.1.1/1.1.1/2.1.1")
        );
    }

    #[test]
    fn legacy_ids_still_resolve_to_the_same_coordinates() {
        assert_eq!(parse_lane_id("k0"), Some(at("1.1.1/1.1.1/1.1.1")));
        assert_eq!(parse_lane_id("kcre66i9s"), Some(at("1.2.1/1.1.1/1.1.1")));
        assert_eq!(parse_lane_id("kcre66j1k"), Some(at("1.2.1/1.2.1/1.1.1")));
        assert_eq!(parse_post_id("k1"), Some(at("1.1.1/1.1.1/1.1.2")));
        assert_eq!(
            parse_post_id("k5v1j4f4ds79m9t"),
            Some(at("1.2.1/1.1.1/1.1.2"))
        );
        assert_eq!(parse_section_lane_id("s0"), Some(at("1.1.1/1.1.1/1.1.1")));
        assert_eq!(canonical_lane("kcre66i9s").as_deref(), Some("1.2.1/1.1.1"));
        assert_eq!(canonical_lane("s0").as_deref(), Some("1.1.1/1.1.1/1.1.1"));
        assert_eq!(
            canonical_post("k5v1j4f4ds79m9t").as_deref(),
            Some("1.2.1/1.1.1/1.1.2")
        );
        let fern = at("1.2.1/1.1.1/1.1.4");
        assert_eq!(
            encode(&post_parts(fern)).and_then(|id| parse_post_id(&id)),
            Some(fern)
        );
    }

    #[test]
    fn section_ids_roundtrip_without_taking_a_chat_lane() {
        let fern = at("1.2.1/1.1.1/1.1.1");
        let section = section_lane_id(fern);
        assert_ne!(section, lane_id(fern));
        assert_eq!(parse_section_lane_id(&section), Some(section_anchor(fern)));
        assert!(parse_section_lane_id("1.2.1/1.1.1").is_none());
        assert!(parse_section_lane_id("kcre66i9s").is_none());
        assert!(parse_section_lane_id("k0").is_none());
        assert!(parse_lane_id("s0").is_none());
        assert!(parse_lane_id("1.1.1/1.1.1/1.1.1").is_none());
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
        assert!(parse_lane_id("nope").is_none());
        assert!(parse_lane_id("1.2/1.1.1").is_none());
        assert!(parse_lane_id("0.1.1/1.1.1").is_none());
        assert!(parse_lane_id("1.1.1//1.1.1").is_none());
        assert!(parse_post_id("1.2.1/1.1.1/1.1.2/").is_none());
        assert!(parse_post_id("1.2.1/1.1.1").is_none());
        assert!(parse_post_id("k5v1j4f4ds79m9t/").is_none());
        assert!(parse_post_id("1001.1.1/1.1.1/1.1.2").is_none());
        assert!(parse_lane_id(&post_id(at("1.2.1/1.1.1/1.1.2"))).is_none());
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
        assert_eq!(lane_id(fern.coord), seed::FERN_THREAD);
        let opening = fern
            .messages
            .iter()
            .find(|message| message.text.contains("north-wall ferns"))
            .unwrap();
        assert_eq!(post_id(opening.coord), "1.2.1/1.1.1/1.1.2");
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
