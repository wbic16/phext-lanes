//! Small axum reading room. One HTML page, JSON for the feed.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::coord::{format_coord, parse_coord, parse_target, thread_key, topic_key, Target};
use crate::error::Error;
use crate::link::{lane_id, post_id};
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
        .route("/", get(page_home))
        .route("/l/:id", get(page_lane))
        .route("/p/:id", get(page_post))
        .route("/api/health", get(health))
        .route("/api/state", get(state_view))
        .route("/api/l/:id", get(api_lane))
        .route("/api/p/:id", get(api_post))
        .route("/api/room", get(room))
        .route("/api/suggest", get(suggest))
        .route("/api/toc", get(toc))
        .route("/api/scroll", get(scroll))
        .route("/api/post", post(post_message))
        .route("/api/reset", post(reset))
        .with_state(state)
}

async fn page_home(headers: HeaderMap) -> Response {
    render_page(&home_share(), &public_url(&headers, "/")).into_response()
}

async fn page_lane(
    State(app): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let found = {
        let store = match lock(&app) {
            Ok(store) => store,
            Err(err) => return err.into_response(),
        };
        let world = store.world();
        match world.find_lane(&id) {
            Some(thread) => Some((lane_share(thread), format!("/l/{}", lane_id(thread.coord)))),
            None => None,
        }
    };
    match found {
        Some((share, path)) => render_page(&share, &public_url(&headers, &path)).into_response(),
        None => not_found_page(),
    }
}

async fn page_post(
    State(app): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let found = {
        let store = match lock(&app) {
            Ok(store) => store,
            Err(err) => return err.into_response(),
        };
        let world = store.world();
        match world.find_post(&id) {
            Some((thread, message)) => Some((
                post_share(thread, message),
                format!("/p/{}", post_id(message.coord)),
            )),
            None => None,
        }
    };
    match found {
        Some((share, path)) => render_page(&share, &public_url(&headers, &path)).into_response(),
        None => not_found_page(),
    }
}

async fn api_lane(
    State(app): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<OpenDto>, AppError> {
    let store = lock(&app)?;
    let world = store.world();
    let thread = world.find_lane(&id).ok_or_else(AppError::missing)?;
    Ok(Json(OpenDto {
        highlight: None,
        room: room_dto(thread),
    }))
}

async fn api_post(
    State(app): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<OpenDto>, AppError> {
    let store = lock(&app)?;
    let world = store.world();
    let (thread, message) = world.find_post(&id).ok_or_else(AppError::missing)?;
    Ok(Json(OpenDto {
        highlight: Some(post_id(message.coord)),
        room: room_dto(thread),
    }))
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
        id: post_id(placed.coord),
        lane: lane_id(placed.thread),
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
            id: lane_id(thread.coord),
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
        id: lane_id(thread.coord),
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
        id: post_id(message.coord),
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
    fn missing() -> Self {
        Self::status(StatusCode::NOT_FOUND, "That link doesn't go anywhere.")
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
    id: String,
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
    id: String,
    coord: String,
    author: String,
    ts: String,
    text: String,
    preview: String,
}

#[derive(Serialize)]
struct RoomDto {
    id: String,
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
    id: String,
    lane: String,
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

#[derive(Serialize)]
struct OpenDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    highlight: Option<String>,
    #[serde(flatten)]
    room: RoomDto,
}

struct Share {
    title: String,
    og_title: String,
    description: String,
    og_type: &'static str,
}

fn home_share() -> Share {
    Share {
        title: "Lanes".into(),
        og_title: "Lanes".into(),
        description: "A quiet feed of threads.".into(),
        og_type: "website",
    }
}

fn lane_share(thread: &crate::model::Thread) -> Share {
    let description = thread
        .messages
        .first()
        .map(|message| preview(&message.text, 180))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| thread.topic.clone());
    Share {
        title: format!("{} · Lanes", thread.title),
        og_title: thread.title.clone(),
        description,
        og_type: "article",
    }
}

fn post_share(thread: &crate::model::Thread, message: &Message) -> Share {
    let og_title = format!("{} — {}", thread.title, message.author);
    let description = {
        let text = preview(&message.text, 180);
        if text.is_empty() {
            "A message in Lanes.".to_string()
        } else {
            text
        }
    };
    Share {
        title: format!("{og_title} · Lanes"),
        og_title,
        description,
        og_type: "article",
    }
}

fn render_page(share: &Share, og_url: &str) -> Html<String> {
    let html = INDEX
        .replace("%%TITLE%%", &escape_html(&share.title))
        .replace("%%DESCRIPTION%%", &escape_html(&share.description))
        .replace("%%OG_TITLE%%", &escape_html(&share.og_title))
        .replace("%%OG_DESCRIPTION%%", &escape_html(&share.description))
        .replace("%%OG_TYPE%%", share.og_type)
        .replace("%%OG_URL%%", &escape_html(og_url));
    Html(html)
}

fn not_found_page() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        NOT_FOUND_HTML,
    )
        .into_response()
}

const NOT_FOUND_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Not found · Lanes</title>
<meta name="robots" content="noindex">
<style>
  body { margin: 0; min-height: 100vh; background: #f4f1ea; color: #221f1b;
    font-family: "Iowan Old Style", Palatino, Georgia, serif; font-size: 18px; }
  main { max-width: 38rem; margin: 0 auto; padding: 18vh 1.5rem; }
  a { color: inherit; }
</style>
</head>
<body>
<main>
  <p>That link doesn't go anywhere.</p>
  <p><a href="/">Back to Lanes</a></p>
</main>
</body>
</html>
"#;

fn public_url(headers: &HeaderMap, path: &str) -> String {
    match origin_from(headers) {
        Some(origin) => format!("{origin}{path}"),
        None => String::new(),
    }
}

fn origin_from(headers: &HeaderMap) -> Option<String> {
    let proto = header_token(headers, "x-forwarded-proto").unwrap_or_else(|| "http".to_string());
    if proto != "http" && proto != "https" {
        return None;
    }
    let host =
        header_token(headers, "x-forwarded-host").or_else(|| header_token(headers, "host"))?;
    if host.is_empty() || host.len() > 255 || !host_is_safe(&host) {
        return None;
    }
    Some(format!("{proto}://{host}"))
}

fn header_token(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?;
    let token = value.split(',').next()?.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn host_is_safe(host: &str) -> bool {
    host.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':'))
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
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
        assert!(page.contains("copylink"));
        assert!(page.contains("%%OG_TITLE%%"));
        assert!(page.contains("%%OG_DESCRIPTION%%"));
        assert!(!page.contains("Coordinate map"));
        assert!(!page.contains("lane-banner"));
        assert!(!page.contains("class=\"spine\""));
        assert!(!page.contains("class=\"pages\""));
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn app() -> (Router, Temp) {
        let path = std::env::temp_dir().join(format!(
            "lanes-route-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let store = Store::open(&path).unwrap();
        let state = AppState {
            store: Arc::new(Mutex::new(store)),
        };
        (router(state), Temp(path))
    }

    async fn send(app: &Router, request: Request<Body>) -> (StatusCode, String) {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn lane_and_post_pages_preview_the_seed() {
        let (app, file) = app();
        let (status, body) = send(
            &app,
            Request::builder()
                .uri("/l/kcre66i9s")
                .header("x-forwarded-proto", "https")
                .header("host", "mc.phext.io")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("property=\"og:title\" content=\"Fern wall\""));
        assert!(body.contains("property=\"og:description\" content=\"The north-wall ferns"));
        assert!(body.contains("property=\"og:url\" content=\"https://mc.phext.io/l/kcre66i9s\""));
        assert!(body.contains("id=\"text\""));
        assert!(body.contains("copylink"));
        assert!(!body.contains("%%"));

        let (status, body) = send(
            &app,
            Request::builder()
                .uri("/p/k5v1j4f4ds79m9t")
                .header("x-forwarded-proto", "https")
                .header("host", "mc.phext.io")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<title>Fern wall — Mira Quill · Lanes</title>"));
        assert!(body.contains("property=\"og:title\" content=\"Fern wall — Mira Quill\""));
        assert!(body.contains("property=\"og:description\" content=\"The north-wall ferns"));
        assert!(body.contains("property=\"og:type\" content=\"article\""));
        assert!(body.contains("https://mc.phext.io/p/k5v1j4f4ds79m9t"));
        drop(file);
    }

    #[tokio::test]
    async fn unknown_ids_are_a_friendly_404() {
        let (app, _file) = app();
        for uri in [
            "/l/nope",
            "/p/nope",
            "/p/k0",
            "/l/k5v1j4f4ds79m9t",
            "/api/l/nope",
            "/api/p/kzzzz",
        ] {
            let (status, body) = send(&app, get(uri)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
            assert!(
                body.contains("That link doesn't go anywhere."),
                "{uri}: {body}"
            );
        }
        let (_, html) = send(&app, get("/l/nope")).await;
        assert!(!html.contains("id=\"text\""));
        let (_, json) = send(&app, get("/api/l/nope")).await;
        assert!(json.contains("\"error\""));
    }

    #[tokio::test]
    async fn api_resolves_seeded_links_and_home_stays_the_feed() {
        let (app, _file) = app();
        let (status, body) = send(
            &app,
            Request::builder()
                .uri("/")
                .header("host", "127.0.0.1:7841")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<title>Lanes</title>"));
        assert!(body.contains("A quiet feed of threads."));
        assert!(body.contains("http://127.0.0.1:7841/"));
        assert!(body.contains("Posting to:"));

        let (status, body) = send(&app, get("/api/l/kcre66i9s")).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["title"], "Fern wall");
        assert_eq!(json["id"], "kcre66i9s");
        assert!(json.get("highlight").is_none());
        assert!(json["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["id"] == "k5v1j4f4ds79m9t"));

        let (status, body) = send(&app, get("/api/p/k5v1j4f4ds79m9t")).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["highlight"], "k5v1j4f4ds79m9t");
        assert_eq!(json["id"], "kcre66i9s");
        let marked = json["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["id"] == "k5v1j4f4ds79m9t")
            .unwrap();
        assert!(marked["text"]
            .as_str()
            .unwrap()
            .contains("north-wall ferns"));
    }

    #[tokio::test]
    async fn a_new_post_keeps_its_link_and_the_preview_is_escaped() {
        let (app, file) = app();
        let (status, body) = send(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/post")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"user":"mira","text":"say \"hello\" <b>","coord":"1.2.1/1.1.2"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let posted: serde_json::Value = serde_json::from_str(&body).unwrap();
        let id = posted["id"].as_str().unwrap().to_string();
        let lane = posted["lane"].as_str().unwrap().to_string();
        assert_ne!(id, lane);
        assert!(id.starts_with('k'));

        let (status, page) = send(&app, get(&format!("/p/{id}"))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("say &quot;hello&quot; &lt;b&gt;"));
        assert!(!page.contains("content=\"say \"hello\""));

        let (status, again) = send(&app, get(&format!("/api/p/{id}"))).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_str(&again).unwrap();
        assert_eq!(json["highlight"], id);

        let reopened = Store::open(&file.0).unwrap();
        let world = reopened.world();
        let (_, message) = world.find_post(&id).unwrap();
        assert_eq!(message.text, "say \"hello\" <b>");
        assert_eq!(post_id(message.coord), id);
    }

    #[tokio::test]
    async fn a_hostile_host_header_is_not_copied_into_the_page() {
        let (app, _file) = app();
        let (status, body) = send(
            &app,
            Request::builder()
                .uri("/")
                .header("host", "bad host\"><script>")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("property=\"og:url\" content=\"\""));
        assert!(!body.contains("bad host"));
    }
}
