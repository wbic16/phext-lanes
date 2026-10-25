//! GitHub and Google sign-in.
//!
//! The session is an HttpOnly cookie signed with `LANES_SESSION_SECRET`.
//! The authorization-code flow uses PKCE and a `state` value kept in a
//! second short-lived cookie. When the client ids are unset, writes stay
//! closed.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::auth::token_eq;

const SESSION_COOKIE: &str = "lanes_session";
const PENDING_COOKIE: &str = "lanes_oauth";
const SESSION_TTL: i64 = 60 * 60 * 24 * 14;
const PENDING_TTL: i64 = 600;
pub const TEST_CSRF: &str = "csrf-test-token";

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct Identity {
    pub who: String,
    pub handle: String,
    pub name: String,
    pub email: String,
}

pub trait IdentitySource: Send + Sync {
    fn exchange(
        &self,
        provider: &str,
        code: &str,
        verifier: &str,
        redirect: &str,
        client_id: &str,
        client_secret: &str,
    ) -> Result<Identity, String>;
}

#[derive(Clone)]
struct ProviderKeys {
    id: String,
    secret: String,
}

pub struct Oauth {
    origin: String,
    session_secret: String,
    cookie_secure: bool,
    github: Option<ProviderKeys>,
    google: Option<ProviderKeys>,
    source: Arc<dyn IdentitySource>,
}

impl std::fmt::Debug for Oauth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Oauth")
    }
}

#[derive(Clone)]
pub struct Session {
    pub who: String,
    pub handle: String,
    pub name: String,
    pub email: String,
    pub csrf: String,
}

#[derive(Serialize, Deserialize)]
struct SessionBody {
    who: String,
    handle: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    csrf: String,
    exp: i64,
}

#[derive(Serialize, Deserialize)]
struct PendingBody {
    provider: String,
    state: String,
    verifier: String,
    next: String,
    exp: i64,
}

pub struct Pending {
    pub provider: String,
    pub state: String,
    pub verifier: String,
    pub next: String,
}

struct HttpSource;

impl IdentitySource for HttpSource {
    fn exchange(
        &self,
        provider: &str,
        code: &str,
        verifier: &str,
        redirect: &str,
        client_id: &str,
        client_secret: &str,
    ) -> Result<Identity, String> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(10))
            .redirects(0)
            .build();
        match provider {
            "github" => github_identity(&agent, code, verifier, redirect, client_id, client_secret),
            "google" => google_identity(&agent, code, verifier, redirect, client_id, client_secret),
            _ => Err("the provider did not complete sign-in.".into()),
        }
    }
}

struct ScriptedSource;

impl IdentitySource for ScriptedSource {
    fn exchange(
        &self,
        provider: &str,
        code: &str,
        _verifier: &str,
        _redirect: &str,
        _client_id: &str,
        _client_secret: &str,
    ) -> Result<Identity, String> {
        if code != "ok" {
            return Err("the provider did not complete sign-in.".into());
        }
        match provider {
            "github" => Ok(Identity {
                who: "github:100".into(),
                handle: "octocat".into(),
                name: "The Octocat".into(),
                email: "octo@example.com".into(),
            }),
            "google" => Ok(Identity {
                who: "google:sub-1".into(),
                handle: "ada".into(),
                name: "Ada Example".into(),
                email: "ada@example.com".into(),
            }),
            _ => Err("the provider did not complete sign-in.".into()),
        }
    }
}

impl Oauth {
    pub fn from_env() -> Self {
        Self::from_parts(
            &std::env::var("LANES_PUBLIC_ORIGIN").unwrap_or_default(),
            &std::env::var("LANES_SESSION_SECRET").unwrap_or_default(),
            std::env::var("LANES_COOKIE_SECURE")
                .unwrap_or_default()
                .trim()
                == "1",
            &std::env::var("GITHUB_CLIENT_ID").unwrap_or_default(),
            &std::env::var("GITHUB_CLIENT_SECRET").unwrap_or_default(),
            &std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default(),
            &std::env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default(),
            Arc::new(HttpSource),
        )
    }

    pub fn test_ready() -> Self {
        Self::from_parts(
            "http://127.0.0.1:7841",
            "test-session-secret-must-be-32chars",
            true,
            "github-client-id",
            "github-client-secret",
            "google-client-id",
            "google-client-secret",
            Arc::new(ScriptedSource),
        )
    }

    pub fn closed() -> Self {
        Self::from_parts("", "", false, "", "", "", "", Arc::new(ScriptedSource))
    }

    #[allow(clippy::too_many_arguments)]
    fn from_parts(
        origin: &str,
        session_secret: &str,
        cookie_secure: bool,
        github_id: &str,
        github_secret: &str,
        google_id: &str,
        google_secret: &str,
        source: Arc<dyn IdentitySource>,
    ) -> Self {
        let origin = origin.trim().trim_end_matches('/').to_string();
        let origin = if valid_origin(&origin) {
            origin
        } else {
            String::new()
        };
        Self {
            origin,
            session_secret: session_secret.trim().to_string(),
            cookie_secure,
            github: provider(github_id, github_secret),
            google: provider(google_id, google_secret),
            source,
        }
    }

    pub fn ready(&self) -> bool {
        real_session_secret(&self.session_secret)
            && !self.origin.is_empty()
            && (self.github.is_some() || self.google.is_some())
    }

    pub fn providers(&self) -> Vec<&'static str> {
        if !self.ready() {
            return Vec::new();
        }
        let mut names = Vec::new();
        if self.github.is_some() {
            names.push("github");
        }
        if self.google.is_some() {
            names.push("google");
        }
        names
    }

    pub fn cookie_secure(&self) -> bool {
        self.cookie_secure
    }

    pub fn authorize_url(
        &self,
        provider_name: &str,
        next: &str,
    ) -> Result<(String, String), String> {
        if !self.ready() {
            return Err("Sign-in is not set up yet.".into());
        }
        let keys = self
            .keys(provider_name)
            .ok_or_else(|| "Sign-in is not set up yet.".to_string())?;
        let verifier = random_token(32)?;
        let state = random_token(16)?;
        let challenge = pkce_challenge(&verifier);
        let redirect = self.redirect_uri(provider_name);
        let next = safe_next(next);
        let url = match provider_name {
            "github" => format!(
                "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope=read:user&state={}&code_challenge={}&code_challenge_method=S256",
                form_encode(&keys.id),
                form_encode(&redirect),
                form_encode(&state),
                form_encode(&challenge),
            ),
            "google" => format!(
                "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256&prompt=select_account",
                form_encode(&keys.id),
                form_encode(&redirect),
                form_encode("openid email profile"),
                form_encode(&state),
                form_encode(&challenge),
            ),
            _ => return Err("Sign-in is not set up yet.".into()),
        };
        let pending = self.seal(&PendingBody {
            provider: provider_name.to_string(),
            state,
            verifier,
            next,
            exp: Utc::now().timestamp() + PENDING_TTL,
        })?;
        Ok((url, pending))
    }

    pub fn finish(
        &self,
        provider_name: &str,
        code: &str,
        state: &str,
        pending_cookie: &str,
    ) -> Result<(Identity, String, String), String> {
        if !self.ready() {
            return Err("Sign-in is not set up yet.".into());
        }
        let pending = self.open_pending(pending_cookie)?;
        if pending.provider != provider_name || !token_eq(&pending.state, state) {
            return Err("Sign-in did not complete.".into());
        }
        let keys = self
            .keys(provider_name)
            .ok_or_else(|| "Sign-in is not set up yet.".to_string())?;
        let identity = self.source.exchange(
            provider_name,
            code,
            &pending.verifier,
            &self.redirect_uri(provider_name),
            &keys.id,
            &keys.secret,
        )?;
        if !valid_who(&identity.who) || identity.handle.is_empty() {
            return Err("Sign-in did not complete.".into());
        }
        let csrf = random_token(16)?;
        let name = display_name(&identity.name, &identity.handle);
        let email = clean_email(&identity.email);
        let session = self.seal(&SessionBody {
            who: identity.who.clone(),
            handle: identity.handle.clone(),
            name,
            email,
            csrf,
            exp: Utc::now().timestamp() + SESSION_TTL,
        })?;
        Ok((identity, session, pending.next))
    }

    pub fn read_session(&self, cookie_header: &str) -> Option<Session> {
        if !real_session_secret(&self.session_secret) {
            return None;
        }
        let raw = cookie_value(cookie_header, SESSION_COOKIE)?;
        let body = self.open::<SessionBody>(&raw).ok()?;
        if body.exp < Utc::now().timestamp() || body.csrf.is_empty() || !valid_who(&body.who) {
            return None;
        }
        let handle = clean_handle(&body.handle);
        if handle.is_empty() {
            return None;
        }
        Some(Session {
            who: body.who,
            handle: handle.clone(),
            name: display_name(&body.name, &handle),
            email: clean_email(&body.email),
            csrf: body.csrf,
        })
    }

    pub fn csrf_matches(&self, session: &Session, header: &str) -> bool {
        !session.csrf.is_empty() && token_eq(&session.csrf, header.trim())
    }

    pub fn test_session_cookie() -> String {
        let oauth = Self::test_ready();
        oauth
            .seal(&SessionBody {
                who: "github:100".into(),
                handle: "octocat".into(),
                name: "The Octocat".into(),
                email: "octo@example.com".into(),
                csrf: TEST_CSRF.into(),
                exp: Utc::now().timestamp() + 3600,
            })
            .expect("test session")
    }

    fn keys(&self, provider_name: &str) -> Option<&ProviderKeys> {
        match provider_name {
            "github" => self.github.as_ref(),
            "google" => self.google.as_ref(),
            _ => None,
        }
    }

    fn redirect_uri(&self, provider_name: &str) -> String {
        format!("{}/auth/{provider_name}/callback", self.origin)
    }

    fn seal<T: Serialize>(&self, body: &T) -> Result<String, String> {
        let json = serde_json::to_vec(body).map_err(|_| "Sign-in did not complete.".to_string())?;
        let payload = base64url(&json);
        let sig = sign(&self.session_secret, &payload)?;
        Ok(format!("{payload}.{sig}"))
    }

    fn open<T: for<'de> Deserialize<'de>>(&self, token: &str) -> Result<T, String> {
        let (payload, sig) = token
            .split_once('.')
            .ok_or_else(|| "Sign-in did not complete.".to_string())?;
        if payload.len() > 4000 || sig.len() > 200 || !verify(&self.session_secret, payload, sig) {
            return Err("Sign-in did not complete.".into());
        }
        let bytes =
            base64url_decode(payload).ok_or_else(|| "Sign-in did not complete.".to_string())?;
        serde_json::from_slice(&bytes).map_err(|_| "Sign-in did not complete.".into())
    }

    fn open_pending(&self, token: &str) -> Result<Pending, String> {
        let body = self.open::<PendingBody>(token)?;
        if body.exp < Utc::now().timestamp() {
            return Err("Sign-in did not complete.".into());
        }
        Ok(Pending {
            provider: body.provider,
            state: body.state,
            verifier: body.verifier,
            next: safe_next(&body.next),
        })
    }
}

pub fn session_cookie_name() -> &'static str {
    SESSION_COOKIE
}

pub fn pending_cookie_name() -> &'static str {
    PENDING_COOKIE
}

pub fn cookie_header(name: &str, value: &str, max_age: i64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{name}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}{secure}")
}

pub fn session_cookie(value: &str, secure: bool) -> String {
    cookie_header(SESSION_COOKIE, value, SESSION_TTL, secure)
}

pub fn pending_cookie(value: &str, secure: bool) -> String {
    cookie_header(PENDING_COOKIE, value, PENDING_TTL, secure)
}

pub fn clear_cookie(name: &str, secure: bool) -> String {
    cookie_header(name, "", 0, secure)
}

fn provider(id: &str, secret: &str) -> Option<ProviderKeys> {
    let id = id.trim();
    let secret = secret.trim();
    if !real_client(id) || !real_client(secret) {
        return None;
    }
    Some(ProviderKeys {
        id: id.to_string(),
        secret: secret.to_string(),
    })
}

fn real_client(value: &str) -> bool {
    value.len() >= 8
        && !value.eq_ignore_ascii_case("replace-me")
        && !value.eq_ignore_ascii_case("changeme")
        && value.chars().all(|ch| ch.is_ascii_graphic() && ch != ' ')
}

fn real_session_secret(value: &str) -> bool {
    value.len() >= 32
        && !value.eq_ignore_ascii_case("replace-me")
        && !value.eq_ignore_ascii_case("changeme")
        && value.chars().all(|ch| ch.is_ascii_graphic())
}

fn valid_origin(origin: &str) -> bool {
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"));
    let Some(rest) = rest else {
        return false;
    };
    !rest.is_empty()
        && !rest.contains('/')
        && !rest.contains('@')
        && !rest.contains('?')
        && !rest.contains('#')
        && !rest.contains('\\')
}

pub fn safe_next(next: &str) -> String {
    let next = next.trim();
    if next.len() > 200
        || !next.starts_with('/')
        || next.starts_with("//")
        || next.contains('\\')
        || next.contains(':')
        || next.chars().any(|ch| ch.is_control())
    {
        return "/".into();
    }
    next.to_string()
}

pub(crate) fn valid_who(who: &str) -> bool {
    let Some((kind, id)) = who.split_once(':') else {
        return false;
    };
    if id.is_empty() || id.len() > 80 {
        return false;
    }
    let id_ok = id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    id_ok
        && match kind {
            "github" => id.chars().all(|ch| ch.is_ascii_digit()),
            "google" => true,
            _ => false,
        }
}

pub fn display_name(raw: &str, fallback: &str) -> String {
    let flat = crate::coord::one_line(raw);
    let name: String = flat.chars().take(80).collect();
    let name = name.trim().to_string();
    if name.is_empty() {
        let fallback = fallback.trim();
        if fallback.is_empty() {
            "reader".into()
        } else {
            fallback.to_string()
        }
    } else {
        name
    }
}

pub fn clean_email(raw: &str) -> String {
    let email = raw.trim().to_lowercase();
    if !(3..=120).contains(&email.len()) {
        return String::new();
    }
    let Some((local, host)) = email.split_once('@') else {
        return String::new();
    };
    if local.is_empty()
        || host.is_empty()
        || !host.contains('.')
        || email.contains(':')
        || !email
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '@' | '.' | '-' | '_' | '+'))
    {
        return String::new();
    }
    email
}

pub fn clean_handle(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            out.push(ch);
        }
        if out.chars().count() == 39 {
            break;
        }
    }
    if out.is_empty() || out.eq_ignore_ascii_case("phext") {
        "reader".into()
    } else {
        out
    }
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url(&digest)
}

fn random_token(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    let mut file =
        std::fs::File::open("/dev/urandom").map_err(|_| "Sign-in did not complete.".to_string())?;
    std::io::Read::read_exact(&mut file, &mut buf)
        .map_err(|_| "Sign-in did not complete.".to_string())?;
    Ok(base64url(&buf))
}

fn sign(secret: &str, payload: &str) -> Result<String, String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| "Sign-in did not complete.".to_string())?;
    mac.update(payload.as_bytes());
    Ok(base64url(&mac.finalize().into_bytes()))
}

fn verify(secret: &str, payload: &str, sig: &str) -> bool {
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(payload.as_bytes());
    let Some(bytes) = base64url_decode(sig) else {
        return false;
    };
    mac.verify_slice(&bytes).is_ok()
}

pub(crate) fn cookie_value(header: &str, name: &str) -> Option<String> {
    for part in header.split(';') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        if key.trim() == name {
            let value = value.trim();
            if value.is_empty() || value.len() > 4500 {
                return None;
            }
            return Some(value.to_string());
        }
    }
    None
}

pub fn pending_token(cookie_header: &str) -> Option<String> {
    cookie_value(cookie_header, PENDING_COOKIE)
}

pub fn cookie_from_headers(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

fn github_identity(
    agent: &ureq::Agent,
    code: &str,
    verifier: &str,
    redirect: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<Identity, String> {
    let body = json!({
        "client_id": client_id,
        "client_secret": client_secret,
        "code": code,
        "redirect_uri": redirect,
        "code_verifier": verifier,
    });
    let token = post_json(
        agent,
        "https://github.com/login/oauth/access_token",
        &body.to_string(),
        "application/json",
    )?;
    let parsed: serde_json::Value =
        serde_json::from_str(&token).map_err(|_| "the provider did not complete sign-in.")?;
    let access = parsed
        .get("access_token")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "the provider did not complete sign-in.".to_string())?;
    let user = agent
        .get("https://api.github.com/user")
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "phext-lanes")
        .set("Authorization", &format!("Bearer {access}"))
        .call()
        .map_err(|_| "the provider did not complete sign-in.")?
        .into_string()
        .map_err(|_| "the provider did not complete sign-in.")?;
    let user: serde_json::Value =
        serde_json::from_str(&user).map_err(|_| "the provider did not complete sign-in.")?;
    let id = user
        .get("id")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| "the provider did not complete sign-in.".to_string())?;
    let login = user
        .get("login")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let handle = clean_handle(login);
    let profile = user
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let email = user
        .get("email")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    Ok(Identity {
        who: format!("github:{id}"),
        handle: handle.clone(),
        name: display_name(profile, &handle),
        email: clean_email(email),
    })
}

fn google_identity(
    agent: &ureq::Agent,
    code: &str,
    verifier: &str,
    redirect: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<Identity, String> {
    let form = format!(
        "code={}&client_id={}&client_secret={}&redirect_uri={}&grant_type=authorization_code&code_verifier={}",
        form_encode(code),
        form_encode(client_id),
        form_encode(client_secret),
        form_encode(redirect),
        form_encode(verifier),
    );
    let token = post_json(
        agent,
        "https://oauth2.googleapis.com/token",
        &form,
        "application/x-www-form-urlencoded",
    )?;
    let parsed: serde_json::Value =
        serde_json::from_str(&token).map_err(|_| "the provider did not complete sign-in.")?;
    let access = parsed
        .get("access_token")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "the provider did not complete sign-in.".to_string())?;
    let user = agent
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .set("Accept", "application/json")
        .set("User-Agent", "phext-lanes")
        .set("Authorization", &format!("Bearer {access}"))
        .call()
        .map_err(|_| "the provider did not complete sign-in.")?
        .into_string()
        .map_err(|_| "the provider did not complete sign-in.")?;
    let user: serde_json::Value =
        serde_json::from_str(&user).map_err(|_| "the provider did not complete sign-in.")?;
    let sub = user
        .get("sub")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let email = user
        .get("email")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let local = email.split('@').next().unwrap_or("");
    let handle = if local.is_empty() {
        clean_handle(
            user.get("name")
                .and_then(|value| value.as_str())
                .unwrap_or(""),
        )
    } else {
        clean_handle(local)
    };
    if !valid_who(&format!("google:{sub}")) {
        return Err("the provider did not complete sign-in.".into());
    }
    let profile = user
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    Ok(Identity {
        who: format!("google:{sub}"),
        handle: handle.clone(),
        name: display_name(profile, &handle),
        email: clean_email(email),
    })
}

fn post_json(
    agent: &ureq::Agent,
    url: &str,
    body: &str,
    content_type: &str,
) -> Result<String, String> {
    agent
        .post(url)
        .set("Accept", "application/json")
        .set("User-Agent", "phext-lanes")
        .set("Content-Type", content_type)
        .send_string(body)
        .map_err(|_| "the provider did not complete sign-in.")?
        .into_string()
        .map_err(|_| "the provider did not complete sign-in.".into())
}

fn form_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let n = ((bytes[index] as u32) << 16)
            | ((bytes[index + 1] as u32) << 8)
            | (bytes[index + 2] as u32);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push(TABLE[(n & 63) as usize] as char);
        index += 3;
    }
    let rest = bytes.len() - index;
    if rest == 1 {
        let n = (bytes[index] as u32) << 16;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
    } else if rest == 2 {
        let n = ((bytes[index] as u32) << 16) | ((bytes[index + 1] as u32) << 8);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
    }
    out
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    fn val(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let bytes = input.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::new();
    let mut index = 0;
    while index + 4 <= bytes.len() {
        let a = val(bytes[index])?;
        let b = val(bytes[index + 1])?;
        let c = val(bytes[index + 2])?;
        let d = val(bytes[index + 3])?;
        let n = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6) | (d as u32);
        out.push((n >> 16) as u8);
        out.push((n >> 8) as u8);
        out.push(n as u8);
        index += 4;
    }
    let rest = bytes.len() - index;
    if rest == 2 {
        let a = val(bytes[index])?;
        let b = val(bytes[index + 1])?;
        let n = ((a as u32) << 18) | ((b as u32) << 12);
        out.push((n >> 16) as u8);
    } else if rest == 3 {
        let a = val(bytes[index])?;
        let b = val(bytes[index + 1])?;
        let c = val(bytes[index + 2])?;
        let n = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6);
        out.push((n >> 16) as u8);
        out.push((n >> 8) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_the_spec_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_tampered_session_does_not_open() {
        let cookie = Oauth::test_session_cookie();
        let oauth = Oauth::test_ready();
        assert!(oauth
            .read_session(&format!("lanes_session={cookie}"))
            .is_some());
        let mut flipped = cookie.clone();
        let end = flipped.pop().unwrap();
        flipped.push(if end == 'A' { 'B' } else { 'A' });
        assert!(oauth
            .read_session(&format!("lanes_session={flipped}"))
            .is_none());
    }

    #[test]
    fn hostile_next_urls_stay_on_the_site() {
        assert_eq!(safe_next("/l/1.2.1/1.1.1"), "/l/1.2.1/1.1.1");
        assert_eq!(safe_next("https://evil.example"), "/");
        assert_eq!(safe_next("//evil.example"), "/");
        assert_eq!(safe_next("/\\evil"), "/");
    }

    #[test]
    fn empty_client_ids_leave_writes_closed() {
        let oauth = Oauth::closed();
        assert!(!oauth.ready());
        assert!(oauth.providers().is_empty());
        assert!(oauth.authorize_url("github", "/").is_err());
    }

    #[test]
    fn handles_drop_punctuation_and_the_plain_author_name() {
        assert_eq!(clean_handle("Ada North"), "AdaNorth");
        assert_eq!(clean_handle("phext"), "reader");
        assert_eq!(clean_handle("octocat"), "octocat");
    }

    #[test]
    fn profile_names_keep_spaces_and_email_is_lowercase() {
        assert_eq!(display_name("Stewart Alsop", "stewart"), "Stewart Alsop");
        assert_eq!(display_name("  ", "stewart"), "stewart");
        assert_eq!(clean_email("Stewart@Example.com"), "stewart@example.com");
        assert!(clean_email("not an email").is_empty());
        let session = Oauth::test_ready()
            .read_session(&format!("lanes_session={}", Oauth::test_session_cookie()))
            .unwrap();
        assert_eq!(session.name, "The Octocat");
        assert_eq!(session.email, "octo@example.com");
    }
}
