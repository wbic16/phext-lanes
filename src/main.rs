use std::path::PathBuf;
use std::process::ExitCode;

use chrono::Utc;
use clap::{Parser, Subcommand};

use lanes::calm;
use lanes::config::Settings;
use lanes::coord::{format_coord, parse_target, thread_key, Target};
use lanes::rank;
use lanes::store::{Post, Store, World};
use lanes::Error;

#[derive(Parser, Debug)]
#[command(
    name = "lanes",
    version,
    about = "A short feed over one phext file",
    disable_help_subcommand = true
)]
struct Cli {
    /// Phext file. Defaults to data/chat.phext, or LANES_FILE, or lanes.json.
    #[arg(long, global = true)]
    file: Option<PathBuf>,

    /// Who is reading and posting. Defaults to mira, or LANES_USER, or lanes.json.
    #[arg(long, global = true)]
    user: Option<String>,

    /// Listen address. Defaults to 127.0.0.1, or LANES_HOST.
    #[arg(long, global = true)]
    host: Option<String>,

    /// Listen port when no command is given. Defaults to 7841, or LANES_PORT.
    #[arg(long, global = true)]
    port: Option<u16>,

    /// Replace the phext with the demo cast, then run the command.
    /// This wipes an existing chat. `--empty` will not.
    #[arg(long, global = true)]
    seed: bool,

    /// Create a new chat with General and the phext primer, and no demo cast.
    /// Refuses to replace a file that already has content.
    #[arg(long, global = true)]
    empty: bool,

    /// Show coordinates, scores, and what the feed held back.
    #[arg(long, global = true)]
    details: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// One line for each relevant thread.
    Feed,
    /// Write a message. The thread is chosen from the words unless --coord is set.
    Post {
        #[arg(long)]
        text: String,
        /// Thread `1.2.1/1.1.1` or scroll `1.2.1/1.1.1/1.1.4`.
        #[arg(long)]
        coord: Option<String>,
        /// Print where this would land, and do not write.
        #[arg(long)]
        preview: bool,
    },
    /// Read a thread. A title or coordinate; otherwise the top of the feed.
    Read {
        query: Option<String>,
        /// Print one raw scroll.
        #[arg(long)]
        scroll: Option<String>,
        /// Print the thread index and the phext table of contents.
        #[arg(long)]
        index: bool,
    },
    /// Create, list, or revoke invite links for the private chat.
    Invite {
        #[command(subcommand)]
        action: InviteAction,
    },
}

#[derive(Subcommand, Debug)]
enum InviteAction {
    /// Print a new invite link once. The token is not stored.
    Create {
        /// How many joins the link allows. 1 is single-use. 0 does not run out.
        #[arg(long, default_value_t = 1)]
        uses: u32,
        /// Drop the link after this many days. Omit it to keep the link until you revoke it.
        #[arg(long)]
        days: Option<u32>,
    },
    /// Show invite ids, remaining uses, and expiry. Tokens are not shown.
    List,
    /// Stop an invite from being used.
    Revoke { id: String },
}

#[cfg(unix)]
extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}

#[tokio::main]
async fn main() -> ExitCode {
    // `println` panics when a pipe closes. Restore the default SIGPIPE so
    // `lanes feed | head` ends quietly.
    #[cfg(unix)]
    unsafe {
        signal(13, 0);
    }
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("lanes: {err}");
            ExitCode::from(1)
        }
    }
}

async fn run() -> Result<(), Error> {
    let cli = Cli::parse();
    let settings = lanes::config::load(cli.user.as_deref(), cli.file.as_deref())?;
    let listen = lanes::config::load_listen(cli.host.as_deref(), cli.port)?;
    check_flags(cli.seed, cli.empty)?;
    if cli.seed {
        eprintln!(
            "warning: --seed replaces {} with the demo cast. This wipes whatever is already in that file.",
            settings.file.display()
        );
        let mut store = Store::open(&settings.file)?;
        store.reseed(Utc::now())?;
        if cli.command.is_none() {
            println!("seeded {}", store.path().display());
            return Ok(());
        }
    }
    if cli.empty {
        Store::init_empty(&settings.file)?;
        println!(
            "created {} with General and the phext primer, and no demo cast.",
            settings.file.display()
        );
        println!("--empty does not replace a chat that already exists.");
        if cli.command.is_none() {
            return Ok(());
        }
    }
    match cli.command {
        None => {
            let limits = lanes::config::load_limits()?;
            lanes::server::serve(settings.file, &listen.host, listen.port, limits).await
        }
        Some(Command::Feed) => {
            let store = Store::open(&settings.file)?;
            print_feed(&store, &settings, cli.details)?;
            Ok(())
        }
        Some(Command::Post {
            text,
            coord,
            preview,
        }) => {
            let mut store = Store::open(&settings.file)?;
            post(
                &mut store,
                &settings,
                &text,
                coord.as_deref(),
                preview,
                cli.details,
            )
        }
        Some(Command::Invite { action }) => invite_cmd(&settings, action),
        Some(Command::Read {
            query,
            scroll,
            index,
        }) => {
            let store = Store::open(&settings.file)?;
            read(
                &store,
                &settings,
                query.as_deref(),
                scroll.as_deref(),
                index,
                cli.details,
            )
        }
    }
}

fn check_flags(seed: bool, empty: bool) -> Result<(), Error> {
    if seed && empty {
        Err(Error::Bad(
            "use either --seed or --empty. --seed replaces the file with the demo cast and wipes it. --empty only creates a missing chat and never overwrites one."
                .into(),
        ))
    } else {
        Ok(())
    }
}

fn invite_cmd(settings: &Settings, action: InviteAction) -> Result<(), Error> {
    let path = lanes::invite::resolve(&settings.file);
    match action {
        InviteAction::Create { uses, days } => {
            let exp = match days {
                None => None,
                Some(0) => return Err(Error::Bad(
                    "--days must be at least 1, or omit it and the link lasts until you revoke it"
                        .into(),
                )),
                Some(days) if days > 3650 => {
                    return Err(Error::Bad("--days must be 3650 or fewer".into()))
                }
                Some(days) => Some(Utc::now() + chrono::Duration::days(i64::from(days))),
            };
            let issued = lanes::invite::create(&path, uses, exp, Utc::now())?;
            let life = if uses == 0 {
                "unlimited".to_string()
            } else if uses == 1 {
                "single-use".to_string()
            } else {
                format!("{uses} uses")
            };
            let until = match issued.exp {
                Some(when) => format!("expires {}", when.format("%Y-%m-%d")),
                None => "until revoked".to_string(),
            };
            println!("{id}  {life}  {until}", id = issued.id);
            println!("This token is shown once and is not stored.");
            println!("{}", invite_url(&issued.token));
            Ok(())
        }
        InviteAction::List => {
            for row in lanes::invite::list(&path)? {
                let life = if row.revoked {
                    "revoked".to_string()
                } else if row.uses == 0 {
                    "unlimited".to_string()
                } else if row.uses == 1 {
                    format!("single-use  {} left", row.remaining)
                } else {
                    format!("{} uses  {} left", row.uses, row.remaining)
                };
                let until = match row.exp {
                    Some(when) => format!("expires {}", when.format("%Y-%m-%d")),
                    None => "until revoked".to_string(),
                };
                println!("{}  {life}  {until}", row.id);
            }
            Ok(())
        }
        InviteAction::Revoke { id } => {
            lanes::invite::revoke(&path, &id)?;
            println!("revoked {id}");
            Ok(())
        }
    }
}

fn invite_url(token: &str) -> String {
    let origin = std::env::var("LANES_PUBLIC_ORIGIN").unwrap_or_default();
    let origin = origin.trim().trim_end_matches('/').to_string();
    if origin.is_empty() {
        format!("/join/{token}")
    } else {
        format!("{origin}/join/{token}")
    }
}

fn require_user<'a>(world: &'a World, query: &str) -> Result<&'a lanes::model::User, Error> {
    world.find_user(query).ok_or_else(|| {
        let known = world
            .users
            .iter()
            .map(|user| user.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        Error::Bad(format!("unknown reader {query}. known: {known}"))
    })
}

/// The server's limits from the environment and `lanes.json`, with this
/// phext's own overrides on top.
fn limits_for(store: &Store) -> Result<lanes::config::Limits, Error> {
    let base = lanes::config::load_limits()?;
    Ok(store.world().settings.limits.apply(base))
}

fn print_feed(store: &Store, settings: &Settings, details: bool) -> Result<(), Error> {
    let limits = limits_for(store)?;
    let world = store.world();
    let interests;
    let interest_ref: &[libphext::phext::Coordinate] = if world.users.is_empty() {
        &[]
    } else {
        interests = require_user(world, &settings.user)?.interests.clone();
        &interests
    };
    let feed = rank::build_feed(&world.threads, interest_ref, Utc::now(), &limits);
    if feed.threads.is_empty() {
        print_quiet(world);
        return Ok(());
    }
    println!("{}", calm::format_feed(&feed, details));
    Ok(())
}

fn post(
    store: &mut Store,
    settings: &Settings,
    text: &str,
    coord: Option<&str>,
    preview: bool,
    details: bool,
) -> Result<(), Error> {
    let prepared = {
        let world = store.world();
        let reader = require_user(world, &settings.user)?;
        let interests = reader.interests.clone();
        let author = reader.name.clone();
        if preview {
            print_destination(world, &interests, text, coord, details)?;
            None
        } else {
            let target = match coord {
                Some(coord) => parse_target(coord).map_err(Error::from)?,
                None => {
                    let suggestion = rank::suggest(&world.threads, &interests, text, Utc::now());
                    Target::Scroll(suggestion.coord)
                }
            };
            Some((author, target))
        }
    };
    let Some((author, target)) = prepared else {
        return Ok(());
    };
    let placed = store.place(Post {
        author,
        who: String::new(),
        text: text.to_string(),
        target,
        now: Utc::now(),
    })?;
    println!("Posted to {}.", placed.title);
    if details {
        println!("{}", format_coord(placed.coord));
    }
    Ok(())
}

fn print_quiet(world: &World) {
    let quiet: Vec<&str> = world
        .threads
        .iter()
        .filter(|thread| !thread.plain && thread.messages.is_empty())
        .map(|thread| thread.title.as_str())
        .collect();
    if quiet.is_empty() {
        println!("Nothing here yet.");
    } else {
        for title in quiet {
            println!("{title}");
        }
    }
}

fn print_destination(
    world: &World,
    interests: &[libphext::phext::Coordinate],
    text: &str,
    coord: Option<&str>,
    details: bool,
) -> Result<(), Error> {
    if let Some(coord) = coord {
        let target = parse_target(coord).map_err(Error::from)?;
        let key = match target {
            Target::Thread(coord) | Target::Scroll(coord) => thread_key(coord),
        };
        match world.thread(&key) {
            Some(thread) => println!("Posting to: {}", thread.title),
            None => println!("Posting to: a new thread"),
        }
        if details {
            println!("{key}");
        }
        return Ok(());
    }
    let suggestion = rank::suggest(&world.threads, interests, text, Utc::now());
    println!("Posting to: {}", suggestion.title);
    if details {
        println!("{}", format_coord(suggestion.coord));
        println!("{}", suggestion.reason);
    }
    Ok(())
}

fn read(
    store: &Store,
    settings: &Settings,
    query: Option<&str>,
    scroll: Option<&str>,
    index: bool,
    details: bool,
) -> Result<(), Error> {
    if index && scroll.is_some() {
        return Err(Error::Bad("use either --index or --scroll".into()));
    }
    if index {
        print_index(store, details);
        return Ok(());
    }
    if let Some(scroll) = scroll {
        let Target::Scroll(coord) = parse_target(scroll).map_err(Error::from)? else {
            return Err(Error::Bad(
                "scroll needs a full coordinate, like 1.2.1/1.1.1/1.1.2".into(),
            ));
        };
        let raw = store.select(coord);
        if raw.is_empty() {
            println!("(empty scroll)");
        } else {
            println!("{raw}");
        }
        return Ok(());
    }
    let limits = limits_for(store)?;
    let world = store.world();
    let thread = if let Some(query) = query {
        world.find_thread(query).map_err(Error::from)?
    } else {
        let user = require_user(world, &settings.user)?;
        let feed = rank::build_feed(&world.threads, &user.interests, Utc::now(), &limits);
        let top = feed
            .threads
            .first()
            .ok_or_else(|| Error::Bad("nothing to read".into()))?;
        world
            .thread(&thread_key(top.coord))
            .ok_or_else(|| Error::Bad("nothing to read".into()))?
    };
    let text = calm::format_thread(thread, details, &limits);
    if text.is_empty() {
        println!("Nothing to read.");
    } else {
        println!("{text}");
    }
    Ok(())
}

fn print_index(store: &Store, details: bool) {
    let world = store.world();
    for thread in &world.threads {
        if details {
            println!(
                "{}  {} / {}  ({} messages)",
                thread_key(thread.coord),
                thread.topic,
                thread.title,
                thread.messages.len()
            );
        } else {
            println!("{}", thread.title);
        }
    }
    if details {
        println!("\n--- phext::textmap ---");
        println!("{}", store.toc());
        println!("--- phext::index ---");
        println!("{}", store.byte_index());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_verbs_are_feed_post_and_read() {
        let names: Vec<_> = Cli::command()
            .get_subcommands()
            .map(|command| command.get_name().to_string())
            .collect();
        assert!(names.contains(&"feed".to_string()));
        assert!(names.contains(&"post".to_string()));
        assert!(names.contains(&"read".to_string()));
        for gone in ["suggest", "room", "toc", "select", "seed", "serve"] {
            assert!(!names.iter().any(|name| name == gone), "{names:?}");
        }
        assert!(names.contains(&"invite".to_string()));
    }

    #[test]
    fn empty_and_invite_parse_and_seed_will_not_combine_with_empty() {
        let empty = Cli::try_parse_from(["lanes", "--empty"]).unwrap();
        assert!(empty.empty);
        assert!(!empty.seed);
        let invite =
            Cli::try_parse_from(["lanes", "invite", "create", "--uses", "4", "--days", "14"])
                .unwrap();
        match invite.command {
            Some(Command::Invite {
                action: InviteAction::Create { uses, days },
            }) => {
                assert_eq!(uses, 4);
                assert_eq!(days, Some(14));
            }
            other => panic!("expected invite create, got {other:?}"),
        }
        assert!(Cli::try_parse_from(["lanes", "invite", "list"]).is_ok());
        assert!(Cli::try_parse_from(["lanes", "invite", "revoke", "abc"]).is_ok());
        let err = check_flags(true, true).unwrap_err();
        assert!(err.to_string().contains("never overwrites"), "{err}");
        assert!(check_flags(true, false).is_ok());
        assert!(check_flags(false, true).is_ok());
    }

    #[test]
    fn retired_verbs_do_not_parse() {
        for args in [
            vec!["lanes", "suggest", "--text", "ferns"],
            vec!["lanes", "room", "--coord", "1.1.1/1.1.1"],
            vec!["lanes", "toc"],
            vec!["lanes", "select", "--coord", "1.2.1/1.1.1/1.1.2"],
            vec!["lanes", "seed"],
            vec!["lanes", "serve"],
        ] {
            assert!(Cli::try_parse_from(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn the_old_commands_are_flags() {
        let seeded = Cli::try_parse_from(["lanes", "--seed"]).unwrap();
        assert!(seeded.seed);
        assert!(seeded.command.is_none());

        let feed = Cli::try_parse_from(["lanes", "feed", "--user", "casey", "--details"]).unwrap();
        assert_eq!(feed.user.as_deref(), Some("casey"));
        assert!(feed.details);
        assert!(matches!(feed.command, Some(Command::Feed)));

        let preview = Cli::try_parse_from([
            "lanes",
            "post",
            "--text",
            "The ferns need shade",
            "--preview",
        ])
        .unwrap();
        match preview.command {
            Some(Command::Post {
                text,
                preview,
                coord,
            }) => {
                assert_eq!(text, "The ferns need shade");
                assert!(preview);
                assert!(coord.is_none());
            }
            other => panic!("expected post, got {other:?}"),
        }

        let scroll =
            Cli::try_parse_from(["lanes", "read", "--scroll", "1.2.1/1.1.1/1.1.2"]).unwrap();
        match scroll.command {
            Some(Command::Read {
                scroll: Some(coord),
                index,
                ..
            }) => {
                assert_eq!(coord, "1.2.1/1.1.1/1.1.2");
                assert!(!index);
            }
            other => panic!("expected read --scroll, got {other:?}"),
        }

        let index = Cli::try_parse_from(["lanes", "read", "--index"]).unwrap();
        assert!(matches!(
            index.command,
            Some(Command::Read { index: true, .. })
        ));

        let hosted =
            Cli::try_parse_from(["lanes", "--host", "127.0.0.1", "--port", "7841"]).unwrap();
        assert_eq!(hosted.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(hosted.port, Some(7841));
        assert!(hosted.command.is_none());

        let titled = Cli::try_parse_from(["lanes", "read", "Fern wall"]).unwrap();
        match titled.command {
            Some(Command::Read {
                query: Some(title), ..
            }) => assert_eq!(title, "Fern wall"),
            other => panic!("expected a title, got {other:?}"),
        }
    }
}
