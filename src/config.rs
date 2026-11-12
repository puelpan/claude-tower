//! Optional `~/.config/claude-tower/config.toml` (§11).

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::model::GroupBy;

/// The main view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// A grid of cards, most urgent first.
    #[default]
    Cards,
    /// The grouped list with the details panel.
    List,
}

/// Lowest accepted `refresh_ms`.
const MIN_REFRESH_MS: u64 = 200;

/// Every key has a default; unknown keys are an error so typos show up.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Collector period. Default 1000; values under 200 are raised to 200.
    pub refresh_ms: u64,
    pub group_by: GroupBy,
    pub details_panel: bool,
    /// Lines of the selected pane shown in the details panel. Default 15.
    pub preview_lines: u32,
    /// `cards` or `list`; `v` switches while running.
    pub layout: Layout,
    /// Claude's folder; `~` is expanded. Default: `$CLAUDE_CONFIG_DIR`,
    /// then `~/.claude`.
    pub claude_dir: Option<String>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            refresh_ms: 1000,
            group_by: GroupBy::Tmux,
            details_panel: true,
            preview_lines: 15,
            layout: Layout::Cards,
            claude_dir: None,
        }
    }
}

impl Config {
    /// Loads `config_path()`; a missing file gives the defaults.
    pub fn load() -> anyhow::Result<Config> {
        Config::load_from(&config_path())
    }

    /// Loads `path`; a missing file gives the defaults.
    pub fn load_from(path: &Path) -> anyhow::Result<Config> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        parse(&text).with_context(|| format!("in {}", path.display()))
    }

    /// Claude's folder (§3.1): `claude_dir`, else `$CLAUDE_CONFIG_DIR`,
    /// else `~/.claude`.
    pub fn claude_dir(&self) -> PathBuf {
        resolve_claude_dir(
            self.claude_dir.as_deref(),
            std::env::var_os("CLAUDE_CONFIG_DIR"),
            home().as_deref(),
        )
    }
}

/// Parses a config file's text and applies the limits.
fn parse(text: &str) -> Result<Config, toml::de::Error> {
    let mut config: Config = toml::from_str(text)?;
    config.refresh_ms = config.refresh_ms.max(MIN_REFRESH_MS);
    Ok(config)
}

/// `$HOME` if set and not empty, else the passwd entry's home.
fn home() -> Option<PathBuf> {
    non_empty(std::env::var_os("HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().filter(|h| !h.as_os_str().is_empty()))
}

fn non_empty(v: Option<OsString>) -> Option<OsString> {
    v.filter(|v| !v.is_empty())
}

fn resolve_claude_dir(
    configured: Option<&str>,
    env_dir: Option<OsString>,
    home: Option<&Path>,
) -> PathBuf {
    if let Some(c) = configured.filter(|c| !c.is_empty()) {
        return expand_tilde_with(c, home);
    }
    if let Some(d) = non_empty(env_dir) {
        return PathBuf::from(d);
    }
    home.unwrap_or(Path::new("~")).join(".claude")
}

/// `$XDG_CONFIG_HOME/claude-tower/config.toml`, else
/// `~/.config/claude-tower/config.toml`.
pub fn config_path() -> PathBuf {
    config_path_with(std::env::var_os("XDG_CONFIG_HOME"), home().as_deref())
}

fn config_path_with(xdg: Option<OsString>, home: Option<&Path>) -> PathBuf {
    // XDG says relative values are ignored.
    let base = non_empty(xdg)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.unwrap_or(Path::new("~")).join(".config"));
    base.join("claude-tower").join("config.toml")
}

/// Expands a leading `~` or `~/` to `$HOME`.
pub fn expand_tilde(path: &str) -> PathBuf {
    expand_tilde_with(path, home().as_deref())
}

/// `~user` and paths without a leading `~` are left alone, as is
/// everything when `home` is unknown.
fn expand_tilde_with(path: &str, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return PathBuf::from(path);
    };
    if path == "~" {
        return home.to_path_buf();
    }
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    fn home_u() -> Option<&'static Path> {
        Some(Path::new("/home/u"))
    }

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.refresh_ms, 1000);
        assert_eq!(c.group_by, GroupBy::Tmux);
        assert!(c.details_panel);
        assert_eq!(c.preview_lines, 15);
        assert_eq!(c.claude_dir, None);
        assert_eq!(parse("").unwrap(), c);
        assert_eq!(parse("# only a comment\n").unwrap(), c);
    }

    #[test]
    fn full_file() {
        let c = parse(
            r#"
            refresh_ms    = 500
            group_by      = "repo"
            details_panel = false
            preview_lines = 30
            layout        = "list"
            claude_dir    = "~/.claude-alt"
            "#,
        )
        .unwrap();
        assert_eq!(
            c,
            Config {
                refresh_ms: 500,
                group_by: GroupBy::Repo,
                details_panel: false,
                preview_lines: 30,
                layout: Layout::List,
                claude_dir: Some("~/.claude-alt".into()),
            }
        );
    }

    #[test]
    fn partial_files() {
        let c = parse("group_by = \"none\"").unwrap();
        assert_eq!(
            c,
            Config {
                group_by: GroupBy::None,
                ..Config::default()
            }
        );
        let c = parse("details_panel = false\npreview_lines = 0").unwrap();
        assert_eq!(
            c,
            Config {
                details_panel: false,
                preview_lines: 0,
                ..Config::default()
            }
        );
        assert_eq!(parse("group_by = \"tmux\"").unwrap(), Config::default());
    }

    #[test]
    fn unknown_key() {
        let e = parse("refresh = 500").unwrap_err().to_string();
        assert!(e.contains("refresh"), "{e}");
    }

    #[test]
    fn bad_values() {
        assert!(parse("group_by = \"project\"").is_err());
        assert!(parse("group_by = \"Tmux\"").is_err());
        assert!(parse("group_by = 1").is_err());
        assert!(parse("refresh_ms = -1").is_err());
        assert!(parse("refresh_ms = \"fast\"").is_err());
        assert!(parse("details_panel = \"yes\"").is_err());
        assert!(parse("not toml at all").is_err());
    }

    #[test]
    fn refresh_clamped() {
        assert_eq!(parse("refresh_ms = 0").unwrap().refresh_ms, 200);
        assert_eq!(parse("refresh_ms = 199").unwrap().refresh_ms, 200);
        assert_eq!(parse("refresh_ms = 200").unwrap().refresh_ms, 200);
        assert_eq!(parse("refresh_ms = 201").unwrap().refresh_ms, 201);
    }

    #[test]
    fn load_from_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        assert_eq!(Config::load_from(&path).unwrap(), Config::default());

        std::fs::write(&path, "refresh_ms = 50\ngroup_by = \"repo\"\n").unwrap();
        let c = Config::load_from(&path).unwrap();
        assert_eq!((c.refresh_ms, c.group_by), (200, GroupBy::Repo));

        // Errors name the file and the bad key.
        std::fs::write(&path, "refrsh_ms = 500\n").unwrap();
        let e = format!("{:#}", Config::load_from(&path).unwrap_err());
        assert!(e.contains(&path.display().to_string()), "{e}");
        assert!(e.contains("refrsh_ms"), "{e}");

        std::fs::write(&path, "group_by = \"projects\"\n").unwrap();
        let e = format!("{:#}", Config::load_from(&path).unwrap_err());
        assert!(e.contains(&path.display().to_string()), "{e}");

        // A folder where the file should be.
        let e = Config::load_from(tmp.path()).unwrap_err();
        assert!(format!("{e:#}").contains("reading"), "{e:#}");
    }

    #[test]
    fn tilde() {
        let p = |s| expand_tilde_with(s, home_u());
        assert_eq!(p("~"), PathBuf::from("/home/u"));
        assert_eq!(p("~/"), PathBuf::from("/home/u"));
        assert_eq!(p("~/.claude"), PathBuf::from("/home/u/.claude"));
        assert_eq!(p("~//x"), PathBuf::from("/home/u/x"));
        assert_eq!(p("~other/x"), PathBuf::from("~other/x"));
        assert_eq!(p("/abs/~/x"), PathBuf::from("/abs/~/x"));
        assert_eq!(p("rel"), PathBuf::from("rel"));
        assert_eq!(expand_tilde_with("~/x", None), PathBuf::from("~/x"));
    }

    #[test]
    fn claude_dir_precedence() {
        let r = resolve_claude_dir;
        // Config value first, tilde-expanded.
        assert_eq!(
            r(Some("~/.c"), os("/env"), home_u()),
            PathBuf::from("/home/u/.c")
        );
        assert_eq!(r(Some("/cfg"), os("/env"), home_u()), PathBuf::from("/cfg"));
        // Then $CLAUDE_CONFIG_DIR.
        assert_eq!(r(None, os("/env"), home_u()), PathBuf::from("/env"));
        assert_eq!(r(Some(""), os("/env"), home_u()), PathBuf::from("/env"));
        // Then ~/.claude.
        assert_eq!(r(None, os(""), home_u()), PathBuf::from("/home/u/.claude"));
        assert_eq!(r(None, None, home_u()), PathBuf::from("/home/u/.claude"));
    }

    #[test]
    fn claude_dir_uses_config_value() {
        let c = Config {
            claude_dir: Some("/x/claude".into()),
            ..Config::default()
        };
        assert_eq!(c.claude_dir(), PathBuf::from("/x/claude"));
    }

    #[test]
    fn config_paths() {
        assert_eq!(
            config_path_with(os("/xdg"), home_u()),
            PathBuf::from("/xdg/claude-tower/config.toml")
        );
        assert_eq!(
            config_path_with(None, home_u()),
            PathBuf::from("/home/u/.config/claude-tower/config.toml")
        );
        assert_eq!(
            config_path_with(os(""), home_u()),
            PathBuf::from("/home/u/.config/claude-tower/config.toml")
        );
        assert_eq!(
            config_path_with(os("rel"), home_u()),
            PathBuf::from("/home/u/.config/claude-tower/config.toml")
        );
        assert!(config_path().ends_with("claude-tower/config.toml"));
    }
}
