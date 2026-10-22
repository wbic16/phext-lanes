use std::path::PathBuf;
use std::process::ExitCode;

use chrono::Utc;
use clap::{Parser, Subcommand};

use lanes::coord::{format_coord, parse_target, thread_key, Target};
use lanes::rank;
use lanes::store::{Post, Store};
use lanes::Error;

#[derive(Parser, Debug)]
#[command(
    name = "lanes",
    version,
    about = "Predictive multi-chat stored as one phext file"
)]
struct Cli {
    /// Path to the chat phext. Created and seeded if it is missing.
    #[arg(long, global = true, default_value = "data/chat.phext")]
    file: PathBuf,

    /// Listen port for the reading room.
    #[arg(long, global = true, default_value_t = 7841)]
    port: u16,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Serve the reading room. This is also the default.
    Serve,
    /// Replace the phext with the demo cast.
    Seed,
    /// Place a message. Omit --coord to predict one from --user's interests.
    Post {
        /// Display name or reader id (`mira`, `Casey Horn`).
        #[arg(long)]
        author: String,
        #[arg(long)]
        text: String,
        /// Thread `1.2.1/1.1.1` or scroll `1.2.1/1.1.1/1.1.4`.
        #[arg(long)]
        coord: Option<String>,
        /// Whose interests guide prediction when --coord is omitted.
        #[arg(long, default_value = "mira")]
        user: String,
    },
    /// Ranked feed for a reader.
    Feed {
        #[arg(long, default_value = "mira")]
        user: String,
    },
    /// Chronological room. Pass a thread or any scroll in it.
    Room {
        #[arg(long)]
        coord: String,
    },
    /// Predict a coordinate for a message.
    Suggest {
        #[arg(long, default_value = "mira")]
        user: String,
        #[arg(long)]
        text: String,
    },
    /// Thread index, phext::textmap, and phext::index byte offsets.
    Toc,
    /// Select one scroll with phext::fetch.
    Select {
        #[arg(long)]
        coord: String,
    },
}

#[cfg(unix)]
extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}

#[tokio::main]
async fn main() -> ExitCode {
    // `println` panics when a pipe closes. Restore the default SIGPIPE so
    // `lanes toc | head` ends quietly.
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
    match cli.command {
        None | Some(Command::Serve) => lanes::server::serve(cli.file, cli.port).await,
        Some(Command::Seed) => {
            let mut store = Store::open(&cli.file)?;
            store.reseed(Utc::now())?;
            println!("seeded {}", store.path().display());
            Ok(())
        }
        Some(Command::Post {
            author,
            text,
            coord,
            user,
        }) => {
            let mut store = Store::open(&cli.file)?;
            let world = store.world();
            let reader = require_user(&world, &user)?;
            let author_name = world
                .find_user(&author)
                .map(|found| found.name.clone())
                .unwrap_or(author);
            let target = if let Some(coord) = coord {
                parse_target(&coord).map_err(Error::from)?
            } else {
                let suggestion =
                    rank::suggest(&world.threads, &reader.interests, &text, Utc::now());
                println!("{}", suggestion.reason);
                Target::Scroll(suggestion.coord)
            };
            let placed = store.place(Post {
                author: author_name,
                text,
                target,
                now: Utc::now(),
            })?;
            println!(
                "placed {}  {} / {}  {}",
                format_coord(placed.coord),
                placed.topic,
                placed.title,
                if placed.new_thread {
                    "new thread"
                } else {
                    "existing thread"
                }
            );
            Ok(())
        }
        Some(Command::Feed { user }) => {
            let store = Store::open(&cli.file)?;
            print_feed(&store, &user)?;
            Ok(())
        }
        Some(Command::Room { coord }) => {
            let store = Store::open(&cli.file)?;
            print_room(&store, &coord)?;
            Ok(())
        }
        Some(Command::Suggest { user, text }) => {
            let store = Store::open(&cli.file)?;
            let world = store.world();
            let reader = require_user(&world, &user)?;
            let suggestion = rank::suggest(&world.threads, &reader.interests, &text, Utc::now());
            println!("{}", format_coord(suggestion.coord));
            println!(
                "{} / {}{}",
                suggestion.topic,
                suggestion.title,
                if suggestion.new_thread { "  (new)" } else { "" }
            );
            println!("{}", suggestion.reason);
            Ok(())
        }
        Some(Command::Toc) => {
            let store = Store::open(&cli.file)?;
            print_toc(&store);
            Ok(())
        }
        Some(Command::Select { coord }) => {
            let Target::Scroll(coord) = parse_target(&coord).map_err(Error::from)? else {
                return Err(Error::Bad(
                    "select needs a full scroll coordinate, like 1.2.1/1.1.1/1.1.2".into(),
                ));
            };
            let store = Store::open(&cli.file)?;
            let raw = store.select(coord);
            if raw.is_empty() {
                println!("(empty scroll)");
            } else {
                println!("{raw}");
            }
            Ok(())
        }
    }
}

fn require_user<'a>(
    world: &'a lanes::store::World,
    query: &str,
) -> Result<&'a lanes::model::User, Error> {
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

fn print_feed(store: &Store, query: &str) -> Result<(), Error> {
    let world = store.world();
    let user = require_user(&world, query)?;
    let now = Utc::now();
    let feed = rank::build_feed(
        &world.threads,
        &user.interests,
        now,
        rank::AUTHOR_CAP,
        rank::MAX_THREADS,
    );
    println!("{}  ·  {}", user.name, user.blurb);
    let interests = user
        .interests
        .iter()
        .map(|coord| thread_key(*coord))
        .collect::<Vec<_>>()
        .join(", ");
    println!("interests  {interests}");
    println!(
        "cap        {} messages from any one author\n",
        rank::AUTHOR_CAP
    );
    for thread in &feed.threads {
        println!(
            "{}  {}  {}  distance {}",
            thread.title,
            thread_key(thread.coord),
            thread.nearness,
            thread.distance
        );
        for message in &thread.admitted {
            println!("  {:<16} {}", message.author, first_line(&message.text));
        }
        if !thread.held.is_empty() {
            let mut authors: Vec<&str> = thread
                .held
                .iter()
                .map(|message| message.author.as_str())
                .collect();
            authors.sort();
            authors.dedup();
            for author in authors {
                let count = thread
                    .held
                    .iter()
                    .filter(|message| message.author == author)
                    .count();
                println!("  lane  {author} × {count} held");
            }
        }
        println!();
    }
    for lane in &feed.lanes {
        println!(
            "side lane  {} × {} held, {} shown",
            lane.author, lane.held, lane.shown
        );
    }
    Ok(())
}

fn print_room(store: &Store, coord: &str) -> Result<(), Error> {
    let target = parse_target(coord).map_err(Error::from)?;
    let key = match target {
        Target::Thread(coord) | Target::Scroll(coord) => thread_key(coord),
    };
    let world = store.world();
    let thread = world
        .thread(&key)
        .ok_or_else(|| Error::Bad(format!("no thread at {key}")))?;
    let (author, count, share) = rank::majority(&thread.messages);
    println!("{} / {}", thread.topic, thread.title);
    println!("{key}  ·  {} messages", thread.messages.len());
    if let Some(author) = author {
        println!(
            "{author} wrote {count} ({:.0}%){}",
            share * 100.0,
            if rank::is_flooded(thread.messages.len(), share) {
                "  ·  flooded"
            } else {
                ""
            }
        );
    }
    println!();
    for message in &thread.messages {
        println!("{}  {}", message.author, format_coord(message.coord));
        println!("  {}", first_line(&message.text));
    }
    Ok(())
}

fn print_toc(store: &Store) {
    let world = store.world();
    println!("threads");
    for thread in &world.threads {
        println!(
            "  {}  {} / {}  ({} messages)",
            thread_key(thread.coord),
            thread.topic,
            thread.title,
            thread.messages.len()
        );
    }
    println!("\n--- phext::textmap ---");
    println!("{}", store.toc());
    println!("--- phext::index ---");
    println!("{}", store.byte_index());
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").to_string()
}
