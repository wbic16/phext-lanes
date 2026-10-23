//! Small axum reading room. One HTML page, JSON for the feed.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::auth::{AdminTokenCheck, PlotHolderCheck};
use crate::coord::{format_coord, parse_coord, parse_target, thread_key, topic_key, Target};
use crate::error::Error;
use crate::fabric::{self, day_stamp, download_filename, FabricError, ParsedRoute, Route};
use crate::link::{lane_id, post_id};
use crate::model::{preview, Message};
use crate::rank::{self, Feed, MapEntry};
use crate::store::{Post, Store};

const INDEX: &str = include_str!("../static/index.html");
const BODY_LIMIT: usize = fabric::MAX_FABRIC_BYTES;

#[derive(Clone)]
pub struct AppState {
    hub: Arc<fabric::Hub>,
    auth: Arc<dyn PlotHolderCheck>,
}

impl AppState {
    pub fn open(path: std::path::PathBuf) -> Result<Self, Error> {
        Ok(Self {
            hub: Arc::new(fabric::Hub::open(path)?),
            auth: Arc::new(AdminTokenCheck::from_env()),
        })
    }

    pub fn with_auth(
        path: std::path::PathBuf,
        auth: Arc<dyn PlotHolderCheck>,
    ) -> Result<Self, Error> {
        Ok(Self {
            hub: Arc::new(fabric::Hub::open(path)?),
            auth,
        })
    }
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
    let state = AppState::open(path)?;
    let label = state
        .hub
        .with_store(None, |store| store.path().display().to_string())
        .unwrap_or_else(|_| "chat.phext".into());
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
        .route("/", any(dispatch))
        .route("/*path", any(dispatch))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

async fn dispatch(
    State(app): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    method: axum::http::Method,
    body: Bytes,
) -> Response {
    let parsed = match fabric::parse_route(uri.path()) {
        Ok(parsed) => parsed,
        Err(_) => return not_found_page(),
    };
    let get = method == axum::http::Method::GET || method == axum::http::Method::HEAD;
    let post = method == axum::http::Method::POST;
    if parsed.route.is_post() {
        if !post {
            return method_not_allowed();
        }
    } else if !get {
        return method_not_allowed();
    }
    match handle(&app, &parsed, &uri, &headers, &body) {
        Ok(response) => response,
        Err(err) => {
            if matches!(err.status, StatusCode::NOT_FOUND) && parsed.route.is_page() {
                not_found_page()
            } else {
                err.into_response()
            }
        }
    }
}

fn handle(
    app: &AppState,
    parsed: &ParsedRoute,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Response, AppError> {
    let fabric = parsed.fabric.as_deref();
    let query = query_map(uri);
    match &parsed.route {
        Route::Home => Ok(page(app, fabric, headers, PageView::Home)?),
        Route::Lane(id) => Ok(page(app, fabric, headers, PageView::Lane(id))?),
        Route::Post(id) => Ok(page(app, fabric, headers, PageView::Post(id))?),
        Route::Download { lane } => Ok(download(app, fabric, lane.as_deref())?),
        Route::Health => Ok(Json(serde_json::json!({"ok": true})).into_response()),
        Route::State => {
            let user = query.get("user").map(String::as_str);
            Ok(Json(state_of(app, fabric, user)?).into_response())
        }
        Route::ApiLane(id) => Ok(Json(open_lane(app, fabric, id)?).into_response()),
        Route::ApiPost(id) => Ok(Json(open_post(app, fabric, id)?).into_response()),
        Route::Room => {
            let thread = query
                .get("thread")
                .map(String::as_str)
                .unwrap_or("")
                .to_string();
            Ok(Json(room(app, fabric, &thread)?).into_response())
        }
        Route::Suggest => Ok(Json(suggest(
            app,
            fabric,
            query.get("user").map(String::as_str),
            query.get("text").map(String::as_str).unwrap_or(""),
        )?)
        .into_response()),
        Route::Toc => Ok(Json(toc(app, fabric)?).into_response()),
        Route::Scroll => {
            let coord = query.get("coord").map(String::as_str).unwrap_or("");
            Ok(scroll(app, fabric, coord)?)
        }
        Route::PostMessage => Ok(Json(post_message(app, fabric, body)?).into_response()),
        Route::Reset => Ok(Json(reset(app, fabric)?).into_response()),
        Route::Access => Ok(Json(serde_json::json!({
            "create": app.auth.plot_holder_check(&bearer(headers)),
        }))
        .into_response()),
        Route::Create => Ok(Json(create_empty(app, fabric, headers, body)?).into_response()),
        Route::Upload => Ok(Json(upload_fabric(
            app,
            fabric,
            headers,
            body,
            query.get("name").map(String::as_str),
        )?)
        .into_response()),
    }
}

enum PageView<'a> {
    Home,
    Lane(&'a str),
    Post(&'a str),
}

fn page(
    app: &AppState,
    fabric: Option<&str>,
    headers: &HeaderMap,
    view: PageView<'_>,
) -> Result<Response, AppError> {
    let found = app.hub.with_store(fabric, |store| {
        let world = store.world();
        match view {
            PageView::Home => Ok(home_share()),
            PageView::Lane(id) => world
                .find_lane(id)
                .map(lane_share)
                .ok_or_else(AppError::missing),
            PageView::Post(id) => world
                .find_post(id)
                .map(|(thread, message)| post_share(thread, message))
                .ok_or_else(AppError::missing),
        }
    })??;
    let path = match view {
        PageView::Home => fabric_home(fabric),
        PageView::Lane(id) => format!("{}/l/{id}", public_prefix(fabric)),
        PageView::Post(id) => format!("{}/p/{id}", public_prefix(fabric)),
    };
    Ok(render_page(&found, &public_url(headers, &path), fabric).into_response())
}

fn download(
    app: &AppState,
    fabric: Option<&str>,
    lane: Option<&str>,
) -> Result<Response, AppError> {
    let day = day_stamp(Utc::now());
    let packed = app
        .hub
        .with_store(fabric, |store| -> Result<(String, String), AppError> {
            let doc = store.doc().to_string();
            let world = store.world();
            match lane {
                None => Ok((download_filename(fabric, None, &day), doc)),
                Some(id) => {
                    let thread = world.find_lane(id).ok_or_else(AppError::missing)?;
                    let body = fabric::lane_document(&doc, thread);
                    let name = download_filename(fabric, Some(&thread.title), &day);
                    Ok((name, body))
                }
            }
        })??;
    let (filename, body) = packed;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    let disposition = format!("attachment; filename=\"{filename}\"");
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(|_| AppError::server("bad filename"))?,
    );
    Ok((headers, body).into_response())
}

fn state_of(
    app: &AppState,
    fabric: Option<&str>,
    user: Option<&str>,
) -> Result<StateDto, AppError> {
    let names = app.hub.list()?;
    let meta = FabricView {
        fabric: fabric.unwrap_or("").to_string(),
        fabrics: names
            .into_iter()
            .map(|name| FabricDto {
                label: if name.is_empty() {
                    "Lanes".into()
                } else {
                    name.clone()
                },
                name,
            })
            .collect(),
        fork: format!("{}/chat.phext", public_prefix(fabric)),
        demo: fabric.is_none(),
    };
    app.hub
        .with_store(fabric, |store| snapshot(store, user, &meta))?
}

fn open_lane(app: &AppState, fabric: Option<&str>, id: &str) -> Result<OpenDto, AppError> {
    app.hub.with_store(fabric, |store| {
        let world = store.world();
        let thread = world.find_lane(id).ok_or_else(AppError::missing)?;
        Ok(OpenDto {
            highlight: None,
            room: room_dto(thread),
        })
    })?
}

fn open_post(app: &AppState, fabric: Option<&str>, id: &str) -> Result<OpenDto, AppError> {
    app.hub.with_store(fabric, |store| {
        let world = store.world();
        let (thread, message) = world.find_post(id).ok_or_else(AppError::missing)?;
        Ok(OpenDto {
            highlight: Some(post_id(message.coord)),
            room: room_dto(thread),
        })
    })?
}

fn room(app: &AppState, fabric: Option<&str>, thread: &str) -> Result<RoomDto, AppError> {
    let key = thread_key_from(thread)?;
    app.hub.with_store(fabric, |store| {
        let world = store.world();
        let thread = world
            .thread(&key)
            .ok_or_else(|| AppError::bad(format!("no thread at {key}")))?;
        Ok(room_dto(thread))
    })?
}

fn suggest(
    app: &AppState,
    fabric: Option<&str>,
    user: Option<&str>,
    text: &str,
) -> Result<SuggestDto, AppError> {
    app.hub.with_store(fabric, |store| {
        let world = store.world();
        let interests = if world.users.is_empty() {
            Vec::new()
        } else {
            resolve_user(&world, user)?.interests.clone()
        };
        let suggestion = rank::suggest(&world.threads, &interests, text, Utc::now());
        Ok(SuggestDto {
            coord: format_coord(suggestion.coord),
            thread: thread_key(suggestion.thread),
            title: suggestion.title,
            topic: suggestion.topic,
            new_thread: suggestion.new_thread,
            overlap: suggestion.overlap,
            reason: suggestion.reason,
        })
    })?
}

fn toc(app: &AppState, fabric: Option<&str>) -> Result<TocDto, AppError> {
    app.hub.with_store(fabric, |store| {
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
        Ok(TocDto {
            textmap: store.toc(),
            index: store.byte_index(),
            threads,
        })
    })?
}

fn scroll(app: &AppState, fabric: Option<&str>, coord: &str) -> Result<Response, AppError> {
    let coord = parse_coord(coord).map_err(AppError::bad)?;
    let body = app.hub.with_store(fabric, |store| {
        let raw = store.select(coord);
        if raw.is_empty() {
            "(empty scroll)\n".to_string()
        } else {
            raw
        }
    })?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response())
}

#[derive(Deserialize)]
struct PostBody {
    user: String,
    text: String,
    coord: Option<String>,
}

fn post_message(app: &AppState, fabric: Option<&str>, body: &[u8]) -> Result<PostOut, AppError> {
    let body: PostBody = serde_json::from_slice(body).map_err(|_| AppError::bad("bad json"))?;
    app.hub.with_store(fabric, |store| {
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
        let placed = store.place(Post {
            author: user.name,
            text: body.text,
            target,
            now: Utc::now(),
        })?;
        Ok(PostOut {
            ok: true,
            id: post_id(placed.coord),
            lane: lane_id(placed.thread),
            coord: format_coord(placed.coord),
            thread: thread_key(placed.thread),
            title: placed.title,
            topic: placed.topic,
            new_thread: placed.new_thread,
        })
    })?
}

fn reset(app: &AppState, fabric: Option<&str>) -> Result<serde_json::Value, AppError> {
    if fabric.is_some() {
        return Err(AppError::bad("reset is only for the demo fabric"));
    }
    app.hub.with_store(None, |store| {
        store.reseed(Utc::now())?;
        Ok(serde_json::json!({"ok": true}))
    })?
}

#[derive(Deserialize)]
struct CreateBody {
    name: Option<String>,
}

fn create_empty(
    app: &AppState,
    fabric: Option<&str>,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<serde_json::Value, AppError> {
    let parsed: CreateBody = serde_json::from_slice(body).map_err(|_| AppError::bad("bad json"))?;
    let requested = parsed
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    create_named(app, fabric, headers, requested, None)
}

fn upload_fabric(
    app: &AppState,
    fabric: Option<&str>,
    headers: &HeaderMap,
    body: &[u8],
    name: Option<&str>,
) -> Result<serde_json::Value, AppError> {
    let requested = name.map(str::trim).filter(|name| !name.is_empty());
    create_named(app, fabric, headers, requested, Some(body))
}

fn create_named(
    app: &AppState,
    fabric: Option<&str>,
    headers: &HeaderMap,
    requested: Option<&str>,
    upload: Option<&[u8]>,
) -> Result<serde_json::Value, AppError> {
    if fabric.is_some() {
        return Err(AppError::bad("create a fabric from the server root"));
    }
    if !app.auth.plot_holder_check(&bearer(headers)) {
        return Err(AppError::forbidden());
    }
    let ip = client_key(headers);
    let name = match upload {
        Some(bytes) => app.hub.create_upload(requested, bytes, &ip)?,
        None => app.hub.create_empty(requested, &ip)?,
    };
    Ok(serde_json::json!({"ok": true, "name": name}))
}

impl Route {
    fn is_post(&self) -> bool {
        matches!(
            self,
            Route::PostMessage | Route::Reset | Route::Create | Route::Upload
        )
    }

    fn is_page(&self) -> bool {
        matches!(
            self,
            Route::Home | Route::Lane(_) | Route::Post(_) | Route::Download { .. }
        )
    }
}

fn method_not_allowed() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, "method").into_response()
}

fn public_prefix(fabric: Option<&str>) -> String {
    match fabric {
        Some(name) => format!("/f/{name}"),
        None => String::new(),
    }
}

fn fabric_home(fabric: Option<&str>) -> String {
    match fabric {
        Some(name) => format!("/f/{name}/"),
        None => "/".into(),
    }
}

fn bearer(headers: &HeaderMap) -> String {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return String::new();
    };
    let value = value.trim();
    let Some(token) = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
    else {
        return String::new();
    };
    token.trim().to_string()
}

fn client_key(headers: &HeaderMap) -> String {
    let Some(value) = header_token(headers, "x-forwarded-for") else {
        return "local".into();
    };
    if value.len() <= 64
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | ':'))
    {
        value
    } else {
        "local".into()
    }
}

fn query_map(uri: &Uri) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(query) = uri.query() else {
        return out;
    };
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(key), percent_decode(value));
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        if bytes[index] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[index]);
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

struct FabricView {
    fabric: String,
    fabrics: Vec<FabricDto>,
    fork: String,
    demo: bool,
}

fn snapshot(
    store: &Store,
    user_query: Option<&str>,
    meta: &FabricView,
) -> Result<StateDto, AppError> {
    let world = store.world();
    let (viewer, interests) = if world.users.is_empty() {
        (
            ViewerDto {
                id: String::new(),
                name: "Guest".into(),
                blurb: String::new(),
                interests: Vec::new(),
            },
            Vec::new(),
        )
    } else {
        let user = resolve_user(&world, user_query)?;
        let interests = user.interests.clone();
        (viewer_dto(&world, user), interests)
    };
    let now = Utc::now();
    let feed = rank::build_feed(
        &world.threads,
        &interests,
        now,
        rank::AUTHOR_CAP,
        rank::MAX_THREADS,
    );
    let map = rank::map_of(&world.threads, &interests);
    Ok(StateDto {
        file: store.path().display().to_string(),
        fabric: meta.fabric.clone(),
        fabrics: meta.fabrics.clone(),
        fork: meta.fork.clone(),
        demo: meta.demo,
        cap: rank::AUTHOR_CAP,
        loud_at: rank::LOUD_AT,
        viewer,
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
    fn forbidden() -> Self {
        Self::status(
            StatusCode::FORBIDDEN,
            "Founding plot holders in Tessera can create fabrics.",
        )
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

impl From<FabricError> for AppError {
    fn from(err: FabricError) -> Self {
        match err {
            FabricError::Bad(message) => AppError::bad(message),
            FabricError::Conflict(message) => AppError::status(StatusCode::CONFLICT, message),
            FabricError::Limited(message) => {
                AppError::status(StatusCode::TOO_MANY_REQUESTS, message)
            }
            FabricError::Forbidden => AppError::forbidden(),
            FabricError::Missing => {
                AppError::status(StatusCode::NOT_FOUND, "That fabric doesn't exist.")
            }
            FabricError::Io(err) => AppError::server(err.to_string()),
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
    fabric: String,
    fabrics: Vec<FabricDto>,
    fork: String,
    demo: bool,
    cap: usize,
    loud_at: usize,
    viewer: ViewerDto,
    users: Vec<UserDto>,
    feed: Vec<FeedThreadDto>,
    lanes: Vec<LaneDto>,
    map: Vec<MapDto>,
}

#[derive(Clone, Serialize)]
struct FabricDto {
    name: String,
    label: String,
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

fn render_page(share: &Share, og_url: &str, fabric: Option<&str>) -> Html<String> {
    let html = INDEX
        .replace("%%TITLE%%", &escape_html(&share.title))
        .replace("%%DESCRIPTION%%", &escape_html(&share.description))
        .replace("%%OG_TITLE%%", &escape_html(&share.og_title))
        .replace("%%OG_DESCRIPTION%%", &escape_html(&share.description))
        .replace("%%OG_TYPE%%", share.og_type)
        .replace("%%OG_URL%%", &escape_html(og_url))
        .replace("%%FABRIC%%", &escape_html(fabric.unwrap_or("")))
        .replace("%%HOME%%", &escape_html(&fabric_home(fabric)))
        .replace(
            "%%FORK%%",
            &escape_html(&format!("{}/chat.phext", public_prefix(fabric))),
        );
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
        assert!(page.contains("https://mirrorborn.us"));
        assert!(page.contains("Founding plot holders in Tessera can create fabrics."));
        assert!(!page.contains("phext.io"));
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

    struct Temp {
        dir: std::path::PathBuf,
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn app() -> (Router, Temp) {
        let dir = std::env::temp_dir().join(format!(
            "lanes-route-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = AppState::with_auth(
            dir.join("chat.phext"),
            Arc::new(crate::auth::AdminTokenCheck::from_token("plot-secret")),
        )
        .unwrap();
        (router(state), Temp { dir })
    }

    fn bearer(token: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/api/fabrics")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(r#"{"name":"notes"}"#))
            .unwrap()
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

        let reopened = Store::open(file.dir.join("chat.phext")).unwrap();
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
        assert!(body.contains("https://mirrorborn.us"));
        assert!(!body.contains("phext.io"));
    }

    #[tokio::test]
    async fn fork_download_is_open_and_a_lane_fork_is_only_that_lane() {
        let (app, _file) = app();
        let (status, headers, body) = send_parts(&app, get("/chat.phext")).await;
        assert_eq!(status, StatusCode::OK);
        let disposition = headers
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(disposition.starts_with("attachment; filename=\"phext-lanes-"));
        assert!(disposition.ends_with(".phext\""));
        assert!(body.contains("north-wall ferns"));
        assert!(body.contains("starter is slow"));

        let (status, _headers, lane) = send_parts(&app, get("/l/kcre66i9s/chat.phext")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(lane.contains("north-wall ferns"));
        assert!(!lane.contains("starter is slow"));
    }

    #[tokio::test]
    async fn creating_a_fabric_requires_the_plot_check() {
        let (app, file) = app();
        let (status, body) = send(&app, get("/api/fabrics/access")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"create\":false"));

        let (status, body) = send(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/fabrics")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"notes"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("Founding plot holders in Tessera can create fabrics."));
        assert!(!file.dir.join("fabrics/notes.phext").exists());

        let (status, body) = send(&app, bearer("nope")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(!file.dir.join("fabrics/notes.phext").exists());

        let (status, body) = send(&app, bearer("plot-secret")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(file.dir.join("fabrics/notes.phext").exists());

        let (status, page) = send(&app, get("/f/notes/")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("data-fabric=\"notes\""));
        assert!(page.contains("href=\"/f/notes/chat.phext\""));
        assert!(page.contains("https://mirrorborn.us"));
        let (status, home) = send(&app, get("/l/kcre66i9s")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(home.contains("data-fabric=\"\""));
        assert!(home.contains("href=\"/chat.phext\""));
        assert!(!home.contains("href=\"/f/"));

        let (status, body) = send(&app, bearer("plot-secret")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");

        let (status, body) = send(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/fabrics")
                .header("authorization", "Bearer plot-secret")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"../x"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(!file.dir.join("x.phext").exists());
    }

    async fn send_parts(
        app: &Router,
        request: Request<Body>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
    }
}
