//! Where the reader name, the phext path, and the limits come from.
//!
//! A flag wins, then the environment, then `lanes.json` in the working
//! directory, then the built-in default. Limits have no flags: set them in
//! the environment (`LANES_LOUD_AT`, ...) or under `"limits"` in `lanes.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;

pub const DEFAULT_USER: &str = "mira";
pub const DEFAULT_FILE: &str = "data/chat.phext";
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 7841;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub user: String,
    pub file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listen {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    user: Option<String>,
    file: Option<String>,
    #[serde(default)]
    limits: Option<FileLimits>,
}

/// How much the feed shows and how fast people may write.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Limits {
    /// An author with at least this many messages is loud. Only loud
    /// authors are capped. `LANES_LOUD_AT`
    pub loud_at: usize,
    /// Lines a loud author may hold in another reader's feed. `LANES_AUTHOR_CAP`
    pub author_cap: usize,
    /// Ranked threads in a feed. The rest are listed without ranking.
    /// `LANES_MAX_THREADS`
    pub max_threads: usize,
    /// A thread needs this many messages before it can be flooded.
    /// `LANES_FLOOD_MIN_MESSAGES`
    pub flood_min_messages: usize,
    /// One author's share of a thread, 0.0 to 1.0, that marks it flooded.
    /// `LANES_FLOOD_SHARE`
    pub flood_share: f64,
    /// Messages per page when a thread opens. `LANES_PAGE_SIZE`
    pub page_size: usize,
    /// Sign-in attempts per address per ten minutes. `LANES_AUTH_PER_IP`
    pub auth_per_ip: usize,
    /// Writes per address per ten minutes. `LANES_WRITES_PER_IP`
    pub writes_per_ip: usize,
    /// Writes per signed-in account per ten minutes. `LANES_WRITES_PER_ACCOUNT`
    pub writes_per_account: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            loud_at: 8,
            author_cap: 2,
            max_threads: 8,
            flood_min_messages: 10,
            flood_share: 0.70,
            page_size: 40,
            auth_per_ip: 20,
            writes_per_ip: 40,
            writes_per_account: 30,
        }
    }
}

impl Limits {
    /// True when one author wrote enough of a large enough thread.
    pub fn is_flooded(&self, count: usize, share: f64) -> bool {
        count >= self.flood_min_messages && share >= self.flood_share
    }

    /// The first value out of range, as `(key, allowed range, value)`.
    pub fn check(&self) -> Result<(), (&'static str, &'static str, String)> {
        let counts: [(&'static str, usize, usize, &'static str); 8] = [
            ("loud_at", self.loud_at, 1_000_000, "1..=1000000"),
            ("author_cap", self.author_cap, 1000, "1..=1000"),
            ("max_threads", self.max_threads, 1000, "1..=1000"),
            (
                "flood_min_messages",
                self.flood_min_messages,
                1_000_000,
                "1..=1000000",
            ),
            ("page_size", self.page_size, 1000, "1..=1000"),
            ("auth_per_ip", self.auth_per_ip, 100_000, "1..=100000"),
            ("writes_per_ip", self.writes_per_ip, 100_000, "1..=100000"),
            (
                "writes_per_account",
                self.writes_per_account,
                100_000,
                "1..=100000",
            ),
        ];
        for (key, value, max, range) in counts {
            if !(1..=max).contains(&value) {
                return Err((key, range, value.to_string()));
            }
        }
        if !(0.0..=1.0).contains(&self.flood_share) {
            return Err(("flood_share", "0.0..=1.0", self.flood_share.to_string()));
        }
        Ok(())
    }
}

/// Feed limits one lane overrides on top of the server's [`Limits`].
/// Rate limits are not here: they protect the whole server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitOverrides {
    pub loud_at: Option<usize>,
    pub author_cap: Option<usize>,
    pub max_threads: Option<usize>,
    pub flood_min_messages: Option<usize>,
    pub flood_share: Option<f64>,
    pub page_size: Option<usize>,
}

impl LimitOverrides {
    pub const KEYS: [&'static str; 6] = [
        "loud_at",
        "author_cap",
        "max_threads",
        "flood_min_messages",
        "flood_share",
        "page_size",
    ];

    pub fn apply(&self, base: Limits) -> Limits {
        Limits {
            loud_at: self.loud_at.unwrap_or(base.loud_at),
            author_cap: self.author_cap.unwrap_or(base.author_cap),
            max_threads: self.max_threads.unwrap_or(base.max_threads),
            flood_min_messages: self.flood_min_messages.unwrap_or(base.flood_min_messages),
            flood_share: self.flood_share.unwrap_or(base.flood_share),
            page_size: self.page_size.unwrap_or(base.page_size),
            ..base
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == LimitOverrides::default()
    }

    /// `key: value` lines for the ones that are set.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        let counts = [
            ("loud_at", self.loud_at),
            ("author_cap", self.author_cap),
            ("max_threads", self.max_threads),
            ("flood_min_messages", self.flood_min_messages),
        ];
        for (key, value) in counts {
            if let Some(value) = value {
                out.push(format!("{key}: {value}"));
            }
        }
        if let Some(share) = self.flood_share {
            out.push(format!("flood_share: {share}"));
        }
        if let Some(page) = self.page_size {
            out.push(format!("page_size: {page}"));
        }
        out
    }

    /// Read one `key: value` line. Unknown keys and bad numbers are skipped,
    /// so a hand-edited scroll never breaks the lane.
    pub fn read(&mut self, key: &str, value: &str) {
        let value = value.trim();
        match key {
            "loud_at" => self.loud_at = value.parse().ok(),
            "author_cap" => self.author_cap = value.parse().ok(),
            "max_threads" => self.max_threads = value.parse().ok(),
            "flood_min_messages" => self.flood_min_messages = value.parse().ok(),
            "flood_share" => self.flood_share = value.parse().ok(),
            "page_size" => self.page_size = value.parse().ok(),
            _ => {}
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLimits {
    loud_at: Option<usize>,
    author_cap: Option<usize>,
    max_threads: Option<usize>,
    flood_min_messages: Option<usize>,
    flood_share: Option<f64>,
    page_size: Option<usize>,
    auth_per_ip: Option<usize>,
    writes_per_ip: Option<usize>,
    writes_per_account: Option<usize>,
}

/// Limits from the environment and `lanes.json`, checked for range.
pub fn load_limits() -> Result<Limits, Error> {
    let path = Path::new("lanes.json");
    let config = if path.is_file() {
        Some(std::fs::read_to_string(path)?)
    } else {
        None
    };
    resolve_limits(|name| std::env::var(name).ok(), config.as_deref())
}

pub fn resolve_limits(
    env: impl Fn(&str) -> Option<String>,
    config_json: Option<&str>,
) -> Result<Limits, Error> {
    let file = match config_json {
        Some(json) => serde_json::from_str::<FileConfig>(json)
            .map_err(|err| Error::Bad(format!("lanes.json: {err}")))?
            .limits
            .unwrap_or_default(),
        None => FileLimits::default(),
    };
    let base = Limits::default();
    let read = |name: &str| {
        env(name)
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty())
    };
    let count = |name: &str, file: Option<usize>, default: usize| -> Result<usize, Error> {
        match read(name) {
            Some(raw) => raw
                .parse::<usize>()
                .map_err(|_| Error::Bad(format!("{name} is not a whole number: {raw}"))),
            None => Ok(file.unwrap_or(default)),
        }
    };
    let flood_share = match read("LANES_FLOOD_SHARE") {
        Some(raw) => raw
            .parse::<f64>()
            .map_err(|_| Error::Bad(format!("LANES_FLOOD_SHARE is not a number: {raw}")))?,
        None => file.flood_share.unwrap_or(base.flood_share),
    };
    let limits = Limits {
        loud_at: count("LANES_LOUD_AT", file.loud_at, base.loud_at)?,
        author_cap: count("LANES_AUTHOR_CAP", file.author_cap, base.author_cap)?,
        max_threads: count("LANES_MAX_THREADS", file.max_threads, base.max_threads)?,
        flood_min_messages: count(
            "LANES_FLOOD_MIN_MESSAGES",
            file.flood_min_messages,
            base.flood_min_messages,
        )?,
        flood_share,
        page_size: count("LANES_PAGE_SIZE", file.page_size, base.page_size)?,
        auth_per_ip: count("LANES_AUTH_PER_IP", file.auth_per_ip, base.auth_per_ip)?,
        writes_per_ip: count(
            "LANES_WRITES_PER_IP",
            file.writes_per_ip,
            base.writes_per_ip,
        )?,
        writes_per_account: count(
            "LANES_WRITES_PER_ACCOUNT",
            file.writes_per_account,
            base.writes_per_account,
        )?,
    };
    limits.check().map_err(|(key, range, value)| {
        Error::Bad(format!(
            "LANES_{} (limits.{key}) must be {range}, got {value}",
            key.to_uppercase()
        ))
    })?;
    Ok(limits)
}

pub fn load(flag_user: Option<&str>, flag_file: Option<&Path>) -> Result<Settings, Error> {
    let env_user = std::env::var("LANES_USER").ok();
    let env_file = std::env::var("LANES_FILE").ok();
    let path = Path::new("lanes.json");
    let config = if path.is_file() {
        Some(std::fs::read_to_string(path)?)
    } else {
        None
    };
    resolve(
        flag_user,
        flag_file,
        env_user.as_deref(),
        env_file.as_deref(),
        config.as_deref(),
    )
}

pub fn resolve(
    flag_user: Option<&str>,
    flag_file: Option<&Path>,
    env_user: Option<&str>,
    env_file: Option<&str>,
    config_json: Option<&str>,
) -> Result<Settings, Error> {
    let file_config = match config_json {
        Some(json) => Some(
            serde_json::from_str::<FileConfig>(json)
                .map_err(|err| Error::Bad(format!("lanes.json: {err}")))?,
        ),
        None => None,
    };
    let user = first_text([
        flag_user,
        env_user,
        file_config
            .as_ref()
            .and_then(|config| config.user.as_deref()),
    ])
    .unwrap_or_else(|| DEFAULT_USER.to_string());
    let file = first_text([
        flag_file.and_then(|path| path.to_str()),
        env_file,
        file_config
            .as_ref()
            .and_then(|config| config.file.as_deref()),
    ])
    .unwrap_or_else(|| DEFAULT_FILE.to_string());
    Ok(Settings {
        user,
        file: PathBuf::from(file),
    })
}

pub fn load_listen(flag_host: Option<&str>, flag_port: Option<u16>) -> Result<Listen, Error> {
    let env_host = std::env::var("LANES_HOST").ok();
    let env_port = std::env::var("LANES_PORT").ok();
    resolve_listen(
        flag_host,
        flag_port,
        env_host.as_deref(),
        env_port.as_deref(),
    )
}

pub fn resolve_listen(
    flag_host: Option<&str>,
    flag_port: Option<u16>,
    env_host: Option<&str>,
    env_port: Option<&str>,
) -> Result<Listen, Error> {
    let host = first_text([flag_host, env_host]).unwrap_or_else(|| DEFAULT_HOST.to_string());
    let port = if let Some(port) = flag_port {
        port
    } else if let Some(raw) = first_text([env_port]) {
        raw.parse::<u16>()
            .map_err(|_| Error::Bad(format!("LANES_PORT is not a port: {raw}")))?
    } else {
        DEFAULT_PORT
    };
    if port == 0 {
        return Err(Error::Bad("listen port must be 1..=65535".into()));
    }
    Ok(Listen { host, port })
}

fn first_text<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    values.into_iter().flatten().find_map(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_beats_env_and_config() {
        let settings = resolve(
            Some("casey"),
            Some(Path::new("flag.phext")),
            Some("ren"),
            Some("env.phext"),
            Some(r#"{"user":"ada","file":"cfg.phext"}"#),
        )
        .unwrap();
        assert_eq!(settings.user, "casey");
        assert_eq!(settings.file, PathBuf::from("flag.phext"));
    }

    #[test]
    fn env_beats_config() {
        let settings = resolve(
            None,
            None,
            Some("ren"),
            Some("env.phext"),
            Some(r#"{"user":"ada","file":"cfg.phext"}"#),
        )
        .unwrap();
        assert_eq!(settings.user, "ren");
        assert_eq!(settings.file, PathBuf::from("env.phext"));
    }

    #[test]
    fn config_beats_the_built_in_default() {
        let settings = resolve(
            None,
            None,
            None,
            None,
            Some(r#"{"user":"ada","file":"notes/chat.phext"}"#),
        )
        .unwrap();
        assert_eq!(settings.user, "ada");
        assert_eq!(settings.file, PathBuf::from("notes/chat.phext"));
    }

    #[test]
    fn blanks_fall_through_to_mira_and_the_default_file() {
        let settings = resolve(
            Some("  "),
            None,
            Some(""),
            None,
            Some(r#"{"user":"","file":""}"#),
        )
        .unwrap();
        assert_eq!(settings.user, DEFAULT_USER);
        assert_eq!(settings.file, PathBuf::from(DEFAULT_FILE));
    }

    #[test]
    fn listen_flag_beats_env_and_defaults_to_localhost() {
        let listen =
            resolve_listen(Some("0.0.0.0"), Some(9), Some("10.0.0.1"), Some("80")).unwrap();
        assert_eq!(
            listen,
            Listen {
                host: "0.0.0.0".into(),
                port: 9
            }
        );
        let from_env = resolve_listen(None, None, Some("127.0.0.1"), Some("7841")).unwrap();
        assert_eq!(from_env.host, "127.0.0.1");
        assert_eq!(from_env.port, 7841);
        let defaults = resolve_listen(None, None, Some("  "), Some("")).unwrap();
        assert_eq!(defaults.host, DEFAULT_HOST);
        assert_eq!(defaults.port, DEFAULT_PORT);
        let bad = resolve_listen(None, None, None, Some("nope")).unwrap_err();
        assert!(bad.to_string().contains("LANES_PORT"), "{bad}");
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn limits_default_without_env_or_file() {
        assert_eq!(
            resolve_limits(env_of(&[]), None).unwrap(),
            Limits::default()
        );
        let only_user = resolve_limits(env_of(&[]), Some(r#"{"user":"ada"}"#)).unwrap();
        assert_eq!(only_user, Limits::default());
    }

    #[test]
    fn limits_env_beats_file_beats_default() {
        let json = r#"{"limits":{"loud_at":3,"author_cap":1,"max_threads":20,"flood_share":0.5}}"#;
        let limits = resolve_limits(
            env_of(&[
                ("LANES_MAX_THREADS", "50"),
                ("LANES_WRITES_PER_ACCOUNT", " 5 "),
            ]),
            Some(json),
        )
        .unwrap();
        assert_eq!(limits.loud_at, 3);
        assert_eq!(limits.author_cap, 1);
        assert_eq!(limits.max_threads, 50);
        assert_eq!(limits.flood_share, 0.5);
        assert_eq!(limits.writes_per_account, 5);
        assert_eq!(limits.page_size, Limits::default().page_size);
        let blank = resolve_limits(env_of(&[("LANES_MAX_THREADS", "  ")]), Some(json)).unwrap();
        assert_eq!(blank.max_threads, 20);
    }

    #[test]
    fn limits_out_of_range_or_misspelled_are_errors() {
        for (name, value) in [
            ("LANES_AUTHOR_CAP", "0"),
            ("LANES_MAX_THREADS", "1001"),
            ("LANES_PAGE_SIZE", "-1"),
            ("LANES_LOUD_AT", "many"),
            ("LANES_FLOOD_SHARE", "1.5"),
        ] {
            let err = resolve_limits(env_of(&[(name, value)]), None).unwrap_err();
            assert!(err.to_string().contains(name), "{name}: {err}");
        }
        let typo = resolve_limits(env_of(&[]), Some(r#"{"limits":{"loud_after":3}}"#)).unwrap_err();
        assert!(typo.to_string().contains("loud_after"), "{typo}");
    }

    #[test]
    fn overrides_apply_only_feed_limits_and_roundtrip_as_lines() {
        let base = Limits {
            writes_per_ip: 7,
            ..Limits::default()
        };
        let mut over = LimitOverrides {
            loud_at: Some(3),
            flood_share: Some(0.5),
            ..LimitOverrides::default()
        };
        let applied = over.apply(base);
        assert_eq!(applied.loud_at, 3);
        assert_eq!(applied.flood_share, 0.5);
        assert_eq!(applied.author_cap, base.author_cap);
        assert_eq!(applied.writes_per_ip, 7);
        assert_eq!(over.lines(), vec!["loud_at: 3", "flood_share: 0.5"]);
        let mut back = LimitOverrides::default();
        for line in over.lines() {
            let (key, value) = line.split_once(':').unwrap();
            back.read(key, value);
        }
        assert_eq!(back, over);
        over.read("max_threads", "lots");
        assert_eq!(over.max_threads, None);
        assert!(LimitOverrides::default().is_empty());
    }

    #[test]
    fn flooded_follows_the_limits() {
        let limits = Limits::default();
        assert!(limits.is_flooded(10, 0.7));
        assert!(!limits.is_flooded(9, 1.0));
        let strict = Limits {
            flood_min_messages: 3,
            flood_share: 0.5,
            ..Limits::default()
        };
        assert!(strict.is_flooded(3, 0.5));
    }

    #[test]
    fn a_broken_config_file_is_an_error() {
        let err = resolve(None, None, None, None, Some("not json")).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("lanes.json"), "{message}");
    }
}
