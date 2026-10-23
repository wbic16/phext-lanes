//! Small axum reading room. One HTML page, JSON for the feed.

use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::coord::{format_coord, parse_coord, parse_target, thread_key, topic_key, Target};
use crate::error::Error;
use crate::model::{preview, Message};
use crate::rank::{self, Feed, MapEntry};
use crate::store::{Post, Store};

const INDEX: &str = include_str!("../static/index.html");

#[derive(Clone)]
pub struct AppState {
    store: Arc<Mutex<Store>>,
}

/// `host:port`, with brackets when `host` is an IPv6 address.
pub fn listen_addr(host: &str, port: u16) -> String {
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub async fn serve(path: std::path::PathBuf, host: &str, port: u16) -> Result<(), Error> {
    let host = host.trim();
    if host.is_empty() {
        return Err(Error::Bad("listen host is empty".into()));
    }
    let store = Store::open(path)?;
    let label = store.path().display().to_string();
    let state = AppState {
        store: Arc::new(Mutex::new(store)),
    };
    let app = router(state);
    let addr = listen_addr(host, port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|err| Error::Bad(format!("could not listen on {addr}: {err}")))?;
    println!("listening  {addr}");
    let browse = if host == "0.0.0.0" || host == "::" {
        format!("http://127.0.0.1:{port}")
    } else {
        format!("http://{addr}")
    };
    println!("Lanes   {browse}");
    println!("phext   {label}");
    axum::serve(listener, app)
        .await
        .map_err(|err| Error::Bad(format!("server stopped: {err}")))?;
    Ok(())
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/state", get(state_view))
        .route("/api/room", get(room))
        .route("/api/suggest", get(suggest))
        .route("/api/toc", get(toc))
        .route("/api/scroll", get(scroll))
        .route("/api/post", post(post_message))
        .route("/api/reset", post(reset))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true}))
}

#[derive(Deserialize)]
struct UserQuery {
    user: Option<String>,
}

async fn state_view(
    State(app): State<AppState>,
    Query(query): Query<UserQuery>,
) -> Result<Json<StateDto>, AppError> {
    let store = lock(&app)?;
    let dto = snapshot(&store, query.user.as_deref())?;
    Ok(Json(dto))
}

#[derive(Deserialize)]
struct RoomQuery {
    thread: String,
}

async fn room(
    State(app): State<AppState>,
    Query(query): Query<RoomQuery>,
) -> Result<Json<RoomDto>, AppError> {
    let store = lock(&app)?;
    let key = thread_key_from(&query.thread)?;
    let world = store.world();
    let thread = world
        .thread(&key)
        .ok_or_else(|| AppError::bad(format!("no thread at {key}")))?;
    Ok(Json(room_dto(thread)))
}

#[derive(Deserialize)]
struct SuggestQuery {
    user: Option<String>,
    text: Option<String>,
}

async fn suggest(
    State(app): State<AppState>,
    Query(query): Query<SuggestQuery>,
) -> Result<Json<SuggestDto>, AppError> {
    let store = lock(&app)?;
    let world = store.world();
    let user = resolve_user(&world, query.user.as_deref())?;
    let suggestion = rank::suggest(
        &world.threads,
        &user.interests,
        query.text.as_deref().unwrap_or(""),
        Utc::now(),
    );
    Ok(Json(SuggestDto {
        coord: format_coord(suggestion.coord),
        thread: thread_key(suggestion.thread),
        title: suggestion.title,
        topic: suggestion.topic,
        new_thread: suggestion.new_thread,
        overlap: suggestion.overlap,
        reason: suggestion.reason,
    }))
}

async fn toc(State(app): State<AppState>) -> Result<Json<TocDto>, AppError> {
    let store = lock(&app)?;
    let world = store.world();
    let threads = world
        .threads
        .iter()
        .map(|thread| TocThread {
            thread: thread_key(thread.coord),
            topic: thread.topic.clone(),
            title: thread.title.clone(),
            messages: thread.messages.len(),
        })
        .collect();
    Ok(Json(TocDto {
        textmap: store.toc(),
        index: store.byte_index(),
        threads,
    }))
}

#[derive(Deserialize)]
struct ScrollQuery {
    coord: String,
}

async fn scroll(
    State(app): State<AppState>,
    Query(query): Query<ScrollQuery>,
) -> Result<Response, AppError> {
    let coord = parse_coord(&query.coord).map_err(AppError::bad)?;
    let store = lock(&app)?;
    let raw = store.select(coord);
    let body = if raw.is_empty() {
        "(empty scroll)\n".to_string()
    } else {
        raw
    };
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response())
}

#[derive(Deserialize)]
struct PostBody {
    user: String,
    text: String,
    coord: Option<String>,
}

async fn post_message(
    State(app): State<AppState>,
    Json(body): Json<PostBody>,
) -> Result<Json<PostOut>, AppError> {
    let mut store = lock(&app)?;
    let world = store.world();
    let user = resolve_user(&world, Some(&body.user))?.clone();
    let target = if let Some(coord) = body
        .coord
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        parse_target(coord).map_err(AppError::bad)?
    } else {
        let suggestion = rank::suggest(&world.threads, &user.interests, &body.text, Utc::now());
        Target::Scroll(suggestion.coord)
    };
    let placed = store
        .place(Post {
            author: user.name,
            text: body.text,
            target,
            now: Utc::now(),
        })
        .map_err(AppError::from)?;
    Ok(Json(PostOut {
        ok: true,
        coord: format_coord(placed.coord),
        thread: thread_key(placed.thread),
        title: placed.title,
        topic: placed.topic,
        new_thread: placed.new_thread,
    }))
}

async fn reset(State(app): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let mut store = lock(&app)?;
    store.reseed(Utc::now()).map_err(AppError::from)?;
    Ok(Json(serde_json::json!({"ok": true})))
}

fn lock(app: &AppState) -> Result<std::sync::MutexGuard<'_, Store>, AppError> {
    app.store
        .lock()
        .map_err(|_| AppError::server("store lock poisoned"))
}

fn thread_key_from(input: &str) -> Result<String, AppError> {
    let target = parse_target(input).map_err(AppError::bad)?;
    let coord = match target {
        Target::Thread(coord) | Target::Scroll(coord) => coord,
    };
    Ok(thread_key(coord))
}

fn resolve_user<'a>(
    world: &'a crate::store::World,
    query: Option<&str>,
) -> Result<&'a crate::model::User, AppError> {
    let query = query.unwrap_or("mira");
    world.find_user(query).ok_or_else(|| {
        let known = world
            .users
            .iter()
            .map(|user| user.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        AppError::status(
            StatusCode::NOT_FOUND,
            format!("unknown reader {query}. known: {known}"),
        )
    })
}

fn snapshot(store: &Store, user_query: Option<&str>) -> Result<StateDto, AppError> {
    let world = store.world();
    let user = resolve_user(&world, user_query)?;
    let now = Utc::now();
    let feed = rank::build_feed(
        &world.threads,
        &user.interests,
        now,
        rank::AUTHOR_CAP,
        rank::MAX_THREADS,
    );
    let map = rank::map_of(&world.threads, &user.interests);
    Ok(StateDto {
        file: store.path().display().to_string(),
        cap: rank::AUTHOR_CAP,
        loud_at: rank::LOUD_AT,
        viewer: viewer_dto(&world, user),
        users: world
            .users
            .iter()
            .map(|user| UserDto {
                id: user.id.clone(),
                name: user.name.clone(),
                blurb: user.blurb.clone(),
            })
            .collect(),
        feed: feed_dto(&feed),
        lanes: feed
            .lanes
            .iter()
            .map(|lane| LaneDto {
                author: lane.author.clone(),
                held: lane.held,
                shown: lane.shown,
                threads: lane.threads.clone(),
            })
            .collect(),
        map: map.into_iter().map(map_dto).collect(),
    })
}

fn viewer_dto(world: &crate::store::World, user: &crate::model::User) -> ViewerDto {
    ViewerDto {
        id: user.id.clone(),
        name: user.name.clone(),
        blurb: user.blurb.clone(),
        interests: user
            .interests
            .iter()
            .map(|coord| {
                let label = world
                    .thread(&thread_key(*coord))
                    .map(|thread| format!("{} · {}", thread.topic, thread.title))
                    .unwrap_or_else(|| thread_key(*coord));
                InterestDto {
                    coord: format_coord(*coord),
                    thread: thread_key(*coord),
                    label,
                }
            })
            .collect(),
    }
}

fn feed_dto(feed: &Feed) -> Vec<FeedThreadDto> {
    feed.threads
        .iter()
        .map(|thread| FeedThreadDto {
            thread: thread_key(thread.coord),
            topic: thread.topic.clone(),
            topic_coord: topic_key(thread.coord),
            title: thread.title.clone(),
            distance: thread.distance,
            score: thread.score,
            nearness: thread.nearness.to_string(),
            messages: thread.admitted.iter().map(msg_dto).collect(),
            held: thread.held.iter().map(msg_dto).collect(),
        })
        .collect()
}

fn map_dto(entry: MapEntry) -> MapDto {
    MapDto {
        thread: thread_key(entry.coord),
        topic: entry.topic,
        topic_coord: topic_key(entry.coord),
        title: entry.title,
        distance: entry.distance,
        nearness: rank::nearness(entry.distance).to_string(),
        messages: entry.message_count,
        latest_author: entry.latest_author,
        latest_ts: entry.latest_ts.map(stamp),
        preview: entry.preview,
        majority_author: entry.majority_author,
        majority_count: entry.majority_count,
        majority_share: entry.majority_share,
        flooded: entry.flooded,
    }
}

fn room_dto(thread: &crate::model::Thread) -> RoomDto {
    let (majority_author, majority_count, majority_share) = rank::majority(&thread.messages);
    RoomDto {
        thread: thread_key(thread.coord),
        topic: thread.topic.clone(),
        topic_coord: topic_key(thread.coord),
        title: thread.title.clone(),
        messages: thread.messages.iter().map(msg_dto).collect(),
        message_count: thread.messages.len(),
        majority_author,
        majority_count,
        majority_share,
        flooded: rank::is_flooded(thread.messages.len(), majority_share),
    }
}

fn msg_dto(message: &Message) -> MsgDto {
    MsgDto {
        coord: format_coord(message.coord),
        author: message.author.clone(),
        ts: stamp(message.ts),
        text: message.text.clone(),
        preview: preview(&message.text, 160),
    }
}

fn stamp(ts: chrono::DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Secs, true)
}

struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    fn server(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
    fn status(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl From<Error> for AppError {
    fn from(err: Error) -> Self {
        match err {
            Error::Bad(message) => AppError::bad(message),
            Error::Io(err) => AppError::server(err.to_string()),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({"error": self.message}));
        (self.status, body).into_response()
    }
}

#[derive(Serialize)]
struct StateDto {
    file: String,
    cap: usize,
    loud_at: usize,
    viewer: ViewerDto,
    users: Vec<UserDto>,
    feed: Vec<FeedThreadDto>,
    lanes: Vec<LaneDto>,
    map: Vec<MapDto>,
}

#[derive(Serialize)]
struct ViewerDto {
    id: String,
    name: String,
    blurb: String,
    interests: Vec<InterestDto>,
}

#[derive(Serialize)]
struct InterestDto {
    coord: String,
    thread: String,
    label: String,
}

#[derive(Serialize)]
struct UserDto {
    id: String,
    name: String,
    blurb: String,
}

#[derive(Serialize)]
struct FeedThreadDto {
    thread: String,
    topic: String,
    topic_coord: String,
    title: String,
    distance: u64,
    score: f64,
    nearness: String,
    messages: Vec<MsgDto>,
    held: Vec<MsgDto>,
}

#[derive(Serialize)]
struct LaneDto {
    author: String,
    held: usize,
    shown: usize,
    threads: Vec<String>,
}

#[derive(Serialize)]
struct MapDto {
    thread: String,
    topic: String,
    topic_coord: String,
    title: String,
    distance: u64,
    nearness: String,
    messages: usize,
    latest_author: Option<String>,
    latest_ts: Option<String>,
    preview: String,
    majority_author: Option<String>,
    majority_count: usize,
    majority_share: f64,
    flooded: bool,
}

#[derive(Serialize)]
struct MsgDto {
    coord: String,
    author: String,
    ts: String,
    text: String,
    preview: String,
}

#[derive(Serialize)]
struct RoomDto {
    thread: String,
    topic: String,
    topic_coord: String,
    title: String,
    messages: Vec<MsgDto>,
    message_count: usize,
    majority_author: Option<String>,
    majority_count: usize,
    majority_share: f64,
    flooded: bool,
}

#[derive(Serialize)]
struct SuggestDto {
    coord: String,
    thread: String,
    title: String,
    topic: String,
    new_thread: bool,
    overlap: Vec<String>,
    reason: String,
}

#[derive(Serialize)]
struct PostOut {
    ok: bool,
    coord: String,
    thread: String,
    title: String,
    topic: String,
    new_thread: bool,
}

#[derive(Serialize)]
struct TocDto {
    textmap: String,
    index: String,
    threads: Vec<TocThread>,
}

#[derive(Serialize)]
struct TocThread {
    thread: String,
    topic: String,
    title: String,
    messages: usize,
}

#[cfg(test)]
mod tests {
    use super::listen_addr;

    #[test]
    fn listen_addr_is_localhost_and_brackets_ipv6() {
        assert_eq!(listen_addr("127.0.0.1", 7841), "127.0.0.1:7841");
        assert_eq!(listen_addr("::1", 7841), "[::1]:7841");
        assert_eq!(listen_addr("[::1]", 7841), "[::1]:7841");
    }

    #[test]
    fn the_page_is_one_feed_and_a_compose_box() {
        let page = include_str!("../static/index.html");
        assert!(page.contains("Posting to:"));
        assert!(page.contains("more held"));
        assert!(page.contains("id=\"details\""));
        assert!(page.contains("id=\"text\""));
        assert!(!page.contains("Coordinate map"));
        assert!(!page.contains("lane-banner"));
        assert!(!page.contains("class=\"spine\""));
        assert!(!page.contains("class=\"pages\""));
    }
}
