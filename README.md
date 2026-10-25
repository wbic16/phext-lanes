# Lanes

```bash
git clone <repo-url> lanes && cd lanes && cargo run
```

Open http://127.0.0.1:7841. The first run writes `data/chat.phext` with a General thread and a short primer, **How phext works**, that explains coordinates and links. Type a message and press Enter.

`cargo run -- --seed` writes the demo cast instead (you are Mira). Starting the server never writes the demo on its own.

`cargo run -- --empty` writes the same General thread and primer without starting the server. It refuses to replace a file that already has anything in it. `--seed` is the one that wipes a file, and it puts the demo cast back. Do not pass `--seed` against a chat you mean to keep.

---

Predictive multi-chat, stored as one [phext](https://mirrorborn.us) file. The page is one feed: a line for each relevant thread, then a compose box. Each line shows the thread's title, its newest note, and how many notes it holds and how recent the last one is. Messages that would flood the feed stay in the file and show up as a quiet "N more held" note.

The cast is fictional: Mira Quill, Jules Fen, Ada North, Ren Sato, and Casey Horn. Casey is the hyper-poster.

## On the page

With the demo cast seeded, you land on Mira's feed. Fern wall and Seed swap lead, because those are her interests. Each thread is one line. Click it to read the messages underneath, numbered in order with who wrote each and when. With a thread open, the box replies to it. **Start a thread** sends just the next message to a new thread, then the box goes back to replying. The box at the bottom says where the next message will go (`Posting to: Fern wall — change`). Enter posts. Shift+Enter adds a line. **change** picks another thread by name. **details** shows coordinates, scores, and who was held back.

Each thread, and each message inside it, has a quiet **link**. The address is always the full nine-part phext coordinate. A thread is its card scroll: `/l/1.2.1/1.1.1/1.1.1` opens Fern wall. A message is its own scroll: `/p/1.2.1/1.1.1/1.1.2` opens Fern wall with that message marked. A section of ordinary text is its first scroll, `/l/1.1.1/1.1.1/2.1.1`. Shorter or looser forms redirect to the nine-part one: a six-part thread (`/l/1.2.1/1.1.1`), semicolons in place of slashes, and links from older builds (`/l/k…`, `/p/k…`, `/l/s…`). An unknown address says it doesn't go anywhere.

To link from a message, type the nine-part address. A thread card such as `1.2.1/1.1.1/1.1.1` links to that thread; any other scroll such as `1.2.1/1.1.1/1.1.2` links to that message. `/l/…` or `/p/…` in front picks the kind explicitly. Messages in an open thread alternate background shades so neighbouring scrolls are easy to tell apart.

The name at the top switches reader. Casey's feed opens on General. Other readers only see two lines of his, so the quieter voices stay visible in their feeds. **Reset demo** lives under details. `cargo run -- --seed` restores the same cast.

**fork** downloads this phext. An open thread also has a fork of just that thread. The menu switches fabric. The default fabric keeps `/l/…` and `/p/…`. Another fabric lives at `/f/<name>/…`, in `data/fabrics/<name>.phext` locally and `/var/lib/phext-lanes/fabrics/<name>.phext` on the server. **create fabric** is for [founding plot holders in Tessera](https://mirrorborn.us). Until that check is connected, the server allows it only with the admin token in `/etc/phext-lanes/admin.env`, which `deploy.sh` creates on the server and does not print. Browsing, permalinks, and fork stay open.

**Sign in** is the quiet control in the header. Posting, replying, and creating a fabric need a GitHub or Google session. Creating a fabric still needs the plot key as well. If `/etc/phext-lanes/oauth.env` has no client credentials, those writes stay closed and the page says sign-in is not set up yet.

On the server, `deploy.sh` creates that file mode `0600` and does not print the session secret. Fill in the four client lines, then `sudo systemctl restart phext-lanes` (or run `bash deploy.sh` again).

## Members

When `LANES_OWNERS` is set, the default chat is invite-only and the signed-in person is the reader. There is no reader switcher. A new member's roster scroll (library 5) starts with an empty interest list, so the feed is recency. Posting or opening a thread adds that thread to the list. The same distance ranking as the demo does the rest.

Leave `LANES_OWNERS` empty and the public demo stays as it is: Mira's feed, the reader switcher, open permalinks, open fork.

`deploy.sh` writes `/etc/phext-lanes/owners.env` mode `0600` when that file is missing, and does not replace it later. The systemd unit loads it. One line, comma-separated, each entry a `provider:id` or an email:

```text
LANES_OWNERS=github:12345,stewart@example.com
```

GitHub's id is the numeric id, not the login. An email matches when the provider sends one. Google does. GitHub does when the address on the profile is public. Owners are members the first time they sign in. They do not need an invite.

Named fabrics under `/f/<name>/` stay public to read, including their permalinks and fork. That is the incipit and demo-fabric behavior. The default fabric is the group chat: when owners are set, `/`, `/l/…`, `/p/…`, the JSON for those, and `/chat.phext` are members-only. A non-member who signs in sees a short page that asks for an invite, and does not get the text.

## Invites

On the server, as the account that runs Lanes:

```bash
lanes invite create --uses 1 --days 14 --file /var/lib/phext-lanes/chat.phext
lanes invite list --file /var/lib/phext-lanes/chat.phext
lanes invite revoke <id> --file /var/lib/phext-lanes/chat.phext
```

`create` prints the link once (`https://mc.phext.io/join/…` when `LANES_PUBLIC_ORIGIN` is set). The token is not stored. The file next to the phext, `invites.json`, keeps a hash, mode `0600`. `--uses 1` is single-use. A larger number can be used that many times. `--uses 0` does not run out. Omit `--days` and the link lasts until you revoke it.

Stewart opens the link, signs in with GitHub or Google, and is a member. The next page is a short welcome. Send a fresh link if that one was single-use and already spent.

To invite Stewart: put yourself in `LANES_OWNERS`, restart the service, run `lanes invite create --uses 1 --days 14` on the server, and send him the printed URL. Do not put the token in a log, a screenshot of the file, or the chat.

GitHub: Settings → Developer settings → OAuth Apps → New OAuth App. Homepage `https://mc.phext.io`. Authorization callback URL `https://mc.phext.io/auth/github/callback`. Put the client id and secret in `GITHUB_CLIENT_ID` and `GITHUB_CLIENT_SECRET`.

Google: Cloud Console → APIs & Services → Credentials → Create OAuth client ID → Web application. Authorized JavaScript origin `https://mc.phext.io`. Authorized redirect URI `https://mc.phext.io/auth/google/callback`. Put them in `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET`. If the consent screen is in testing, add yourself as a test user. The consent screen needs the `openid`, `email`, and `profile` scopes.

## CLI

With no command, Lanes serves the page. `feed`, `post`, and `read` are the chat. `invite` manages links.

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
cargo run -- --empty --file data/chat.phext
cargo run -- invite create --uses 1 --days 14
cargo run -- invite list
cargo run -- invite revoke <id>
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

A scroll that is not a reader, a thread card, or a message is still shown. Those scrolls are grouped by chapter and section, titled from the first line, and left read-only. A file that mixes both keeps the chat threads as they are. The feed lists section lanes forty at a time, and an open section loads forty scrolls at a time.

Dimension breaks are stripped out of anything a person types, so a message cannot split the file. Newlines stay inside the scroll. Writes call `phext::insert`. A single scroll is read with `phext::fetch`. The contents listing is `phext::textmap`; byte offsets come from `phext::index`.

The delimiters, as libphext defines them:

| Break | Code | Between |
|---|---|---|
| Line | `0x0A` | Lines inside a scroll |
| Scroll | `0x17` | Messages in a thread |
| Section | `0x18` | Sections of ordinary text |
| Chapter | `0x19` | Chapters of ordinary text |
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

An author with **8 or more** messages in the phext is loud. A loud author gets at most **2** lines in another reader's feed, wherever those lines would have ranked. Your own posts are never held back in your own feed. The rest stay in the file and in the thread when you open it. The feed only says how many were held. Quieter authors are not capped, which is why Jules's plane notes and Ada's river notes survive even when Casey has been posting in those rooms.

General is marked flooded, under details, when one author wrote at least 70% of a room of 10 or more messages. Casey is 12 of 15 there. Fern wall is half Casey and is not marked; the cap still holds him to two lines.

### Prediction

There is no model. The draft is split into words, a trailing "s" is stripped (`ferns` → `fern`), and stopwords are dropped. Each existing thread is the same bag over its title, keywords, and recent messages. The thread with the most shared words wins. Ties break toward the reader's interests, then recency. If nothing overlaps, the next free book on the reader's home shelf is proposed (`1.2.1/1.1.2` for Mira, beside Fern wall). Placing into a coordinate that does not exist yet writes the thread card and the first message.

The page shows that choice as `Posting to: <title> — change`. It does not show the shared words unless details is open.

## Tests

`cargo test` covers coordinate parsing (including the semicolon form), the nine phext break codes, distance weights, the score ordering (a stale subscribed thread beats a brand-new far one), the loud-author cap, keyword prediction, a new-book suggestion, `insert` / `fetch` / `textmap` roundtrips, a scroll inserted between two existing coordinates, rejection of an injected `0x17`, and the seeded cast: Mira opens on Fern wall, Casey opens on General but still sees the quiet voices, and General is flooded while his held messages stay out of the one-line feed. It also checks that the calm feed is one line per thread plus an "N more held" note, that a thread opens by title, that reader and file defaults come from a flag, then the environment, then `lanes.json`, and that the commands are `feed`, `post`, `read`, and `invite`. Invite tests cover create, redeem, expiry, revoke, and single-use. The token is not in the file. A non-member is refused the default fabric, including permalinks, while a named fabric stays readable. A sign-in profile becomes a roster scroll. `--empty` creates General and will not overwrite. A missing file starts with the primer, not the cast, and the primer's quoted addresses are its own. Addresses are nine-part coordinates, and six-part threads, legacy `k…`/`s…` ids, and semicolons redirect, and unknown coordinates are a 404.

## Deploy

On the Ubuntu host for mc.phext.io, as `ubuntu` or `wbic16` (the account that should own the service):

```bash
curl -fsSL https://raw.githubusercontent.com/wbic16/phext-lanes/exo/deploy.sh | bash
```

The same script from a checkout of `exo`:

```bash
git clone -b exo https://github.com/wbic16/phext-lanes.git
cd phext-lanes
bash deploy.sh
```

That builds this branch with `/snap/bin` and `~/.cargo/bin` on `PATH`, listens on `127.0.0.1:7841`, and keeps the phext in `/var/lib/phext-lanes` across redeploys. Named fabrics go in `/var/lib/phext-lanes/fabrics`. The nginx site allows uploads up to 6 MB. `LANES_ADMIN_TOKEN` is written to `/etc/phext-lanes/admin.env` (mode 0600) only when that file is missing, and the value is not printed. It writes only `/etc/nginx/sites-available/mc.phext.io`, links that file from `sites-enabled`, runs `nginx -t`, and reloads nginx. Other site files are left alone. If nginx is already failing `nginx -t`, the script stops and changes nothing. Point `mc.phext.io` at `44.248.235.76`. Certbot runs `certbot --nginx -d mc.phext.io` only when that name resolves to this machine and certbot is installed; otherwise the script prints the command. When the data file does not exist yet, the first start writes General and the phext primer. Deploy never writes the demo cast. `bash deploy.sh --empty` writes the same file during the deploy, by running `lanes --empty`. If `chat.phext` already exists, `--empty` prints a warning and leaves the file alone. It never deletes a chat. To start over you move the old file aside yourself, then run `bash deploy.sh --empty` again. `--seed` is not part of deploy. Set `CERTBOT_EMAIL` for the first certificate. `--no-tls` skips certbot. `--user` selects the systemd account (the default is whoever runs the script). Run `bash deploy.sh` again to redeploy.

Owners live in `/etc/phext-lanes/owners.env`. Invites land in `/var/lib/phext-lanes/invites.json` the first time you run `lanes invite create`. Neither file is in git. Fill in the OAuth client lines in `/etc/phext-lanes/oauth.env` before anyone can sign in. The steps are under Sign in, above.

## License

MIT. Copyright (c) 2026 Will Bickford. See [LICENSE](LICENSE).
