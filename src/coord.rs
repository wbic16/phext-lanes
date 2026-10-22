//! Phext addresses used by the chat.
//!
//! A canonical coordinate is `LB.SF.SR/CN.VM.BK/CH.SN.SC`.
//! Column and line (the other two of phext's 11 dimensions) live inside
//! the scroll as characters and newlines. URL form swaps `/` for `;`.

use std::collections::BTreeSet;

use libphext::phext::{self, Coordinate};

/// Library reserved for reader records. Chat topics use the other libraries.
pub const ROSTER_LIBRARY: usize = 5;

/// Library, shelf, series, collection, volume, book.
/// Topic steps outweigh thread steps, so a different library cannot outrank
/// a subscribed shelf just by being newer.
pub const WEIGHTS: [u64; 6] = [16, 8, 4, 2, 2, 1];

pub fn parse_coord(input: &str) -> Result<Coordinate, String> {
    let normalized = input.trim().replace(';', "/");
    let coord = Coordinate::try_from(normalized.as_str()).map_err(|err| err.to_string())?;
    if !coord.validate_coordinate() {
        return Err(format!(
            "coordinate out of range (each part is 1..=1000): {input}"
        ));
    }
    Ok(coord)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A thread. The next free scroll is chosen at write time.
    Thread(Coordinate),
    /// A specific scroll. Scroll 1 is the thread card, so a message lands
    /// on the next free scroll instead of overwriting it.
    Scroll(Coordinate),
}

pub fn parse_target(input: &str) -> Result<Target, String> {
    let normalized = input.trim().replace(';', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [z, y] => {
            let coord = parse_coord(&format!("{z}/{y}/1.1.1"))?;
            Ok(Target::Thread(coord))
        }
        [_, _, _] => Ok(Target::Scroll(parse_coord(&normalized)?)),
        _ => Err("use a thread (1.2.1/1.1.1) or a scroll (1.2.1/1.1.1/1.1.2)".into()),
    }
}

pub fn format_coord(coord: Coordinate) -> String {
    format!("{coord}")
}

pub fn topic_key(coord: Coordinate) -> String {
    format!("{}", coord.z)
}

pub fn thread_key(coord: Coordinate) -> String {
    format!("{}/{}", coord.z, coord.y)
}

pub fn thread_anchor(coord: Coordinate) -> Coordinate {
    let mut coord = coord;
    coord.x.chapter = 1;
    coord.x.section = 1;
    coord.x.scroll = 1;
    coord
}

pub fn with_scroll(thread: Coordinate, scroll: usize) -> Coordinate {
    let mut coord = thread_anchor(thread);
    coord.x.scroll = scroll;
    coord
}

pub fn is_roster(coord: Coordinate) -> bool {
    coord.z.library == ROSTER_LIBRARY
}

pub fn is_card_slot(coord: Coordinate) -> bool {
    coord.x.chapter == 1 && coord.x.section == 1 && coord.x.scroll == 1
}

pub fn arms(coord: Coordinate) -> [usize; 6] {
    [
        coord.z.library,
        coord.z.shelf,
        coord.z.series,
        coord.y.collection,
        coord.y.volume,
        coord.y.book,
    ]
}

pub fn weighted_distance(a: Coordinate, b: Coordinate) -> u64 {
    arms(a)
        .into_iter()
        .zip(arms(b))
        .zip(WEIGHTS)
        .map(|((left, right), weight)| (left.abs_diff(right) as u64) * weight)
        .sum()
}

pub fn min_distance(thread: Coordinate, interests: &[Coordinate]) -> u64 {
    interests
        .iter()
        .map(|interest| weighted_distance(thread, *interest))
        .min()
        .unwrap_or(0)
}

/// Smallest free message scroll in chapter 1, section 1.
/// Scroll 1 is reserved for the thread card.
pub fn next_message_scroll(used: impl IntoIterator<Item = usize>) -> usize {
    let mut taken: BTreeSet<usize> = used.into_iter().collect();
    taken.insert(1);
    let mut scroll = 2;
    while taken.contains(&scroll) {
        scroll += 1;
        if scroll > phext::COORDINATE_MAXIMUM {
            break;
        }
    }
    scroll
}

/// Strip dimension breaks so a message cannot split the phext.
/// Newlines stay; they are inside the scroll.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            !matches!(
                *ch,
                '\u{0000}'
                    | '\u{0001}'
                    | '\u{0017}'
                    | '\u{0018}'
                    | '\u{0019}'
                    | '\u{001A}'
                    | '\u{001C}'
                    | '\u{001D}'
                    | '\u{001E}'
                    | '\u{001F}'
            )
        })
        .collect()
}

pub fn one_line(text: &str) -> String {
    sanitize(text)
        .replace(['\n', '\r', ':'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinate_arms_match_the_phext_address() {
        let coord = parse_coord("1.2.3/4.5.6/7.8.9").unwrap();
        assert_eq!(coord, phext::to_coordinate("1.2.3/4.5.6/7.8.9"));
        assert_eq!(coord, Coordinate::try_from("1.2.3/4.5.6/7.8.9").unwrap());
        assert_eq!(coord.z.library, 1);
        assert_eq!(coord.z.shelf, 2);
        assert_eq!(coord.z.series, 3);
        assert_eq!(coord.y.collection, 4);
        assert_eq!(coord.y.volume, 5);
        assert_eq!(coord.y.book, 6);
        assert_eq!(coord.x.chapter, 7);
        assert_eq!(coord.x.section, 8);
        assert_eq!(coord.x.scroll, 9);
        assert_eq!(topic_key(coord), "1.2.3");
        assert_eq!(thread_key(coord), "1.2.3/4.5.6");
        assert_eq!(thread_key(thread_anchor(coord)), thread_key(coord));
        assert!(is_card_slot(thread_anchor(coord)));
        assert!(!is_card_slot(coord));
    }

    #[test]
    fn semicolon_url_form_parses_as_the_same_coordinate() {
        let canonical = parse_coord("1.2.3/4.5.6/7.8.9").unwrap();
        let url = parse_coord("1.2.3;4.5.6;7.8.9").unwrap();
        assert_eq!(canonical, url);
        assert_eq!(
            parse_target("1.2.1/1.1.1").unwrap(),
            Target::Thread(parse_coord("1.2.1/1.1.1/1.1.1").unwrap())
        );
    }

    #[test]
    fn garbage_and_zero_are_rejected() {
        assert!(parse_coord("nope").is_err());
        assert!(parse_coord("0.1.1/1.1.1/1.1.1").is_err());
        assert!(parse_coord("1.1.1/1.1.1").is_err());
        // libphext::to_coordinate does not fail closed on junk.
        assert_eq!(phext::to_coordinate("nope"), phext::default_coordinate());
    }

    #[test]
    fn delimiters_match_the_phext_spec() {
        assert_eq!(phext::SCROLL_BREAK as u32, 0x17);
        assert_eq!(phext::SECTION_BREAK as u32, 0x18);
        assert_eq!(phext::CHAPTER_BREAK as u32, 0x19);
        assert_eq!(phext::BOOK_BREAK as u32, 0x1A);
        assert_eq!(phext::VOLUME_BREAK as u32, 0x1C);
        assert_eq!(phext::COLLECTION_BREAK as u32, 0x1D);
        assert_eq!(phext::SERIES_BREAK as u32, 0x1E);
        assert_eq!(phext::SHELF_BREAK as u32, 0x1F);
        assert_eq!(phext::LIBRARY_BREAK as u32, 0x01);
        assert_eq!(phext::LINE_BREAK as u32, 0x0A);
    }

    #[test]
    fn distance_weights_topic_over_thread() {
        let origin = parse_coord("1.1.1/1.1.1/1.1.1").unwrap();
        let next_book = parse_coord("1.1.1/1.1.2/1.1.1").unwrap();
        let next_shelf = parse_coord("1.2.1/1.1.1/1.1.1").unwrap();
        let next_library = parse_coord("2.1.1/1.1.1/1.1.1").unwrap();
        assert_eq!(weighted_distance(origin, next_book), WEIGHTS[5]);
        assert_eq!(weighted_distance(origin, next_shelf), WEIGHTS[1]);
        assert_eq!(weighted_distance(origin, next_library), WEIGHTS[0]);
        assert!(weighted_distance(origin, next_library) > weighted_distance(origin, next_shelf));
        assert!(weighted_distance(origin, next_shelf) > weighted_distance(origin, next_book));
        // The scroll axis is not part of thread distance.
        let other_scroll = parse_coord("1.1.1/1.1.1/1.1.9").unwrap();
        assert_eq!(weighted_distance(origin, other_scroll), 0);
    }
}
