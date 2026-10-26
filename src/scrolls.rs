//! A linear-time phext index.
//!
//! libphext 0.3.1 tokenizes by copying the rest of the document after every
//! scroll, so `explode`, `fetch`, and `textmap` are quadratic. A 3.5 MB
//! fabric took about 40 seconds to open. This module walks the bytes once.
//!
//! [`tokenize`] maps every non-empty scroll to its byte range, using the
//! same break rules as `phext::explode`. [`serialize`] writes scrolls back
//! with the same minimal breaks as `phext::implode`. Both are checked
//! against libphext in the tests below.

use std::ops::Range;

use libphext::phext::{self, Coordinate};

/// Every non-empty scroll and where its text sits in `doc`, in document
/// order. Coordinates only grow, so the output is sorted.
pub fn tokenize(doc: &str) -> Vec<(Coordinate, Range<usize>)> {
    let bytes = doc.as_bytes();
    let mut out = Vec::new();
    let mut location = phext::default_coordinate();
    let mut start: Option<usize> = None;
    for (at, byte) in bytes.iter().enumerate() {
        if !is_break(*byte) {
            if start.is_none() {
                start = Some(at);
            }
            continue;
        }
        if let Some(begin) = start.take() {
            out.push((location, begin..at));
        }
        advance(&mut location, *byte);
    }
    if let Some(begin) = start {
        out.push((location, begin..bytes.len()));
    }
    out
}

/// A phext document from scrolls in ascending coordinate order. Empty
/// scrolls are skipped, as `phext::implode` skips them.
pub fn serialize<'a>(scrolls: impl IntoIterator<Item = (Coordinate, &'a str)>) -> String {
    let mut out = String::new();
    let mut at = phext::default_coordinate();
    for (coord, text) in scrolls {
        if text.is_empty() {
            continue;
        }
        move_to(&mut out, &mut at, coord);
        out.push_str(text);
    }
    out
}

/// One line per scroll, in the format of `phext::textmap`.
pub fn textmap(doc: &str, scrolls: &[(Coordinate, Range<usize>)]) -> String {
    let mut out = String::new();
    for (coord, range) in scrolls {
        out.push_str(&format!(
            "* {coord}: {}\n",
            phext::create_summary(&doc[range.clone()])
        ));
    }
    out
}

fn is_break(byte: u8) -> bool {
    matches!(
        byte,
        0x01 | 0x17 | 0x18 | 0x19 | 0x1A | 0x1C | 0x1D | 0x1E | 0x1F
    )
}

fn advance(location: &mut Coordinate, byte: u8) {
    match byte {
        0x17 => location.scroll_break(),
        0x18 => location.section_break(),
        0x19 => location.chapter_break(),
        0x1A => location.book_break(),
        0x1C => location.volume_break(),
        0x1D => location.collection_break(),
        0x1E => location.series_break(),
        0x1F => location.shelf_break(),
        0x01 => location.library_break(),
        _ => {}
    }
}

/// The fewest breaks from `at` to `target`, highest dimension first.
fn move_to(out: &mut String, at: &mut Coordinate, target: Coordinate) {
    while *at < target {
        let step = if at.z.library < target.z.library {
            0x01
        } else if at.z.shelf < target.z.shelf {
            0x1F
        } else if at.z.series < target.z.series {
            0x1E
        } else if at.y.collection < target.y.collection {
            0x1D
        } else if at.y.volume < target.y.volume {
            0x1C
        } else if at.y.book < target.y.book {
            0x1A
        } else if at.x.chapter < target.x.chapter {
            0x19
        } else if at.x.section < target.x.section {
            0x18
        } else if at.x.scroll < target.x.scroll {
            0x17
        } else {
            // `at < target` with no lower arm: target is out of order.
            break;
        };
        out.push(char::from(step));
        advance(at, step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn ours(doc: &str) -> HashMap<Coordinate, String> {
        tokenize(doc)
            .into_iter()
            .map(|(coord, range)| (coord, doc[range].to_string()))
            .collect()
    }

    fn theirs(doc: &str) -> HashMap<Coordinate, String> {
        phext::explode(doc)
            .into_iter()
            .filter(|(_, text)| !text.is_empty())
            .collect()
    }

    fn samples() -> Vec<String> {
        let mut docs = vec![
            String::new(),
            "plain text, one scroll\nwith a second line".to_string(),
            "a\x17b\x18c\x19d\x1Ae\x1Cf\x1Dg\x1Eh\x1Fi\x01j".to_string(),
            "\x17\x17lead\x17\x17\x17gap\x01\x01\x17deep\x17".to_string(),
            "ünïcödé ✓\x1Fshelf ☃\x1Aboek ü\x17".to_string(),
            "\x01\x1F\x1E\x1D\x1C\x1A\x19\x18\x17only breaks then text".to_string(),
        ];
        docs.push(crate::seed::document(crate::seed::demo_instant()));
        docs.push(crate::seed::blank(crate::seed::demo_instant()));
        docs
    }

    #[test]
    fn tokenize_matches_libphext_explode() {
        for doc in samples() {
            assert_eq!(ours(&doc), theirs(&doc), "{doc:?}");
        }
    }

    #[test]
    fn serialize_matches_libphext_implode() {
        for doc in samples() {
            let tokens = tokenize(&doc);
            let mine = serialize(tokens.iter().map(|(c, r)| (*c, &doc[r.clone()])));
            let reference = phext::implode(phext::explode(&doc));
            assert_eq!(mine, reference, "{doc:?}");
            assert_eq!(ours(&mine), ours(&doc));
        }
    }

    #[test]
    fn textmap_matches_libphext() {
        for doc in samples() {
            // libphext also lists empty scrolls (an empty file, trailing breaks).
            let reference: String = phext::textmap(&doc)
                .lines()
                .filter(|line| !line.ends_with(": "))
                .map(|line| format!("{line}\n"))
                .collect();
            assert_eq!(textmap(&doc, &tokenize(&doc)), reference, "{doc:?}");
        }
    }

    #[test]
    fn a_large_document_tokenizes_in_linear_time() {
        let mut doc = String::new();
        for index in 0..50_000 {
            if index > 0 {
                // 40 scrolls a book, 40 books a volume: every arm stays <= 1000.
                doc.push(if index % 1600 == 0 {
                    '\x1C'
                } else if index % 40 == 0 {
                    '\x1A'
                } else {
                    '\x17'
                });
            }
            doc.push_str("kind: message\nauthor: bench\n---\nsome words here");
        }
        let start = std::time::Instant::now();
        let tokens = tokenize(&doc);
        let rebuilt = serialize(tokens.iter().map(|(c, r)| (*c, &doc[r.clone()])));
        assert_eq!(tokens.len(), 50_000);
        assert_eq!(rebuilt, doc);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "took {:?}",
            start.elapsed()
        );
    }
}
