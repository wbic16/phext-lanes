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
use crate::coord::{
    format_coord, parse_coord, parse_target, section_key, thread_key, topic_key, Target,
};
use crate::error::Error;
use crate::fabric::{self, day_stamp, download_filename, FabricError, ParsedRoute, Route};
use crate::limit::{Bucket, Limiter};
use crate::link::{lane_id, post_id, section_lane_id};
use crate::model::{self, preview, Message};
use crate::oauth::{self, Oauth};
use crate::rank::{self, Feed, MapEntry};
use crate::store::{Post, Store};

const INDEX: &str = include_str!("../static/index.html");
const BODY_LIMIT: usize = fabric::MAX_FABRIC_BYTES;
const PAGE: usize = 40;

#[derive(Clone)]
pub struct AppState {
    hub: Arc<fabric::Hub>,
    auth: Arc<dyn PlotHolderCheck>,
    oauth: Arc<Oauth>,
    limits: Arc<Limiter>,
}

impl AppState {
    pub fn open(path: std::path::PathBuf) -> Result<Self, Error> {
        Ok(Self {
            hub: Arc::new(fabric::Hub::open(path)?),
            auth: Arc::new(AdminTokenCheck::from_env()),
            oauth: Arc::new(Oauth::from_env()),
            limits: Arc::new(Limiter::standard()),
        })
    }

    pub fn with_auth(
        path: std::path::PathBuf,
        auth: Arc<dyn PlotHolderCheck>,
    ) -> Result<Self, Error> {
        Self::with_gates(
            path,
            auth,
            Arc::new(Oauth::test_ready()),
            Arc::new(Limiter::standard()),
        )
    }

    fn with_gates(
        path: std::path::PathBuf,
        auth: Arc<dyn PlotHolderCheck>,
        oauth: Arc<Oauth>,
        limits: Arc<Limiter>,
    ) -> Result<Self, Error> {
        Ok(Self {
            hub: Arc::new(fabric::Hub::open(path)?),
            auth,
            oauth,
            limits,
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
    if state.oauth.ready() {
        println!("sign-in  ready");
    } else {
        println!("sign-in  not configured; writes are closed");
    }
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
        Route::ApiLane(id) => {
            let offset = query
                .get("offset")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            Ok(Json(open_lane(app, fabric, id, offset)?).into_response())
        }
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
        Route::PostMessage => Ok(Json(post_message(app, fabric, headers, body)?).into_response()),
        Route::Reset => Ok(Json(reset(app, fabric, headers)?).into_response()),
        Route::Session => Ok(session_view(app, headers)),
        Route::AuthStart(provider) => auth_start(
            app,
            headers,
            provider,
            query.get("next").map(String::as_str).unwrap_or("/"),
        ),
        Route::AuthCallback(provider) => auth_callback(app, headers, provider, &query),
        Route::Logout => logout(app, headers),
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

fn open_lane(
    app: &AppState,
    fabric: Option<&str>,
    id: &str,
    offset: usize,
) -> Result<OpenDto, AppError> {
    app.hub.with_store(fabric, |store| {
        let world = store.world();
        let thread = world.find_lane(id).ok_or_else(AppError::missing)?;
        Ok(OpenDto {
            highlight: None,
            room: room_dto(thread, offset),
        })
    })?
}

fn open_post(app: &AppState, fabric: Option<&str>, id: &str) -> Result<OpenDto, AppError> {
    app.hub.with_store(fabric, |store| {
        let world = store.world();
        let (thread, message) = world.find_post(id).ok_or_else(AppError::missing)?;
        let index = thread
            .messages
            .iter()
            .position(|item| item.coord == message.coord)
            .unwrap_or(0);
        Ok(OpenDto {
            highlight: Some(post_id(message.coord)),
            room: room_dto(thread, (index / PAGE) * PAGE),
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
        Ok(room_dto(thread, 0))
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
                thread: if thread.plain {
                    section_key(thread.coord)
                } else {
                    thread_key(thread.coord)
                },
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

fn post_message(
    app: &AppState,
    fabric: Option<&str>,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<PostOut, AppError> {
    let session = guard_write(app, headers)?;
    let body: PostBody = serde_json::from_slice(body).map_err(|_| AppError::bad("bad json"))?;
    app.hub.with_store(fabric, |store| {
        let target = {
            let world = store.world();
            let user = resolve_user(world, Some(&body.user))?.clone();
            if let Some(coord) = body
                .coord
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                parse_target(coord).map_err(AppError::bad)?
            } else {
                let suggestion =
                    rank::suggest(&world.threads, &user.interests, &body.text, Utc::now());
                Target::Scroll(suggestion.coord)
            }
        };
        let placed = store.place(Post {
            author: session.handle.clone(),
            who: session.who.clone(),
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

fn reset(
    app: &AppState,
    fabric: Option<&str>,
    headers: &HeaderMap,
) -> Result<serde_json::Value, AppError> {
    let _session = guard_write(app, headers)?;
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
    let _session = guard_write(app, headers)?;
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
            Route::PostMessage | Route::Reset | Route::Create | Route::Upload | Route::Logout
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
    for name in ["x-real-ip", "x-forwarded-for"] {
        if let Some(value) = header_token(headers, name) {
            if value.len() <= 64
                && value
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | ':'))
            {
                return value;
            }
        }
    }
    "local".into()
}

fn guard_auth(app: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    if !app.oauth.ready() {
        return Err(AppError::status(
            StatusCode::FORBIDDEN,
            "Sign-in is not set up yet.",
        ));
    }
    if app
        .limits
        .hit(Bucket::AuthIp, &client_key(headers))
        .is_err()
    {
        return Err(AppError::status(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests.",
        ));
    }
    Ok(())
}

fn guard_write(app: &AppState, headers: &HeaderMap) -> Result<oauth::Session, AppError> {
    if !app.oauth.ready() {
        return Err(AppError::status(
            StatusCode::FORBIDDEN,
            "Sign-in is not set up yet.",
        ));
    }
    if app
        .limits
        .hit(Bucket::WriteIp, &client_key(headers))
        .is_err()
    {
        return Err(AppError::status(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests.",
        ));
    }
    let session = app
        .oauth
        .read_session(&oauth::cookie_from_headers(headers))
        .ok_or_else(|| AppError::status(StatusCode::UNAUTHORIZED, "Sign in to do that."))?;
    let csrf = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !app.oauth.csrf_matches(&session, csrf) {
        return Err(AppError::status(
            StatusCode::FORBIDDEN,
            "That request did not match this sign-in.",
        ));
    }
    if app.limits.hit(Bucket::WriteAccount, &session.who).is_err() {
        return Err(AppError::status(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests.",
        ));
    }
    Ok(session)
}

fn session_view(app: &AppState, headers: &HeaderMap) -> Response {
    let session = app.oauth.read_session(&oauth::cookie_from_headers(headers));
    Json(serde_json::json!({
        "ready": app.oauth.ready(),
        "signed_in": session.is_some(),
        "handle": session.as_ref().map(|item| item.handle.as_str()).unwrap_or(""),
        "csrf": session.as_ref().map(|item| item.csrf.as_str()).unwrap_or(""),
        "providers": app.oauth.providers(),
    }))
    .into_response()
}

fn auth_start(
    app: &AppState,
    headers: &HeaderMap,
    provider: &str,
    next: &str,
) -> Result<Response, AppError> {
    guard_auth(app, headers)?;
    let (url, pending) = app
        .oauth
        .authorize_url(provider, next)
        .map_err(|message| AppError::status(StatusCode::FORBIDDEN, message))?;
    let mut out = HeaderMap::new();
    out.insert(
        header::LOCATION,
        HeaderValue::from_str(&url).map_err(|_| AppError::server("bad redirect"))?,
    );
    out.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&oauth::pending_cookie(&pending, app.oauth.cookie_secure()))
            .map_err(|_| AppError::server("bad cookie"))?,
    );
    Ok((StatusCode::FOUND, out).into_response())
}

fn auth_callback(
    app: &AppState,
    headers: &HeaderMap,
    provider: &str,
    query: &HashMap<String, String>,
) -> Result<Response, AppError> {
    guard_auth(app, headers)?;
    let code = query.get("code").map(String::as_str).unwrap_or("");
    let state = query.get("state").map(String::as_str).unwrap_or("");
    if query.contains_key("error") || code.is_empty() || state.is_empty() {
        return Ok(auth_failed(app));
    }
    let pending = oauth::pending_token(&oauth::cookie_from_headers(headers)).unwrap_or_default();
    match app.oauth.finish(provider, code, state, &pending) {
        Ok((_identity, session, next)) => {
            let mut out = HeaderMap::new();
            out.insert(
                header::LOCATION,
                HeaderValue::from_str(&next).map_err(|_| AppError::server("bad redirect"))?,
            );
            out.append(
                header::SET_COOKIE,
                HeaderValue::from_str(&oauth::session_cookie(&session, app.oauth.cookie_secure()))
                    .map_err(|_| AppError::server("bad cookie"))?,
            );
            out.append(
                header::SET_COOKIE,
                HeaderValue::from_str(&oauth::clear_cookie(
                    oauth::pending_cookie_name(),
                    app.oauth.cookie_secure(),
                ))
                .map_err(|_| AppError::server("bad cookie"))?,
            );
            Ok((StatusCode::FOUND, out).into_response())
        }
        Err(_) => Ok(auth_failed(app)),
    }
}

fn auth_failed(app: &AppState) -> Response {
    let mut out = HeaderMap::new();
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    if let Ok(cookie) = HeaderValue::from_str(&oauth::clear_cookie(
        oauth::pending_cookie_name(),
        app.oauth.cookie_secure(),
    )) {
        out.insert(header::SET_COOKIE, cookie);
    }
    (
        StatusCode::BAD_REQUEST,
        out,
        Html(
            "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Lanes</title></head><body><p>Sign-in did not complete. <a href=\"/\">Back to Lanes</a></p></body></html>",
        ),
    )
        .into_response()
}

fn logout(app: &AppState, headers: &HeaderMap) -> Result<Response, AppError> {
    guard_auth(app, headers)?;
    let session = app
        .oauth
        .read_session(&oauth::cookie_from_headers(headers))
        .ok_or_else(|| AppError::status(StatusCode::UNAUTHORIZED, "Sign in to do that."))?;
    let csrf = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !app.oauth.csrf_matches(&session, csrf) {
        return Err(AppError::status(
            StatusCode::FORBIDDEN,
            "That request did not match this sign-in.",
        ));
    }
    let mut out = HeaderMap::new();
    out.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&oauth::clear_cookie(
            oauth::session_cookie_name(),
            app.oauth.cookie_secure(),
        ))
        .map_err(|_| AppError::server("bad cookie"))?,
    );
    Ok((out, Json(serde_json::json!({"ok": true}))).into_response())
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
    let chat: Vec<crate::model::Thread> = world
        .threads
        .iter()
        .filter(|thread| !thread.plain)
        .cloned()
        .collect();
    let feed = rank::build_feed(&chat, &interests, now, rank::AUTHOR_CAP, rank::MAX_THREADS);
    let map = rank::map_of(&chat, &interests);
    let mut feed_rows = feed_dto(&feed);
    for thread in world.threads.iter().filter(|thread| thread.plain) {
        feed_rows.push(plain_row(thread));
    }
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
        feed: feed_rows,
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
            plain: false,
        })
        .collect()
}

/// One line of a section. Later scrolls stay out of the feed payload.
fn plain_row(thread: &crate::model::Thread) -> FeedThreadDto {
    let preview_message = thread.messages.first().and_then(|message| {
        let rest = model::rest_after_opening(&message.text);
        let shown = preview(&rest, 160);
        if shown.is_empty() {
            return None;
        }
        Some(MsgDto {
            id: post_id(message.coord),
            coord: format_coord(message.coord),
            author: message.author.clone(),
            ts: stamp(message.ts),
            text: shown.clone(),
            preview: shown,
        })
    });
    FeedThreadDto {
        id: section_lane_id(thread.coord),
        thread: section_key(thread.coord),
        topic: thread.topic.clone(),
        topic_coord: topic_key(thread.coord),
        title: thread.title.clone(),
        distance: 0,
        score: 0.0,
        nearness: String::new(),
        messages: preview_message.into_iter().collect(),
        held: Vec::new(),
        plain: true,
    }
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

fn room_dto(thread: &crate::model::Thread, offset: usize) -> RoomDto {
    let total = thread.messages.len();
    let (start, end) = if offset >= total {
        (total, total)
    } else {
        (offset, (offset + PAGE).min(total))
    };
    let page = &thread.messages[start..end];
    let (majority_author, majority_count, majority_share) = rank::majority(&thread.messages);
    RoomDto {
        id: if thread.plain {
            section_lane_id(thread.coord)
        } else {
            lane_id(thread.coord)
        },
        thread: if thread.plain {
            section_key(thread.coord)
        } else {
            thread_key(thread.coord)
        },
        topic: thread.topic.clone(),
        topic_coord: topic_key(thread.coord),
        title: thread.title.clone(),
        messages: page.iter().map(msg_dto).collect(),
        message_count: total,
        offset: start,
        more: end < total,
        plain: thread.plain,
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
    plain: bool,
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
    offset: usize,
    more: bool,
    plain: bool,
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
    if thread.plain || message.plain {
        let title = model::opening_line(&message.text, 80);
        let rest = model::rest_after_opening(&message.text);
        let description = {
            let text = preview(
                if rest.is_empty() {
                    &message.text
                } else {
                    &rest
                },
                180,
            );
            if text.is_empty() {
                "A scroll.".to_string()
            } else {
                text
            }
        };
        return Share {
            title: format!("{title} · Lanes"),
            og_title: title,
            description,
            og_type: "article",
        };
    }
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
        assert!(page.contains("id=\"signin\""));
        assert!(page.contains(">Sign in<"));
        assert!(!page.contains("phext.io"));
        assert!(page.contains("%%OG_TITLE%%"));
        assert!(page.contains("%%OG_DESCRIPTION%%"));
        assert!(!page.contains("Coordinate map"));
        assert!(!page.contains("lane-banner"));
        assert!(!page.contains("class=\"spine\""));
        assert!(!page.contains("class=\"pages\""));
        assert!(page.contains("Nothing in this phext yet."));
        assert!(page.contains("prefers-color-scheme: dark"));
        assert!(page.contains("color-scheme: dark"));
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

    fn with_session(builder: axum::http::request::Builder) -> axum::http::request::Builder {
        builder
            .header(
                "cookie",
                format!(
                    "lanes_session={}",
                    crate::oauth::Oauth::test_session_cookie()
                ),
            )
            .header("x-csrf-token", crate::oauth::TEST_CSRF)
    }

    fn bearer(token: &str) -> Request<Body> {
        with_session(
            Request::builder()
                .method("POST")
                .uri("/api/fabrics")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}")),
        )
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
            with_session(
                Request::builder()
                    .method("POST")
                    .uri("/api/post")
                    .header("content-type", "application/json"),
            )
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
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("Sign in to do that."));
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
            with_session(
                Request::builder()
                    .method("POST")
                    .uri("/api/fabrics")
                    .header("authorization", "Bearer plot-secret")
                    .header("content-type", "application/json"),
            )
            .body(Body::from(r#"{"name":"../x"}"#))
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(!file.dir.join("x.phext").exists());
    }

    fn put_scroll(doc: &mut String, coord: &str, text: &str) {
        *doc = libphext::phext::insert(std::mem::take(doc), parse_coord(coord).unwrap(), text);
    }

    async fn upload(app: &Router, name: &str, doc: &str) {
        let (status, body) = send(
            app,
            with_session(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/fabrics/upload?name={name}"))
                    .header("authorization", "Bearer plot-secret")
                    .header("content-type", "application/octet-stream"),
            )
            .body(Body::from(doc.to_string()))
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    #[tokio::test]
    async fn ordinary_phext_is_readable_and_chat_stays_chat() {
        let (app, _file) = app();
        let mut doc = String::new();
        put_scroll(
            &mut doc,
            "1.1.1/1.1.1/1.1.1",
            "Incipit (It Begins)\nThe opening stays.",
        );
        put_scroll(
            &mut doc,
            "1.1.1/1.1.1/1.1.2",
            "TTSM\nA second scroll in the same section.",
        );
        put_scroll(&mut doc, "1.1.1/1.1.1/2.1.1", "TAOP\nAnother chapter.");
        put_scroll(&mut doc, "1.1.1/1.2.1/1.1.1", "MOAT\nAnother book.");
        put_scroll(
            &mut doc,
            "1.2.1/1.1.1/1.1.1",
            &crate::model::format_card("Gardens", "Fern wall", "fern"),
        );
        let ts = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 10, 22, 18, 0, 0).unwrap();
        put_scroll(
            &mut doc,
            "1.2.1/1.1.1/1.1.2",
            &crate::model::format_message(
                "Mira Quill",
                ts,
                "The north-wall ferns finally unfurled.",
            ),
        );
        upload(&app, "incipit", &doc).await;

        let (status, body) = send(&app, get("/f/incipit/api/state?user=x")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let titles: Vec<&str> = json["feed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|thread| thread["title"].as_str().unwrap())
            .collect();
        assert!(titles.contains(&"Incipit (It Begins)"), "{titles:?}");
        assert!(titles.contains(&"TAOP"), "{titles:?}");
        assert!(titles.contains(&"MOAT"), "{titles:?}");
        assert!(titles.contains(&"Fern wall"), "{titles:?}");
        assert!(!titles.contains(&"TTSM"), "{titles:?}");
        assert!(!body.contains("A second scroll"));
        let fern = json["feed"]
            .as_array()
            .unwrap()
            .iter()
            .find(|thread| thread["title"] == "Fern wall")
            .unwrap();
        assert_eq!(fern["plain"], false);
        assert_eq!(fern["id"], "kcre66i9s");
        assert_eq!(fern["messages"][0]["author"], "Mira Quill");
        let incipit = json["feed"]
            .as_array()
            .unwrap()
            .iter()
            .find(|thread| thread["title"] == "Incipit (It Begins)")
            .unwrap();
        assert_eq!(incipit["plain"], true);
        assert_eq!(incipit["id"], "s0");
        assert_eq!(incipit["thread"], "1.1.1/1.1.1/1.1");

        let (status, body) = send(&app, get("/f/incipit/api/l/s0")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let room: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(room["messages"].as_array().unwrap().len(), 2);
        assert!(room["messages"][1]["text"]
            .as_str()
            .unwrap()
            .starts_with("TTSM"));
        assert_eq!(room["more"], false);

        let (status, body) = send(&app, get("/f/incipit/api/p/k1")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let post: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(post["highlight"], "k1");
        assert_eq!(post["id"], "s0");
        assert_eq!(post["plain"], true);

        let (status, page) = send(&app, get("/f/incipit/p/k1")).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert!(page.contains("TTSM"));
        assert!(!page.contains("phext —"));

        let (status, _headers, fork) = send_parts(&app, get("/f/incipit/l/s0/chat.phext")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(fork.contains("The opening stays."));
        assert!(fork.contains("TTSM"));
        assert!(!fork.contains("Another chapter."));

        let (status, body) = send(&app, get("/f/incipit/api/l/kcre66i9s")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let fern_room: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(fern_room["plain"], false);
        assert_eq!(fern_room["id"], "kcre66i9s");
        assert_eq!(fern_room["messages"][0]["author"], "Mira Quill");
    }

    #[tokio::test]
    async fn a_long_section_is_paged_and_absent_from_the_feed() {
        let (app, _file) = app();
        let mut doc = String::new();
        for scroll in 1..=45 {
            let text = if scroll == 45 {
                "Scroll 45\nsecret-body-45".to_string()
            } else if scroll == 1 {
                "Scroll 1\nkept in the summary".to_string()
            } else {
                format!("Scroll {scroll}\nbody {scroll}")
            };
            put_scroll(&mut doc, &format!("1.1.1/1.1.1/1.1.{scroll}"), &text);
        }
        upload(&app, "long", &doc).await;

        let (status, body) = send(&app, get("/f/long/api/state?user=x")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains("secret-body-45"), "{body}");
        assert!(body.contains("kept in the summary"));
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["feed"].as_array().unwrap().len(), 1);
        assert_eq!(json["feed"][0]["title"], "Scroll 1");
        assert_eq!(json["feed"][0]["id"], "s0");
        assert_eq!(json["users"].as_array().unwrap().len(), 0);

        let (status, body) = send(&app, get("/f/long/api/l/s0")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let first: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(first["messages"].as_array().unwrap().len(), 40);
        assert_eq!(first["offset"], 0);
        assert_eq!(first["more"], true);
        assert_eq!(first["message_count"], 45);
        assert!(!body.contains("secret-body-45"));

        let (status, body) = send(&app, get("/f/long/api/l/s0?offset=40")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let second: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(second["messages"].as_array().unwrap().len(), 5);
        assert_eq!(second["offset"], 40);
        assert_eq!(second["more"], false);
        assert!(body.contains("secret-body-45"));
        let last = second["messages"].as_array().unwrap().last().unwrap();
        let id = last["id"].as_str().unwrap();

        let (status, body) = send(&app, get(&format!("/f/long/api/p/{id}"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let opened: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(opened["highlight"], id);
        assert_eq!(opened["offset"], 40);
        assert!(opened["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["text"].as_str().unwrap().contains("secret-body-45")));
    }

    #[tokio::test]
    async fn many_sections_stay_listed_without_the_thread_cap() {
        let (app, _file) = app();
        let mut doc = String::new();
        for section in 1..=41 {
            put_scroll(
                &mut doc,
                &format!("1.1.1/1.1.1/1.{section}.1"),
                &format!("Section {section}"),
            );
        }
        upload(&app, "wide", &doc).await;
        let (status, body) = send(&app, get("/f/wide/api/state?user=x")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let feed = json["feed"].as_array().unwrap();
        assert_eq!(feed.len(), 41);
        assert!(feed.iter().all(|thread| thread["plain"] == true));
        assert_eq!(feed[0]["title"], "Section 1");
        assert_eq!(feed[40]["title"], "Section 41");
        assert!(feed
            .iter()
            .all(|thread| thread["messages"].as_array().unwrap().is_empty()));
    }

    #[tokio::test]
    async fn an_empty_fabric_has_an_empty_feed() {
        let (app, _file) = app();
        let (status, body) = send(&app, bearer("plot-secret")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = send(&app, get("/f/notes/api/state?user=mira")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["feed"].as_array().unwrap().len(), 0);
        let (status, page) = send(&app, get("/f/notes/")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("Nothing in this phext yet."));
    }

    fn app_with(oauth: crate::oauth::Oauth, limits: crate::limit::Limiter) -> (Router, Temp) {
        let dir = std::env::temp_dir().join(format!(
            "lanes-route-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = AppState::with_gates(
            dir.join("chat.phext"),
            Arc::new(crate::auth::AdminTokenCheck::from_token("plot-secret")),
            Arc::new(oauth),
            Arc::new(limits),
        )
        .unwrap();
        (router(state), Temp { dir })
    }

    fn set_cookie(headers: &axum::http::HeaderMap, name: &str) -> String {
        headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(|value| {
                let rest = value.strip_prefix(&format!("{name}="))?;
                Some(rest.split(';').next().unwrap_or("").to_string())
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn reading_and_fork_stay_open_without_a_session() {
        let (app, _file) = app();
        let (status, body) = send(&app, get("/l/kcre66i9s")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Fern wall"));
        let (status, _headers, fork) = send_parts(&app, get("/chat.phext")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(fork.contains("north-wall ferns"));
        let (status, body) = send(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/post")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"user":"mira","text":"no session"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert!(body.contains("Sign in to do that."));
    }

    #[tokio::test]
    async fn unset_oauth_closes_writes_with_a_calm_notice() {
        let (app, _file) = app_with(
            crate::oauth::Oauth::closed(),
            crate::limit::Limiter::standard(),
        );
        let (status, page) = send(&app, get("/")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("Sign in"));
        let (status, body) = send(
            &app,
            with_session(
                Request::builder()
                    .method("POST")
                    .uri("/api/post")
                    .header("content-type", "application/json"),
            )
            .body(Body::from(r#"{"user":"mira","text":"closed"}"#))
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(body.contains("Sign-in is not set up yet."));
        let (status, _) = send(&app, get("/chat.phext")).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn github_sign_in_sets_a_session_and_the_poster_identity() {
        let (app, file) = app();
        let (status, headers, _body) =
            send_parts(&app, get("/auth/github?next=/l/kcre66i9s")).await;
        assert_eq!(status, StatusCode::FOUND);
        let location = headers.get(header::LOCATION).unwrap().to_str().unwrap();
        assert!(location.starts_with("https://github.com/login/oauth/authorize?"));
        assert!(location.contains("code_challenge="));
        assert!(location.contains("code_challenge_method=S256"));
        assert!(location
            .contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A7841%2Fauth%2Fgithub%2Fcallback"));
        let pending_header = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(pending_header.contains("HttpOnly"));
        assert!(pending_header.contains("SameSite=Lax"));
        assert!(pending_header.contains("Secure"));
        let pending = set_cookie(&headers, "lanes_oauth");
        let state = location
            .split('&')
            .find_map(|part| {
                part.strip_prefix("state=")
                    .or_else(|| part.split_once("state=").map(|(_, value)| value))
            })
            .unwrap()
            .to_string();
        let (status, headers, body) = send_parts(
            &app,
            Request::builder()
                .uri(format!("/auth/github/callback?code=ok&state={state}"))
                .header("cookie", format!("lanes_oauth={pending}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FOUND, "{body}");
        assert_eq!(
            headers.get(header::LOCATION).unwrap().to_str().unwrap(),
            "/l/kcre66i9s"
        );
        let session = set_cookie(&headers, "lanes_session");
        assert!(!session.is_empty());
        let (status, me) = send(
            &app,
            Request::builder()
                .uri("/api/session")
                .header("cookie", format!("lanes_session={session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{me}");
        let me: serde_json::Value = serde_json::from_str(&me).unwrap();
        assert_eq!(me["handle"], "octocat");
        assert_eq!(me["signed_in"], true);
        let csrf = me["csrf"].as_str().unwrap();
        let (status, posted) = send(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/post")
                .header("content-type", "application/json")
                .header("cookie", format!("lanes_session={session}"))
                .header("x-csrf-token", csrf)
                .body(Body::from(
                    r#"{"user":"mira","text":"signed in note","coord":"1.2.1/1.1.1"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let saved = crate::store::Store::open(file.dir.join("chat.phext")).unwrap();
        let raw = saved.doc();
        assert!(raw.contains("author: octocat"));
        assert!(raw.contains("who: github:100"));

        let (status, _, denied) = send_parts(
            &app,
            Request::builder()
                .uri("/auth/github/callback?code=ok&state=nope")
                .header("cookie", format!("lanes_oauth={pending}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(denied.contains("Sign-in did not complete."));

        let (status, hostile, _) =
            send_parts(&app, get("/auth/github?next=https://evil.example")).await;
        assert_eq!(status, StatusCode::FOUND);
        let hostile_pending = set_cookie(&hostile, "lanes_oauth");
        let hostile_state = hostile
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let (status, back, _) = send_parts(
            &app,
            Request::builder()
                .uri(format!(
                    "/auth/github/callback?code=ok&state={hostile_state}"
                ))
                .header("cookie", format!("lanes_oauth={hostile_pending}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(back.get(header::LOCATION).unwrap().to_str().unwrap(), "/");
    }

    #[tokio::test]
    async fn auth_and_writes_are_rate_limited() {
        let (app, _file) = app_with(
            crate::oauth::Oauth::test_ready(),
            crate::limit::Limiter::with_caps(2, 2, 2),
        );
        for _ in 0..2 {
            let (status, _, _) = send_parts(&app, get("/auth/github")).await;
            assert_eq!(status, StatusCode::FOUND);
        }
        let (status, body) = send(&app, get("/auth/github")).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert!(body.contains("Too many requests."));
        for _ in 0..2 {
            let (status, body) = send(
                &app,
                with_session(
                    Request::builder()
                        .method("POST")
                        .uri("/api/post")
                        .header("content-type", "application/json")
                        .header("x-real-ip", "203.0.113.50"),
                )
                .body(Body::from(
                    r#"{"user":"mira","text":"paced","coord":"1.1.1/1.1.1"}"#,
                ))
                .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
        let (status, body) = send(
            &app,
            with_session(
                Request::builder()
                    .method("POST")
                    .uri("/api/post")
                    .header("content-type", "application/json")
                    .header("x-real-ip", "203.0.113.50"),
            )
            .body(Body::from(
                r#"{"user":"mira","text":"paced again","coord":"1.1.1/1.1.1"}"#,
            ))
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
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
