# Lanes

```bash
git clone <repo-url> lanes && cd lanes && cargo run
```

Open http://127.0.0.1:7841. You are Mira. Type a message and press Enter. The first run writes `data/chat.phext`.

---

Predictive multi-chat, stored as one [phext](https://phext.io) file. The page is one feed: a line for each relevant thread, then a compose box. Messages that would flood the feed stay in the file and show up as a quiet "N more held" note.

The cast is fictional: Mira Quill, Jules Fen, Ada North, Ren Sato, and Casey Horn. Casey is the hyper-poster.

## On the page

You land on Mira's feed. Fern wall and Seed swap lead, because those are her interests. Each thread is one line. Click it to read the messages underneath. The box at the bottom says where the next message will go (`Posting to: Fern wall — change`). Enter posts. Shift+Enter adds a line. **change** picks another thread by name. **details** shows coordinates, scores, and who was held back.

The name at the top switches reader. Casey's feed opens on General, and he still only gets two lines, so the quieter voices stay visible. **Reset demo** lives under details. `cargo run -- --seed` restores the same cast.

## CLI

Three commands. With no command, Lanes serves the page.

```bash
cargo run -- feed
cargo run -- post --text "The moss tray can stay another week."
cargo run -- read "Fern wall"
```

The reader defaults to `mira`. Set `--user`, or `LANES_USER`, or a `lanes.json` next to where you run:

```json
{ "user": "mira", "file": "data/chat.phext" }
```

The phext defaults to `data/chat.phext`. `--file` and `LANES_FILE` override that. `--details` prints coordinates and scores. `--preview` on `post` only says where the message would land. `--scroll` and `--index` on `read` are the raw scroll and the table of contents.

```bash
cargo run -- feed --user casey
cargo run -- --details feed
cargo run -- post --text "The ferns on the north wall need more shade" --preview
cargo run -- read --scroll 1.2.1/1.1.1/1.1.2
cargo run -- read --index
cargo run -- --seed
cargo run -- --host 127.0.0.1 --port 7841 --file data/chat.phext
cargo test
```

`--host` defaults to `127.0.0.1` (`LANES_HOST`). `--port` defaults to `7841` (`LANES_PORT`). `--file` and `LANES_FILE` choose the phext.

`--user` accepts a reader id (`mira`) or a display name. Omit `--coord` on `post` and the same word-overlap guess the page uses picks the scroll.

HTTP, if you want it without the page:

| | |
|---|---|
| `GET /api/state?user=mira` | ranked feed, lanes, coordinate map |
| `GET /api/room?thread=1.1.1/1.1.1` | chronological thread |
| `GET /api/suggest?user=mira&text=ferns` | predicted scroll |
| `POST /api/post` | `{"user":"mira","text":"...","coord":"1.2.1/1.1.1"}` |
| `GET /api/scroll?coord=1.2.1/1.1.1/1.1.2` | raw scroll |
| `GET /api/toc` | `textmap` and `index` |
| `POST /api/reset` | rewrite the demo seed |

## Design

### Coordinates

Phext is 11-dimensional text. Column and line live inside a scroll (characters and newlines). The address written on a message is the other nine dimensions:

```text
library.shelf.series / collection.volume.book / chapter.section.scroll
LB.SF.SR             / CN.VM.BK               / CH.SN.SC
```

Canonical form uses slashes (`1.2.1/1.1.1/1.1.4`). URL form uses semicolons. Both parse.

| Arm | Role in Lanes |
|---|---|
| Library.shelf.series | Topic. `1.2.1` is Gardens, `2.1.1` is Workshops. |
| Collection.volume.book | Thread. Fern wall is `1.2.1/1.1.1`. Seed swap is `1.2.1/1.2.1`. |
| Chapter.section.scroll | One scroll. The thread card is always `…/1.1.1`. Messages are `…/1.1.2`, `…/1.1.3`, … |

Library **5** is the roster, not a room. Each reader is one scroll there: id, name, blurb, and interest coordinates.

A thread card:

```text
kind: thread
topic: Gardens
title: Fern wall
keywords: fern moss shade garden frond quiet north wall
```

A message:

```text
kind: message
author: Mira Quill
ts: 2026-10-22T18:00:00Z
---
The north-wall ferns finally unfurled.
```

Dimension breaks are stripped out of anything a person types, so a message cannot split the file. Newlines stay inside the scroll. Writes call `phext::insert`. A single scroll is read with `phext::fetch`. The contents listing is `phext::textmap`; byte offsets come from `phext::index`.

The delimiters, as libphext defines them:

| Break | Code | Between |
|---|---|---|
| Line | `0x0A` | Lines inside a scroll |
| Scroll | `0x17` | Messages in a thread |
| Section | `0x18` | Sections (unused by this layout) |
| Chapter | `0x19` | Chapters (unused by this layout) |
| Book | `0x1A` | Books, when a new thread opens on the same volume |
| Volume | `0x1C` | Fern wall and Seed swap |
| Collection | `0x1D` | Collections |
| Series | `0x1E` | Series |
| Shelf | `0x1F` | General (`1.1.1`) and Gardens (`1.2.1`) |
| Library | `0x01` | Gardens and Workshops |

Count them in a seeded file:

```bash
python3 -c "d=open('data/chat.phext','rb').read(); print({hex(b): d.count(bytes([b])) for b in (0x01,0x17,0x1c,0x1f)})"
```

You should see scroll breaks (`0x17`) between messages, a shelf break (`0x1f`) between General and the gardens, a volume break (`0x1c`) between Fern wall and Seed swap, and library breaks (`0x01`) between topics.

### Feed

A thread's distance to a reader is the weighted Manhattan distance on the six topic and thread dimensions. Scroll position does not count. The weights are library 16, shelf 8, series 4, collection 2, volume 2, book 1, so a different library cannot outrank a subscribed shelf just by being newer.

```text
score = 0.72 / (1 + distance) + 0.28 / (1 + age_hours / 18)
```

Age is the age of the newest message in the thread. Distance is the minimum over the reader's interests. No interests means every thread is distance 0 and the feed is recency.

Nearness, shown only under details: 0 subscribed, 1–3 next door, 4–12 same wing, otherwise across the library. The default feed is one line per thread, the newest message the cap allowed, and does not print coordinates or scores.

### The lane

An author with **8 or more** messages in the phext is loud. A loud author gets at most **2** lines in one reader's feed, wherever those lines would have ranked. The rest stay in the file and in the thread when you open it. The feed only says how many were held. Quieter authors are not capped, which is why Jules's plane notes and Ada's river notes survive even when Casey has been posting in those rooms.

General is marked flooded, under details, when one author wrote at least 70% of a room of 10 or more messages. Casey is 12 of 15 there. Fern wall is half Casey and is not marked; the cap still holds him to two lines.

### Prediction

There is no model. The draft is split into words, a trailing "s" is stripped (`ferns` → `fern`), and stopwords are dropped. Each existing thread is the same bag over its title, keywords, and recent messages. The thread with the most shared words wins. Ties break toward the reader's interests, then recency. If nothing overlaps, the next free book on the reader's home shelf is proposed (`1.2.1/1.1.2` for Mira, beside Fern wall). Placing into a coordinate that does not exist yet writes the thread card and the first message.

The page shows that choice as `Posting to: <title> — change`. It does not show the shared words unless details is open.

## Tests

`cargo test` covers coordinate parsing (including the semicolon form), the nine phext break codes, distance weights, the score ordering (a stale subscribed thread beats a brand-new far one), the loud-author cap, keyword prediction, a new-book suggestion, `insert` / `fetch` / `textmap` roundtrips, a scroll inserted between two existing coordinates, rejection of an injected `0x17`, and the seeded cast: Mira opens on Fern wall, Casey opens on General but still sees the quiet voices, and General is flooded while his held messages stay out of the one-line feed. It also checks that the calm feed is one line per thread plus an "N more held" note, that a thread opens by title, that reader and file defaults come from a flag, then the environment, then `lanes.json`, and that the commands are `feed`, `post`, and `read`.

## Deploy

On the phext.io server, as `ec2-user`:

```bash
curl -fsSL https://raw.githubusercontent.com/wbic16/phext-lanes/exo/deploy.sh | bash
```

The same script from a checkout of `exo`:

```bash
git clone -b exo https://github.com/wbic16/phext-lanes.git
cd phext-lanes
bash deploy.sh
```

That builds this branch, listens on `127.0.0.1:7841`, keeps the phext in `/var/lib/phext-lanes` across redeploys, and reverse-proxies `mc.phext.io` from Apache. The demo cast is written only when that data file does not exist yet. Set `CERTBOT_EMAIL` for the first certificate. `--no-tls` skips certbot. Run `bash deploy.sh` again to redeploy.

## License

MIT. Copyright (c) 2026 Will Bickford. See [LICENSE](LICENSE).
