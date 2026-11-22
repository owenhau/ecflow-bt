//! Settings: `--flag` > `EF_*` environment variable > `~/.config/ecflow-bt/config.toml`.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub user_id: Option<String>,
    pub serial: Option<String>,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Overrides {
    pub user_id: Option<String>,
    pub serial: Option<String>,
    pub timeout_secs: Option<u64>,
}

impl Overrides {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Self {
            user_id: var("EF_USER_ID"),
            serial: var("EF_SERIAL"),
            timeout_secs: var("EF_TIMEOUT").and_then(|v| v.trim().parse().ok()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub user_id: Option<String>,
    pub serial: Option<String>,
    pub timeout: Duration,
}

pub fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "ecflow-bt").map(|d| d.config_dir().join("config.toml"))
}

/// A missing file is fine (defaults); a malformed one is an error that names the file.
pub fn load_file(path: &Path) -> Result<FileConfig> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("invalid config file {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileConfig::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read config file {}", path.display())),
    }
}

fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

pub fn resolve(flags: Overrides, env: Overrides, file: FileConfig) -> Result<Settings> {
    let user_id = clean(flags.user_id.or(env.user_id).or(file.user_id));
    let serial = clean(flags.serial.or(env.serial).or(file.serial)).map(|s| s.to_ascii_uppercase());
    let timeout_secs = flags
        .timeout_secs
        .or(env.timeout_secs)
        .or(file.timeout_secs)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    if let Some(id) = &user_id
        && !id.is_ascii()
    {
        bail!("user ID must be ASCII");
    }
    if let Some(sn) = &serial
        && !(sn.is_ascii() && sn.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        bail!("serial number must be alphanumeric ASCII");
    }
    if timeout_secs == 0 || timeout_secs > 600 {
        bail!("timeout must be between 1 and 600 seconds");
    }
    Ok(Settings {
        user_id,
        serial,
        timeout: Duration::from_secs(timeout_secs),
    })
}

/// `12••••90` style masking for display and logs.
pub fn mask(s: &str) -> String {
    let n = s.chars().count();
    if n <= 4 {
        return "*".repeat(n);
    }
    let head: String = s.chars().take(2).collect();
    let tail: String = s.chars().skip(n - 2).collect();
    format!("{head}{}{tail}", "*".repeat(n - 4))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(user: Option<&str>) -> Overrides {
        Overrides {
            user_id: user.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn precedence_flag_then_env_then_file() {
        let file = || FileConfig {
            user_id: Some("file".into()),
            serial: Some("r631abc".into()),
            timeout_secs: Some(30),
        };
        let s = resolve(o(Some("flag")), o(Some("env")), file()).unwrap();
        assert_eq!(s.user_id.as_deref(), Some("flag"));
        let s = resolve(o(None), o(Some("env")), file()).unwrap();
        assert_eq!(s.user_id.as_deref(), Some("env"));
        let s = resolve(o(None), o(None), file()).unwrap();
        assert_eq!(s.user_id.as_deref(), Some("file"));
        assert_eq!(s.serial.as_deref(), Some("R631ABC")); // normalized
        assert_eq!(s.timeout, Duration::from_secs(30));
    }

    #[test]
    fn defaults_when_nothing_set() {
        let s = resolve(o(None), o(None), FileConfig::default()).unwrap();
        assert_eq!(s.user_id, None);
        assert_eq!(s.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    }

    #[test]
    fn blank_values_are_ignored() {
        let s = resolve(
            o(Some("  ")),
            o(None),
            FileConfig {
                user_id: Some("x".into()),
                ..Default::default()
            },
        );
        // A blank flag wins over the file but is treated as unset.
        assert_eq!(s.unwrap().user_id, None);
    }

    #[test]
    fn validation_errors() {
        assert!(resolve(o(Some("ü")), o(None), FileConfig::default()).is_err());
        let bad_sn = Overrides {
            serial: Some("R63-1".into()),
            ..Default::default()
        };
        assert!(resolve(bad_sn, o(None), FileConfig::default()).is_err());
        let bad_t = Overrides {
            timeout_secs: Some(0),
            ..Default::default()
        };
        assert!(resolve(bad_t, o(None), FileConfig::default()).is_err());
    }

    #[test]
    fn missing_file_is_default_and_malformed_is_error() {
        let dir = std::env::temp_dir().join(format!("ecflow-bt-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load_file(&dir.join("nope.toml")).unwrap(), FileConfig::default());

        let good = dir.join("good.toml");
        std::fs::write(&good, "user_id = \"123\"\nserial = \"R631X\"\n").unwrap();
        assert_eq!(load_file(&good).unwrap().user_id.as_deref(), Some("123"));

        let bad = dir.join("bad.toml");
        std::fs::write(&bad, "user_id = = 5").unwrap();
        let err = format!("{:#}", load_file(&bad).unwrap_err());
        assert!(err.contains("bad.toml"), "{err}");

        let unknown = dir.join("unknown.toml");
        std::fs::write(&unknown, "userid = \"typo\"").unwrap();
        assert!(load_file(&unknown).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn masking() {
        assert_eq!(mask("1234567890"), "12******90");
        assert_eq!(mask("abc"), "***");
        assert_eq!(mask(""), "");
    }
}
