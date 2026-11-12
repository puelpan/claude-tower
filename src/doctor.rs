//! `claude-tower doctor`: checks the setup, one line per check (§11).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::collector::Collector;
use crate::config::Config;
use crate::model::{VERIFIED_VERSION, version_newer};
use crate::registry::Registry;

/// How long `claude agents --json` may take (it starts Claude's binary).
const AGENTS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

struct Report {
    failed: bool,
}

impl Report {
    fn line(&mut self, level: Level, text: impl AsRef<str>) {
        let tag = match level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => {
                self.failed = true;
                "FAIL"
            }
        };
        println!("{tag}  {}", text.as_ref());
    }
}

/// Prints the checks; returns false if any failed.
pub fn run(config: &Config) -> bool {
    let mut r = Report { failed: false };
    let claude_dir = config.claude_dir();

    // Claude's folder and the registry.
    if !claude_dir.is_dir() {
        r.line(
            Level::Fail,
            format!("Claude's folder {} not found", claude_dir.display()),
        );
        return false;
    }
    r.line(
        Level::Ok,
        format!("Claude's folder: {}", claude_dir.display()),
    );
    let mut registry = Registry::new(claude_dir.join("sessions"));
    let files = match registry.scan() {
        Ok(files) => files,
        Err(e) => {
            r.line(
                Level::Fail,
                format!("registry {}: {e}", registry.dir().display()),
            );
            return false;
        }
    };
    let broken: Vec<_> = files.iter().filter(|f| f.entry.is_err()).collect();
    r.line(
        Level::Ok,
        format!(
            "registry: {} ({} files)",
            registry.dir().display(),
            files.len()
        ),
    );
    for f in &broken {
        let e = f
            .entry
            .as_ref()
            .err()
            .map(String::as_str)
            .unwrap_or_default();
        r.line(
            Level::Warn,
            format!("registry file doesn't parse: {}: {e}", f.path.display()),
        );
    }

    // One round of the collector for everything else.
    let snap = Collector::new(config).collect(None);
    let live: Vec<_> = snap.instances.iter().filter(|i| i.alive).collect();
    let gone = snap.instances.len() - live.len();
    r.line(
        Level::Ok,
        format!(
            "{} running instances, {gone} registry files of dead processes",
            live.len()
        ),
    );

    // Versions.
    let versions: BTreeSet<&str> = live
        .iter()
        .map(|i| i.version.as_str())
        .filter(|v| !v.is_empty())
        .collect();
    let newer: Vec<&str> = versions
        .iter()
        .copied()
        .filter(|v| version_newer(v, VERIFIED_VERSION))
        .collect();
    if newer.is_empty() {
        let seen: Vec<&str> = versions.iter().copied().collect();
        r.line(
            Level::Ok,
            format!(
                "versions: {} (verified: {VERIFIED_VERSION})",
                seen.join(", ")
            ),
        );
    } else {
        r.line(
            Level::Warn,
            format!(
                "versions {} are newer than the verified {VERIFIED_VERSION}: recheck spec §3 \
                 (registry fields and status values, transcript record types, team config) \
                 and add fixtures for the new version",
                newer.join(", ")
            ),
        );
    }

    // Cross-check with `claude agents --json` (§3.8).
    match claude_agents() {
        Ok(agents) => {
            let theirs: BTreeMap<u32, String> = agents
                .into_iter()
                .filter(|a| a.kind.as_deref().unwrap_or("interactive") == "interactive")
                .filter_map(|a| Some((a.pid?, a.session_id.unwrap_or_default())))
                .collect();
            let ours: BTreeMap<u32, String> =
                live.iter().map(|i| (i.pid, i.session_id.clone())).collect();
            let only_theirs: Vec<_> = theirs.keys().filter(|p| !ours.contains_key(p)).collect();
            let only_ours: Vec<_> = ours.keys().filter(|p| !theirs.contains_key(p)).collect();
            let differ: Vec<_> = ours
                .iter()
                .filter(|(p, id)| theirs.get(p).is_some_and(|t| t != *id))
                .map(|(p, _)| p)
                .collect();
            if only_theirs.is_empty() && only_ours.is_empty() && differ.is_empty() {
                r.line(
                    Level::Ok,
                    format!(
                        "claude agents --json: same {} interactive sessions",
                        ours.len()
                    ),
                );
            } else {
                r.line(
                    Level::Warn,
                    format!(
                        "claude agents --json differs: only there {only_theirs:?}, only in the \
                         registry {only_ours:?}, different session id {differ:?}"
                    ),
                );
            }
        }
        Err(e) => r.line(
            Level::Warn,
            format!("claude agents --json: {e} (cross-check skipped)"),
        ),
    }

    // Transcripts.
    let missing: Vec<_> = live.iter().filter(|i| i.transcript.is_none()).collect();
    if missing.is_empty() {
        r.line(
            Level::Ok,
            format!("transcripts: found for all {}", live.len()),
        );
    } else {
        for i in missing {
            r.line(
                Level::Warn,
                format!(
                    "no transcript for {} (pid {}): normal before the first message",
                    i.title(),
                    i.pid
                ),
            );
        }
    }
    for i in live.iter().filter(|i| i.error.is_some()) {
        r.line(
            Level::Warn,
            format!(
                "{} (pid {}): {}",
                i.title(),
                i.pid,
                i.error.as_deref().unwrap_or("")
            ),
        );
    }

    // tmux.
    match &snap.tmux_error {
        Some(e) => r.line(Level::Fail, format!("tmux: {e}")),
        None => {
            r.line(Level::Ok, "tmux: reachable");
            let unmapped: Vec<_> = live.iter().filter(|i| i.pane.is_none()).collect();
            if unmapped.is_empty() {
                r.line(Level::Ok, "panes: every instance maps to a pane");
            }
            for i in unmapped {
                r.line(
                    Level::Warn,
                    format!(
                        "no tmux pane for {} (pid {}, {})",
                        i.title(),
                        i.pid,
                        i.cwd.display()
                    ),
                );
            }
        }
    }
    if !crate::tmux::in_tmux() {
        r.line(
            Level::Warn,
            "not running inside tmux: Enter will only print an attach command",
        );
    }

    !r.failed
}

/// One entry of `claude agents --json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Agent {
    pid: Option<u32>,
    kind: Option<String>,
    session_id: Option<String>,
}

fn claude_agents() -> Result<Vec<Agent>, String> {
    let mut child = Command::new("claude")
        .args(["agents", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("running claude: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= AGENTS_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("exited with {status}"));
    }
    serde_json::from_slice(&out).map_err(|e| format!("unexpected output: {e}"))
}
