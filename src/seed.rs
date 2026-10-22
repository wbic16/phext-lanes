//! Fictional reading room used to show the feed against a flooded hall.
//!
//! Casey Horn is the hyper-poster. The other four readers are deliberately
//! sparse. Nothing here is a real person.

use chrono::{DateTime, Duration, TimeZone, Utc};
use libphext::phext::{self, Coordinate};

use crate::coord::{self, parse_coord};
use crate::model::{self, User};

pub const MIRA: &str = "Mira Quill";
pub const JULES: &str = "Jules Fen";
pub const ADA: &str = "Ada North";
pub const REN: &str = "Ren Sato";
pub const CASEY: &str = "Casey Horn";

pub const HALL: &str = "General";
pub const FERN: &str = "Fern wall";
pub const SWAP: &str = "Seed swap";
pub const PLANES: &str = "Hand planes";
pub const RIVER: &str = "River wards";
pub const BREAD: &str = "Bread hour";

pub const HALL_THREAD: &str = "1.1.1/1.1.1";
pub const FERN_THREAD: &str = "1.2.1/1.1.1";
pub const SWAP_THREAD: &str = "1.2.1/1.2.1";
pub const PLANES_THREAD: &str = "2.1.1/1.1.1";
pub const RIVER_THREAD: &str = "3.1.1/1.1.1";
pub const BREAD_THREAD: &str = "4.1.1/1.1.1";

struct UserSpec {
    id: &'static str,
    name: &'static str,
    blurb: &'static str,
    interests: &'static [&'static str],
}

struct RoomSpec {
    topic: &'static str,
    thread: &'static str,
    title: &'static str,
    keywords: &'static str,
    lines: &'static [(&'static str, i64, &'static str)],
}

const USERS: &[UserSpec] = &[
    UserSpec {
        id: "mira",
        name: MIRA,
        blurb: "Reads the gardens and answers slowly.",
        interests: &["1.2.1/1.1.1/1.1.1", "1.2.1/1.2.1/1.1.1"],
    },
    UserSpec {
        id: "jules",
        name: JULES,
        blurb: "Keeps the workshop notes short.",
        interests: &["2.1.1/1.1.1/1.1.1"],
    },
    UserSpec {
        id: "ada",
        name: ADA,
        blurb: "Traces maps and river wards.",
        interests: &["3.1.1/1.1.1/1.1.1"],
    },
    UserSpec {
        id: "ren",
        name: REN,
        blurb: "Bakes during bread hour, then leaves.",
        interests: &["4.1.1/1.1.1/1.1.1"],
    },
    UserSpec {
        id: "casey",
        name: CASEY,
        blurb: "Posts everywhere, immediately, again.",
        interests: &["1.1.1/1.1.1/1.1.1"],
    },
];

const ROOMS: &[RoomSpec] = &[
    RoomSpec {
        topic: "The Hall",
        thread: HALL_THREAD,
        title: HALL,
        keywords: "hall general hello quiet room",
        lines: &[
            (REN, 520, "Bread hour is at four, for anyone who wants a quieter room."),
            (JULES, 420, "The plane notes are under Workshops, for anyone who wants them without the noise."),
            (MIRA, 320, "I'll be in the fern notes if you need me."),
            (CASEY, 280, "is this thing on"),
            (CASEY, 200, "bump"),
            (CASEY, 140, "hello again"),
            (CASEY, 90, "we should pin me so nobody misses this"),
            (CASEY, 65, "ok that was not the last one"),
            (CASEY, 48, "one more thought and then I'll stop"),
            (CASEY, 34, "why is it so quiet"),
            (CASEY, 26, "still waiting on replies"),
            (CASEY, 18, "don't scroll past this one"),
            (CASEY, 11, "big news: I think we should all talk in here"),
            (CASEY, 6, "posting again so this doesn't get buried"),
            (CASEY, 2, "hello?? is anyone actually here"),
        ],
    },
    RoomSpec {
        topic: "Gardens",
        thread: FERN_THREAD,
        title: FERN,
        keywords: "fern moss shade garden frond quiet north wall",
        lines: &[
            (MIRA, 400, "The north-wall ferns finally unfurled. The shade is right; the soil is still a little sour."),
            (CASEY, 100, "you should post more photos"),
            (MIRA, 90, "Moved the moss tray off the stone so it stops drying at the edge."),
            (CASEY, 70, "bumping this"),
            (JULES, 50, "Shade cloth frame is up. Two stakes, one crosspiece, nothing clever."),
            (CASEY, 45, "hello fern people, why so quiet"),
            (MIRA, 30, "Leaving the fronds alone this week. They don't need more water, they need less fuss."),
            (CASEY, 15, "ferns are cool I guess!! everyone come look"),
        ],
    },
    RoomSpec {
        topic: "Gardens",
        thread: SWAP_THREAD,
        title: SWAP,
        keywords: "seed swap dill fennel basil envelope garden",
        lines: &[
            (MIRA, 600, "Last year's basil went to seed in the cold frame. Envelopes are on the bench."),
            (ADA, 240, "I'll trade a river-map tracing for the fennel, if that's not a strange offer."),
            (MIRA, 180, "I have extra dill seed and a packet of bronze fennel if anyone wants to trade."),
        ],
    },
    RoomSpec {
        topic: "Workshops",
        thread: PLANES_THREAD,
        title: PLANES,
        keywords: "plane blade sharpen wood shaving workshop iron",
        lines: &[
            (JULES, 900, "If you only tune one thing on a hand plane, make it the chip breaker seat."),
            (JULES, 300, "The secondary bevel was too steep. I reset it and the shavings came back."),
            (CASEY, 35, "hello workshop, big news soon"),
            (JULES, 80, "The No. 4 needed a flatter sole more than a sharper iron. It tracks now."),
            (CASEY, 20, "tools tools tools!! anyone selling a plane??"),
        ],
    },
    RoomSpec {
        topic: "Cities",
        thread: RIVER_THREAD,
        title: RIVER,
        keywords: "river map ward bridge city street alley water",
        lines: &[
            (ADA, 800, "The city reads differently if you follow water instead of streets."),
            (ADA, 360, "Redrew the river ward from the old survey. Two alleys are missing on the newer map."),
            (CASEY, 48, "has anyone here been to the bridge lol"),
            (ADA, 100, "Walked the east ward again. The footbridge shifts the current into the market stair."),
            (CASEY, 22, "cities are great, posting so this stays on top"),
        ],
    },
    RoomSpec {
        topic: "Kitchens",
        thread: BREAD_THREAD,
        title: BREAD,
        keywords: "bread dough starter oven kitchen loaf",
        lines: &[
            (REN, 700, "Bread hour is for whoever wants a quiet bench and a warm slice."),
            (MIRA, 260, "Saved a heel from last week. It made better toast than I expected."),
            (REN, 200, "Folded the dough twice and left it alone. The oven is hotter than the dial says."),
            (CASEY, 60, "bump, the bread thread is too quiet"),
            (REN, 40, "The starter is slow today. I'm giving the loaf an extra half hour, no extra yeast."),
            (CASEY, 28, "why isn't this busier"),
            (CASEY, 10, "bread!! everyone drop what you're doing"),
        ],
    },
];

pub fn demo_instant() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 22, 18, 0, 0).unwrap()
}

pub fn readers() -> Vec<User> {
    USERS
        .iter()
        .map(|spec| User {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            blurb: spec.blurb.to_string(),
            interests: spec
                .interests
                .iter()
                .map(|coord| parse_coord(coord).expect("seed interest"))
                .collect(),
        })
        .collect()
}

/// Build the demo phext by inserting each scroll in coordinate order.
pub fn document(now: DateTime<Utc>) -> String {
    let mut rows: Vec<(Coordinate, String)> = Vec::new();
    for (index, user) in readers().into_iter().enumerate() {
        let mut coord = phext::default_coordinate();
        coord.z.library = coord::ROSTER_LIBRARY;
        coord.x.scroll = index + 1;
        rows.push((coord, user.to_scroll()));
    }
    for room in ROOMS {
        let anchor = parse_coord(&format!("{}/1.1.1", room.thread)).expect("seed thread");
        rows.push((
            anchor,
            model::format_card(room.topic, room.title, room.keywords),
        ));
        let mut lines = room.lines.to_vec();
        lines.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(b.2)));
        for (offset, (author, minutes, text)) in lines.into_iter().enumerate() {
            let coord = coord::with_scroll(anchor, offset + 2);
            let ts = now - Duration::minutes(minutes);
            rows.push((coord, model::format_message(author, ts, text)));
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let mut seen = std::collections::BTreeSet::new();
    let mut doc = String::new();
    for (coord, text) in rows {
        if !seen.insert(coord) {
            panic!("duplicate seed coordinate {coord}");
        }
        doc = phext::insert(doc, coord, &text);
    }
    doc
}
