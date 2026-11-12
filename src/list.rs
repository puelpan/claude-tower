//! `claude-tower list`: one line per instance, or JSON for scripts (§8).

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Serialize;

use crate::collector::Collector;
use crate::config::Config;
use crate::model::{GroupBy, Instance, PrLink, Snapshot, State, build_groups, fmt_age};

/// Collects once and prints the instances.
pub fn run(
    config: &Config,
    json: bool,
    all: bool,
    group_by: Option<GroupBy>,
) -> anyhow::Result<()> {
    let snap = Collector::new(config).collect(None);
    let by = group_by.unwrap_or(config.group_by);
    let out = if json {
        let rows = rows(&snap, all);
        serde_json::to_string_pretty(&rows)? + "\n"
    } else {
        text(&snap, by, all)
    };
    print!("{out}");
    Ok(())
}

/// Plain text: warnings, then groups of aligned rows.
pub fn text(snap: &Snapshot, by: GroupBy, all: bool) -> String {
    let mut out = String::new();
    for warning in warnings(snap) {
        let _ = writeln!(out, "warning: {warning}");
    }
    let groups = build_groups(&snap.instances, by, "", all, snap.taken_at_ms);
    if groups.is_empty() {
        out.push_str("no Claude Code instances running\n");
        return out;
    }
    let shown: Vec<&Instance> = groups
        .iter()
        .flat_map(|g| g.rows.iter().map(|&i| &snap.instances[i]))
        .collect();
    let badge_w = shown
        .iter()
        .map(|i| width(&i.state.badge()))
        .max()
        .unwrap_or(0);
    let loc_w = shown.iter().map(|i| width(&location(i))).max().unwrap_or(0);
    let branch_w = shown
        .iter()
        .map(|i| width(i.branch.as_deref().unwrap_or("")))
        .max()
        .unwrap_or(0)
        .min(24);
    let indent = if by == GroupBy::None { "" } else { "  " };
    for group in &groups {
        if by != GroupBy::None {
            let _ = writeln!(out, "{}", group.name);
        }
        for &i in &group.rows {
            let inst = &snap.instances[i];
            let age = inst
                .state_since_ms
                .map(|t| fmt_age(snap.taken_at_ms - t))
                .unwrap_or_default();
            let branch = cut(inst.branch.as_deref().unwrap_or(""), branch_w);
            let line = format!(
                "{indent}{}  {:<3}  {:>4}  {}  {}  {}",
                pad(&inst.state.badge(), badge_w),
                if inst.is_new { "new" } else { "" },
                age,
                pad(&location(inst), loc_w),
                pad(&branch, branch_w),
                inst.title(),
            );
            let _ = writeln!(out, "{}", line.trim_end());
            if !inst.teammates.is_empty() {
                let _ = writeln!(out, "{indent}  └ teammates: {}", inst.teammates.join(", "));
            }
        }
    }
    out
}

fn warnings(snap: &Snapshot) -> Vec<String> {
    let mut w = Vec::new();
    if let Some(e) = &snap.registry_error {
        w.push(format!("registry: {e}"));
    }
    if let Some(e) = &snap.tmux_error {
        w.push(format!("tmux: {e}"));
    }
    for v in &snap.unverified_versions {
        w.push(format!(
            "Claude Code {v} is newer than the verified {}; run `claude-tower doctor`",
            crate::model::VERIFIED_VERSION
        ));
    }
    w
}

fn location(inst: &Instance) -> String {
    inst.location().unwrap_or_else(|| "—".into())
}

fn width(s: &str) -> usize {
    s.chars().count()
}

fn pad(s: &str, w: usize) -> String {
    let n = width(s);
    format!("{s}{}", " ".repeat(w.saturating_sub(n)))
}

fn cut(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// One instance in `list --json`.
#[derive(Debug, Serialize)]
pub struct Row {
    pub pid: u32,
    pub session_id: String,
    /// `needs_you`, `working`, `shell`, `idle`, `unknown`, `gone`.
    pub state: &'static str,
    /// `waitingFor` for needs_you, the raw status for unknown.
    pub state_detail: Option<String>,
    pub badge: String,
    pub state_since_ms: Option<i64>,
    pub is_new: bool,
    pub title: String,
    pub cwd: PathBuf,
    pub repo_root: Option<PathBuf>,
    pub branch: Option<String>,
    pub pr: Option<PrLink>,
    pub permission_mode: Option<String>,
    pub tmux_session: Option<String>,
    pub tmux_window: Option<String>,
    pub tmux_pane: Option<String>,
    pub recap: Option<String>,
    pub last_prompt: Option<String>,
    pub teammates: Vec<String>,
    pub version: String,
    pub transcript: Option<PathBuf>,
    pub last_activity_ms: Option<i64>,
    pub error: Option<String>,
}

/// Rows for `--json`, in state order.
pub fn rows(snap: &Snapshot, all: bool) -> Vec<Row> {
    let groups = build_groups(&snap.instances, GroupBy::None, "", all, snap.taken_at_ms);
    groups
        .iter()
        .flat_map(|g| g.rows.iter().map(|&i| row(&snap.instances[i])))
        .collect()
}

fn row(inst: &Instance) -> Row {
    let state_detail = match &inst.state {
        State::NeedsYou(w) | State::Unknown(w) if !w.is_empty() => Some(w.clone()),
        _ => None,
    };
    Row {
        pid: inst.pid,
        session_id: inst.session_id.clone(),
        state: inst.state.key(),
        state_detail,
        badge: inst.state.badge(),
        state_since_ms: inst.state_since_ms,
        is_new: inst.is_new,
        title: inst.title(),
        cwd: inst.cwd.clone(),
        repo_root: inst.repo_root.clone(),
        branch: inst.branch.clone(),
        pr: inst.pr.clone(),
        permission_mode: inst.permission_mode.clone(),
        tmux_session: inst.pane.as_ref().map(|p| p.session_name.clone()),
        tmux_window: inst.pane.as_ref().map(|p| p.window_name.clone()),
        tmux_pane: inst.pane.as_ref().map(|p| p.pane_id.clone()),
        recap: inst.recap.clone(),
        last_prompt: inst.last_prompt.clone(),
        teammates: inst.teammates.clone(),
        version: inst.version.clone(),
        transcript: inst.transcript.clone(),
        last_activity_ms: inst.last_activity_ms,
        error: inst.error.clone(),
    }
}
