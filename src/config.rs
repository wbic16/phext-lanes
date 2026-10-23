//! Where the reader name and the phext path come from.
//!
//! A flag wins, then the environment, then `lanes.json` in the working
//! directory, then the built-in default.

use std::path::{Path, PathBuf};

use serde::Deserialize;

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

    #[test]
    fn a_broken_config_file_is_an_error() {
        let err = resolve(None, None, None, None, Some("not json")).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("lanes.json"), "{message}");
    }
}
