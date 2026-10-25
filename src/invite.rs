//! Invite-only membership for the default fabric.
//!
//! Owners come from `LANES_OWNERS` (comma-separated `provider:id` or email).
//! Invite tokens are random, shown once, and stored only as a SHA-256 hash
//! in a mode `0600` file beside the phext. The file is never in git.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::token_eq;
use crate::error::Error;

pub const JOIN_COOKIE: &str = "lanes_join";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    Who(String),
    Email(String),
}

#[derive(Clone, Default)]
pub struct Owners {
    entries: Vec<Owner>,
}

impl Owners {
    pub fn from_env() -> Self {
        match std::env::var("LANES_OWNERS") {
            Ok(raw) => Self::parse(&raw),
            Err(_) => Self::default(),
        }
    }

    pub fn parse(raw: &str) -> Self {
        let mut entries = Vec::new();
        for part in raw.split(',') {
            let part = part.trim();
            if part.is_empty() || part.len() > 120 {
                continue;
            }
            if part.contains('@') {
                let email = part.to_lowercase();
                if email_shape(&email) {
                    entries.push(Owner::Email(email));
                }
                continue;
            }
            if crate::oauth::valid_who(part) {
                entries.push(Owner::Who(part.to_string()));
            }
        }
        Self { entries }
    }

    pub fn invite_only(&self) -> bool {
        !self.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn matches(&self, who: &str, email: &str) -> bool {
        let email = email.trim().to_lowercase();
        self.entries.iter().any(|entry| match entry {
            Owner::Who(id) => id == who,
            Owner::Email(expected) => !email.is_empty() && expected == &email,
        })
    }
}

fn email_shape(email: &str) -> bool {
    let Some((local, host)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !host.is_empty()
        && host.contains('.')
        && !email.contains(':')
        && email
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '@' | '.' | '-' | '_' | '+'))
}

/// Default invite file: next to the phext, not inside the checkout when the
/// phext lives in `/var/lib/phext-lanes`.
pub fn beside(phext: &Path) -> PathBuf {
    match phext.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join("invites.json"),
        _ => PathBuf::from("invites.json"),
    }
}

/// `LANES_INVITES` wins. Otherwise [`beside`].
pub fn resolve(phext: &Path) -> PathBuf {
    match std::env::var("LANES_INVITES") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value.trim()),
        _ => beside(phext),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issued {
    pub id: String,
    pub token: String,
    pub uses: u32,
    pub exp: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    pub uses: u32,
    pub remaining: u32,
    pub exp: Option<DateTime<Utc>>,
    pub revoked: bool,
    pub created: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedeemFail {
    Invalid,
    Expired,
    Revoked,
    Spent,
}

impl RedeemFail {
    pub fn message(self) -> &'static str {
        match self {
            Self::Invalid => "That invite is no longer valid.",
            Self::Expired => "That invite has expired.",
            Self::Revoked => "That invite was revoked.",
            Self::Spent => "That invite has already been used.",
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Book {
    invites: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    id: String,
    hash: String,
    uses: u32,
    remaining: u32,
    exp: i64,
    created: i64,
    revoked: bool,
}

pub fn create(
    path: &Path,
    uses: u32,
    exp: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<Issued, Error> {
    if uses > 10_000 {
        return Err(Error::Bad("an invite can allow at most 10000 uses".into()));
    }
    let token = random_hex(32)?;
    let id = random_hex(8)?;
    let hash = hash_token(&token);
    let exp_unix = exp.map(|when| when.timestamp()).unwrap_or(0);
    with_locked(path, true, |book| {
        book.invites.push(Record {
            id: id.clone(),
            hash,
            uses,
            remaining: uses,
            exp: exp_unix,
            created: now.timestamp(),
            revoked: false,
        });
        Ok(())
    })?;
    Ok(Issued {
        id,
        token,
        uses,
        exp,
    })
}

pub fn list(path: &Path) -> Result<Vec<Listed>, Error> {
    with_locked(path, false, |book| {
        let mut rows = Vec::new();
        for record in book.invites.iter().rev() {
            rows.push(Listed {
                id: record.id.clone(),
                uses: record.uses,
                remaining: record.remaining,
                exp: stamp(record.exp),
                revoked: record.revoked,
                created: stamp(record.created).unwrap_or_else(Utc::now),
            });
        }
        Ok(rows)
    })
}

pub fn revoke(path: &Path, id: &str) -> Result<(), Error> {
    let id = id.trim();
    if id.is_empty() {
        return Err(Error::Bad("name the invite id to revoke".into()));
    }
    with_locked(path, true, |book| {
        let Some(record) = book.invites.iter_mut().find(|record| record.id == id) else {
            return Err(Error::Bad(format!("no invite {id}")));
        };
        record.revoked = true;
        Ok(())
    })
}

pub fn redeem(path: &Path, token: &str, now: DateTime<Utc>) -> Result<(), RedeemFail> {
    if !token_shape(token) {
        return Err(RedeemFail::Invalid);
    }
    let want = hash_token(token);
    with_locked(path, true, |book| {
        let mut index = None;
        for (place, record) in book.invites.iter().enumerate() {
            if token_eq(&record.hash, &want) {
                index = Some(place);
            }
        }
        let Some(index) = index else {
            return Err(Error::Bad(RedeemFail::Invalid.message().into()));
        };
        let record = &mut book.invites[index];
        if record.revoked {
            return Err(Error::Bad(RedeemFail::Revoked.message().into()));
        }
        if record.exp > 0 && record.exp < now.timestamp() {
            return Err(Error::Bad(RedeemFail::Expired.message().into()));
        }
        if record.uses > 0 {
            if record.remaining == 0 {
                return Err(Error::Bad(RedeemFail::Spent.message().into()));
            }
            record.remaining -= 1;
        }
        Ok(())
    })
    .map_err(|err| fail_from(&err))
}

pub fn token_shape(token: &str) -> bool {
    token.len() == 64
        && token
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
}

fn fail_from(err: &Error) -> RedeemFail {
    let message = err.to_string();
    for fail in [
        RedeemFail::Expired,
        RedeemFail::Revoked,
        RedeemFail::Spent,
        RedeemFail::Invalid,
    ] {
        if message == fail.message() {
            return fail;
        }
    }
    RedeemFail::Invalid
}

fn stamp(unix: i64) -> Option<DateTime<Utc>> {
    if unix <= 0 {
        None
    } else {
        DateTime::from_timestamp(unix, 0)
    }
}

fn hash_token(token: &str) -> String {
    hex_encode(&Sha256::digest(token.as_bytes()))
}

fn random_hex(bytes: usize) -> Result<String, Error> {
    let mut buf = vec![0u8; bytes];
    let mut file = fs::File::open("/dev/urandom")?;
    file.read_exact(&mut buf)?;
    Ok(hex_encode(&buf))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn with_locked<T>(
    path: &Path,
    mutate: bool,
    body: impl FnOnce(&mut Book) -> Result<T, Error>,
) -> Result<T, Error> {
    if !path.exists() && !mutate {
        let mut book = Book::default();
        return body(&mut book);
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut file = open_private(path)?;
    let _guard = lock_exclusive(&file)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let mut book = if text.trim().is_empty() {
        Book::default()
    } else {
        serde_json::from_str(&text).map_err(|err| Error::Bad(format!("invites file: {err}")))?
    };
    let result = body(&mut book)?;
    if mutate {
        let encoded = serde_json::to_vec_pretty(&book)
            .map_err(|err| Error::Bad(format!("invites file: {err}")))?;
        file.seek(SeekFrom::Start(0))?;
        file.set_len(0)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        tighten(path)?;
    }
    Ok(result)
}

fn open_private(path: &Path) -> Result<fs::File, Error> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    tighten(path)?;
    Ok(file)
}

fn tighten(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

struct LockGuard {
    fd: i32,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: fd came from the locked invite file. LOCK_UN releases it.
        unsafe {
            flock(self.fd, 8);
        }
    }
}

fn lock_exclusive(file: &fs::File) -> Result<LockGuard, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = file.as_raw_fd();
        // SAFETY: fd is a live file descriptor owned by `file` for this call.
        // LOCK_EX waits until this process holds the invite file.
        let rc = unsafe { flock(fd, 2) };
        if rc != 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        Ok(LockGuard { fd })
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(LockGuard { fd: -1 })
    }
}

#[cfg(unix)]
extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("lanes-invite-{}-{id}.json", std::process::id()))
    }

    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn owners_match_provider_id_or_email_and_blank_stays_open() {
        let open = Owners::parse("  , , ");
        assert!(!open.invite_only());
        assert!(!open.matches("github:100", "a@b.co"));

        let owners = Owners::parse("github:100, Stewart@Example.com , not-an-id");
        assert!(owners.invite_only());
        assert_eq!(owners.len(), 2);
        assert!(owners.matches("github:100", ""));
        assert!(!owners.matches("github:101", ""));
        assert!(owners.matches("github:101", "stewart@example.com"));
        assert!(owners.matches("google:abc", "Stewart@Example.com"));
        assert!(!owners.matches("google:abc", ""));
    }

    #[test]
    fn create_stores_a_hash_and_a_private_file() {
        let path = scratch();
        let _tmp = Temp(path.clone());
        let issued = create(&path, 1, None, now()).unwrap();
        assert_eq!(issued.token.len(), 64);
        assert_ne!(issued.id, issued.token);
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains(&issued.token), "token must not be stored");
        assert!(raw.contains(&issued.id));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "mode {mode:o}");
        }
        let again = create(&path, 1, None, now()).unwrap();
        assert_ne!(again.token, issued.token);
    }

    #[test]
    fn single_use_expires_revokes_and_multi_use_counts_down() {
        let path = scratch();
        let _tmp = Temp(path.clone());
        let once = create(&path, 1, None, now()).unwrap();
        assert!(redeem(&path, &once.token, now()).is_ok());
        assert_eq!(
            redeem(&path, &once.token, now()).unwrap_err(),
            RedeemFail::Spent
        );

        let many = create(&path, 2, Some(now() + Duration::days(3)), now()).unwrap();
        assert!(redeem(&path, &many.token, now()).is_ok());
        assert!(redeem(&path, &many.token, now()).is_ok());
        assert_eq!(
            redeem(&path, &many.token, now()).unwrap_err(),
            RedeemFail::Spent
        );

        let forever = create(&path, 0, None, now()).unwrap();
        assert!(redeem(&path, &forever.token, now()).is_ok());
        assert!(redeem(&path, &forever.token, now()).is_ok());
        assert!(redeem(&path, &forever.token, now()).is_ok());

        let expiring = create(&path, 1, Some(now() + Duration::hours(1)), now()).unwrap();
        assert_eq!(
            redeem(&path, &expiring.token, now() + Duration::hours(2)).unwrap_err(),
            RedeemFail::Expired
        );
        assert!(redeem(&path, &expiring.token, now()).is_ok());

        let doomed = create(&path, 3, None, now()).unwrap();
        revoke(&path, &doomed.id).unwrap();
        assert_eq!(
            redeem(&path, &doomed.token, now()).unwrap_err(),
            RedeemFail::Revoked
        );
        assert!(revoke(&path, "missing").is_err());

        assert_eq!(
            redeem(&path, &"ab".repeat(32), now()).unwrap_err(),
            RedeemFail::Invalid
        );

        let listed = list(&path).unwrap();
        assert!(listed
            .iter()
            .any(|row| row.id == once.id && row.remaining == 0));
        assert!(listed.iter().any(|row| row.id == doomed.id && row.revoked));
        assert!(listed
            .iter()
            .any(|row| row.id == forever.id && row.uses == 0));
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains(&once.token));
        assert!(!raw.contains(&many.token));
        assert!(!raw.contains(&forever.token));
    }
}
