//! Rows and how they're shown: state, title, sort, group, filter (§4, §5).
//!
//! Everything here is pure: the collector fills in `Instance`s from the data
//! sources, and the UI and `list` only read them.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The newest Claude Code version whose files were checked against §3.
pub const VERIFIED_VERSION: &str = "2.1.322";

/// Group mode (§5), cycled with `g`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    /// By tmux session name.
    #[default]
    Tmux,
    /// By git repository, worktrees folded into their main repo.
    Repo,
    /// One list.
    None,
}

impl GroupBy {
    /// The next mode in the `g` cycle: tmux → repo → none → tmux.
    pub fn next(self) -> GroupBy {
        match self {
            GroupBy::Tmux => GroupBy::Repo,
            GroupBy::Repo => GroupBy::None,
            GroupBy::None => GroupBy::Tmux,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GroupBy::Tmux => "tmux",
            GroupBy::Repo => "repo",
            GroupBy::None => "none",
        }
    }
}

/// State of a row (§4). Variant order is the sort order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum State {
    /// Registry `waiting`; holds the raw `waitingFor` (empty if missing).
    NeedsYou(String),
    /// `busy`.
    Working,
    /// `shell`.
    Shell,
    /// `idle`.
    Idle,
    /// Any other or missing `status`, raw (empty if missing). Also used
    /// for a registry file that failed to parse; `Instance::error` then
    /// says why.
    Unknown(String),
    /// The process is dead (§3.7).
    Gone,
}

impl Default for State {
    fn default() -> State {
        State::Unknown(String::new())
    }
}

impl State {
    /// Maps the registry's `status` and `waitingFor` (§3.2). Never guesses:
    /// unknown values stay raw.
    pub fn from_status(status: Option<&str>, waiting_for: Option<&str>) -> State {
        match status {
            Some("waiting") => State::NeedsYou(waiting_for.unwrap_or_default().to_string()),
            Some("busy") => State::Working,
            Some("shell") => State::Shell,
            Some("idle") => State::Idle,
            Some(other) => State::Unknown(other.to_string()),
            None => State::Unknown(String::new()),
        }
    }

    /// Sort rank, 1 = most urgent (§4 table); Gone is 6.
    pub fn rank(&self) -> u8 {
        match self {
            State::NeedsYou(_) => 1,
            State::Working => 2,
            State::Shell => 3,
            State::Idle => 4,
            State::Unknown(_) => 5,
            State::Gone => 6,
        }
    }

    pub fn needs_you(&self) -> bool {
        matches!(self, State::NeedsYou(_))
    }

    /// Badge text without the spinner: `! permission`, `? question`,
    /// `! <waitingFor>`, `working`, `$ shell`, `· idle`, `? <raw>`, `gone`.
    /// The UI draws a spinner frame before `working`.
    pub fn badge(&self) -> String {
        match self {
            State::NeedsYou(w) => match w.as_str() {
                "permission prompt" => "! permission".to_string(),
                "input needed" => "? question".to_string(),
                "" => "! waiting".to_string(),
                other => format!("! {other}"),
            },
            State::Working => "working".to_string(),
            State::Shell => "$ shell".to_string(),
            State::Idle => "· idle".to_string(),
            State::Unknown(raw) if raw.is_empty() => "? unknown".to_string(),
            State::Unknown(raw) => format!("? {raw}"),
            State::Gone => "gone".to_string(),
        }
    }

    /// Short machine name for `list --json` and logs: `needs_you`,
    /// `working`, `shell`, `idle`, `unknown`, `gone`.
    pub fn key(&self) -> &'static str {
        match self {
            State::NeedsYou(_) => "needs_you",
            State::Working => "working",
            State::Shell => "shell",
            State::Idle => "idle",
            State::Unknown(_) => "unknown",
            State::Gone => "gone",
        }
    }
}

/// Where an instance's pane is, from `tmux list-panes` (§3.6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PaneInfo {
    /// e.g. `%3`.
    pub pane_id: String,
    /// e.g. `$1`.
    pub session_id: String,
    pub session_name: String,
    /// e.g. `@3`.
    pub window_id: String,
    pub window_index: u32,
    pub window_name: String,
    pub pane_index: u32,
    /// Raw `pane_title`, e.g. `✳ Issue 882`.
    pub title: String,
}

/// A PR linked from the transcript (§3.3 `pr-link`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PrLink {
    pub number: u64,
    pub url: String,
    /// e.g. `owner/repo`.
    pub repository: String,
}

/// One row: a Claude Code instance with everything known about it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Instance {
    // --- registry (§3.2) ---
    pub pid: u32,
    pub session_id: String,
    pub cwd: PathBuf,
    /// `interactive` or `background`.
    pub kind: String,
    pub version: String,
    /// Registry `name`, e.g. `shop-api-38`.
    pub name: String,
    /// Registry `nameSource`: `derived` or `user`.
    pub name_source: String,
    pub state: State,
    /// Registry `statusUpdatedAt`, ms since epoch.
    pub state_since_ms: Option<i64>,
    pub started_at_ms: Option<i64>,
    /// Set when the registry file or transcript couldn't be read; shown in
    /// the details panel.
    pub error: Option<String>,

    // --- liveness (§3.7) ---
    pub alive: bool,

    // --- tmux (§3.6) ---
    pub pane: Option<PaneInfo>,

    // --- transcript (§3.3) ---
    pub transcript: Option<PathBuf>,
    pub custom_title: Option<String>,
    pub ai_title: Option<String>,
    /// Latest `away_summary`.
    pub recap: Option<String>,
    pub last_prompt: Option<String>,
    /// Last assistant text block; shown when there's no recap.
    pub last_said: Option<String>,
    pub pr: Option<PrLink>,
    pub permission_mode: Option<String>,
    /// From the transcript's `gitBranch`, else from the repo's HEAD.
    pub branch: Option<String>,
    pub last_activity_ms: Option<i64>,

    // --- teams (§3.4) ---
    /// Teammate names, lead excluded.
    pub teammates: Vec<String>,

    // --- git (§5) ---
    /// Main working tree of the repo holding `cwd`, worktrees folded in.
    /// `None` outside a repo.
    pub repo_root: Option<PathBuf>,

    // --- derived by the collector (§4) ---
    /// Last status change happened while its pane wasn't being looked at.
    pub is_new: bool,
}

impl Instance {
    /// Stable identity across snapshots (pid plus session).
    pub fn key(&self) -> String {
        format!("{}:{}", self.pid, self.session_id)
    }

    /// Title, in order of preference (§3.3): `customTitle`, `aiTitle`, pane
    /// title without its glyph (ignoring Claude's default "Claude Code"), a
    /// registry name the user set, the first line of `lastPrompt`, the
    /// derived registry name, the folder name, the start of the session id.
    pub fn title(&self) -> String {
        let user_name = (self.name_source == "user").then(|| self.name.clone());
        let candidates = [
            self.custom_title.clone(),
            self.ai_title.clone(),
            self.pane.as_ref().and_then(|p| clean_pane_title(&p.title)),
            user_name,
            self.last_prompt.as_deref().map(prompt_line),
            Some(self.name.clone()),
            self.cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
            Some(self.session_id.chars().take(8).collect()),
        ];
        candidates
            .into_iter()
            .flatten()
            .map(|c| c.trim().to_string())
            .find(|c| !c.is_empty())
            .unwrap_or_default()
    }

    /// `session:window_index` when there's a pane, else `None`.
    pub fn location(&self) -> Option<String> {
        let p = self.pane.as_ref()?;
        Some(format!("{}:{}", p.session_name, p.window_index))
    }

    /// Group key for a mode (§5). Tmux: session name, or `(no tmux)`. Repo:
    /// the repo root path, or `cwd` outside a repo. None: empty.
    pub fn group_key(&self, by: GroupBy) -> String {
        match by {
            GroupBy::Tmux => self.tmux_group(),
            GroupBy::Repo => self.repo_path().to_string_lossy().into_owned(),
            GroupBy::None => String::new(),
        }
    }

    /// Group name shown in the header row. Tmux: session name. Repo: last
    /// path component of the key. None: empty.
    pub fn group_name(&self, by: GroupBy) -> String {
        match by {
            GroupBy::Tmux => self.tmux_group(),
            GroupBy::Repo => {
                let path = self.repo_path();
                match path.file_name() {
                    Some(n) => n.to_string_lossy().into_owned(),
                    None => path.to_string_lossy().into_owned(),
                }
            }
            GroupBy::None => String::new(),
        }
    }

    fn tmux_group(&self) -> String {
        match &self.pane {
            Some(p) => p.session_name.clone(),
            None => NO_TMUX.to_string(),
        }
    }

    /// The repo root, or `cwd` outside a repo.
    fn repo_path(&self) -> &Path {
        self.repo_root.as_deref().unwrap_or(&self.cwd)
    }

    /// Dead process or `State::Gone`.
    fn is_gone(&self) -> bool {
        !self.alive || self.state == State::Gone
    }

    /// `State::rank`, but 6 for any gone row.
    fn sort_rank(&self) -> u8 {
        if self.is_gone() {
            State::Gone.rank()
        } else {
            self.state.rank()
        }
    }
}

/// Tmux group key and name for an instance without a pane.
const NO_TMUX: &str = "(no tmux)";

/// Longest title taken from the last prompt, `…` included.
const PROMPT_TITLE_MAX: usize = 80;

/// First line of a prompt, cut to `PROMPT_TITLE_MAX` chars with `…`.
fn prompt_line(prompt: &str) -> String {
    let line = prompt.trim().lines().next().unwrap_or_default().trim();
    if line.chars().count() <= PROMPT_TITLE_MAX {
        return line.to_string();
    }
    let cut: String = line.chars().take(PROMPT_TITLE_MAX - 1).collect();
    format!("{}…", cut.trim_end())
}

/// Counts by state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub needs_you: usize,
    pub working: usize,
    pub shell: usize,
    pub idle: usize,
    pub unknown: usize,
    pub gone: usize,
}

impl Counts {
    pub fn of<'a>(instances: impl IntoIterator<Item = &'a Instance>) -> Counts {
        let mut c = Counts::default();
        for i in instances {
            if i.is_gone() {
                c.gone += 1;
                continue;
            }
            match i.state {
                State::NeedsYou(_) => c.needs_you += 1,
                State::Working => c.working += 1,
                State::Shell => c.shell += 1,
                State::Idle => c.idle += 1,
                State::Unknown(_) => c.unknown += 1,
                State::Gone => c.gone += 1,
            }
        }
        c
    }

    /// Everything but gone.
    pub fn running(&self) -> usize {
        self.needs_you + self.working + self.shell + self.idle + self.unknown
    }
}

/// A group of rows after filtering and sorting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub key: String,
    pub name: String,
    /// Indices into the `instances` slice given to `build_groups`, sorted.
    pub rows: Vec<usize>,
    pub counts: Counts,
}

/// Groups, filters and sorts `instances` (§4, §5).
///
/// - Gone rows are dropped unless `show_gone`.
/// - `filter` is split on whitespace; every word must match (§5).
/// - Within a group: by `State::rank`, then the longest-waiting needs-you
///   first and, for every other state, the most recent `state_since_ms`
///   first; missing times last; ties by title, then pid.
/// - Groups: by their most urgent row's rank, then by name. Groups left
///   empty by the filter are dropped.
/// - `GroupBy::None` gives one group with an empty key and name.
pub fn build_groups(
    instances: &[Instance],
    by: GroupBy,
    filter: &str,
    show_gone: bool,
    _now_ms: i64,
) -> Vec<Group> {
    let words: Vec<String> = filter.split_whitespace().map(str::to_string).collect();

    // Each group with its rows' indices and titles (computed once for the sort).
    let mut groups: Vec<(Group, Vec<(usize, String)>)> = Vec::new();
    for (idx, inst) in instances.iter().enumerate() {
        if (inst.is_gone() && !show_gone) || !matches_filter(inst, &words, by) {
            continue;
        }
        let key = inst.group_key(by);
        let pos = match groups.iter().position(|(g, _)| g.key == key) {
            Some(pos) => pos,
            None => {
                let group = Group {
                    key,
                    name: inst.group_name(by),
                    rows: Vec::new(),
                    counts: Counts::default(),
                };
                groups.push((group, Vec::new()));
                groups.len() - 1
            }
        };
        groups[pos].1.push((idx, inst.title()));
    }

    let mut ranked: Vec<(u8, Group)> = groups
        .into_iter()
        .map(|(mut group, mut rows)| {
            rows.sort_by(|(a, title_a), (b, title_b)| {
                let (a, b) = (&instances[*a], &instances[*b]);
                cmp_rows(a, b)
                    .then_with(|| title_a.cmp(title_b))
                    .then_with(|| a.pid.cmp(&b.pid))
            });
            group.rows = rows.into_iter().map(|(idx, _)| idx).collect();
            group.counts = Counts::of(group.rows.iter().map(|&i| &instances[i]));
            let best = group
                .rows
                .first()
                .map_or(u8::MAX, |&i| instances[i].sort_rank());
            (best, group)
        })
        .collect();
    ranked.sort_by(|(rank_a, a), (rank_b, b)| {
        rank_a
            .cmp(rank_b)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.key.cmp(&b.key))
    });
    ranked.into_iter().map(|(_, g)| g).collect()
}

/// Rank, then time in state: oldest first for needs-you, newest first for
/// the rest, missing times last.
fn cmp_rows(a: &Instance, b: &Instance) -> Ordering {
    // Only read when the ranks are equal, so `a` speaks for both.
    let oldest_first = a.state.needs_you() && !a.is_gone();
    a.sort_rank()
        .cmp(&b.sort_rank())
        .then_with(|| match (a.state_since_ms, b.state_since_ms) {
            (Some(ta), Some(tb)) if oldest_first => ta.cmp(&tb),
            (Some(ta), Some(tb)) => tb.cmp(&ta),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
}

/// Case-insensitive: every word must appear in the title, recap, last
/// prompt, branch, PR (`#<n>` or repository), group name, tmux session
/// name or window name.
pub fn matches_filter(instance: &Instance, words: &[String], by: GroupBy) -> bool {
    if words.is_empty() {
        return true;
    }
    let mut fields = vec![instance.title(), instance.group_name(by)];
    fields.extend(instance.recap.clone());
    fields.extend(instance.last_prompt.clone());
    fields.extend(instance.branch.clone());
    if let Some(pr) = &instance.pr {
        fields.push(format!("#{}", pr.number));
        fields.push(pr.repository.clone());
    }
    if let Some(p) = &instance.pane {
        fields.push(p.session_name.clone());
        fields.push(p.window_name.clone());
    }
    let fields: Vec<String> = fields.iter().map(|f| f.to_lowercase()).collect();
    words.iter().all(|w| {
        let w = w.to_lowercase();
        fields.iter().any(|f| f.contains(&w))
    })
}

/// Pane title without Claude's leading status glyph; `None` when empty or
/// Claude's default ("Claude Code").
pub fn clean_pane_title(title: &str) -> Option<String> {
    let title = title.trim();
    let (first, rest) = title.split_once(char::is_whitespace).unwrap_or((title, ""));
    // A glyph: one or two chars, none of them a letter or digit.
    let is_glyph = first.chars().count() <= 2 && !first.chars().any(char::is_alphanumeric);
    let cleaned = if is_glyph { rest.trim() } else { title };
    if cleaned.is_empty() || cleaned == "Claude Code" {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// Compact age: `12s`, `3m`, `2h`, `4d`. Negative counts as 0.
pub fn fmt_age(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 60 * 60 {
        format!("{}m", s / 60)
    } else if s < 24 * 60 * 60 {
        format!("{}h", s / (60 * 60))
    } else {
        format!("{}d", s / (24 * 60 * 60))
    }
}

/// True if dotted version `v` is newer than `than` (numeric per part;
/// non-numeric parts compare as 0). A `-` or `+` suffix is ignored.
pub fn version_newer(v: &str, than: &str) -> bool {
    let (a, b) = (version_parts(v), version_parts(than));
    let part = |p: &[u64], i: usize| p.get(i).copied().unwrap_or(0);
    for i in 0..a.len().max(b.len()) {
        match part(&a, i).cmp(&part(&b, i)) {
            Ordering::Equal => continue,
            o => return o == Ordering::Greater,
        }
    }
    false
}

/// `2.1.322-beta.1` → `[2, 1, 322]`.
fn version_parts(v: &str) -> Vec<u64> {
    let core = v.trim().split(['-', '+']).next().unwrap_or_default();
    core.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// The last lines of a pane, captured for the selected row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    pub pane_id: String,
    /// `capture-pane -e` output: text with SGR escape sequences.
    pub text: String,
}

/// Everything the collector found in one round.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Every registry entry, gone ones included (alive = false).
    pub instances: Vec<Instance>,
    pub taken_at_ms: i64,
    /// `tmux list-panes` failed: why.
    pub tmux_error: Option<String>,
    /// The registry folder couldn't be read: why.
    pub registry_error: Option<String>,
    /// Versions seen that are newer than `VERIFIED_VERSION`, sorted.
    pub unverified_versions: Vec<String>,
    pub preview: Option<Preview>,
    /// Transcripts still waiting for their first full read.
    pub transcripts_pending: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(session: &str, window_index: u32, window_name: &str) -> PaneInfo {
        PaneInfo {
            pane_id: format!("%{window_index}"),
            session_name: session.to_string(),
            window_index,
            window_name: window_name.to_string(),
            ..Default::default()
        }
    }

    /// A live instance with a fixed title.
    fn row(pid: u32, title: &str, state: State, since: Option<i64>) -> Instance {
        Instance {
            pid,
            alive: true,
            custom_title: Some(title.to_string()),
            state,
            state_since_ms: since,
            ..Default::default()
        }
    }

    fn in_session(mut i: Instance, session: &str) -> Instance {
        i.pane = Some(pane(session, 1, "claude"));
        i
    }

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    /// Titles of each group's rows, in order.
    fn titles(instances: &[Instance], groups: &[Group]) -> Vec<Vec<String>> {
        groups
            .iter()
            .map(|g| g.rows.iter().map(|&i| instances[i].title()).collect())
            .collect()
    }

    fn names(groups: &[Group]) -> Vec<&str> {
        groups.iter().map(|g| g.name.as_str()).collect()
    }

    // --- GroupBy ---

    #[test]
    fn group_by_cycles_and_labels() {
        assert_eq!(GroupBy::default(), GroupBy::Tmux);
        assert_eq!(GroupBy::Tmux.next(), GroupBy::Repo);
        assert_eq!(GroupBy::Repo.next(), GroupBy::None);
        assert_eq!(GroupBy::None.next(), GroupBy::Tmux);
        assert_eq!(GroupBy::Tmux.label(), "tmux");
        assert_eq!(GroupBy::Repo.label(), "repo");
        assert_eq!(GroupBy::None.label(), "none");
        for by in [GroupBy::Tmux, GroupBy::Repo, GroupBy::None] {
            let json = format!("\"{}\"", by.label());
            assert_eq!(serde_json::from_str::<GroupBy>(&json).unwrap(), by);
        }
    }

    // --- State ---

    #[test]
    fn state_from_every_status() {
        assert_eq!(State::from_status(Some("busy"), None), State::Working);
        assert_eq!(State::from_status(Some("shell"), None), State::Shell);
        assert_eq!(State::from_status(Some("idle"), None), State::Idle);
        assert_eq!(
            State::from_status(Some("waiting"), None),
            State::NeedsYou(String::new())
        );
        // waitingFor only matters for `waiting`.
        assert_eq!(
            State::from_status(Some("idle"), Some("permission prompt")),
            State::Idle
        );
    }

    #[test]
    fn state_from_every_waiting_for() {
        for w in [
            "permission prompt",
            "input needed",
            "goal proposal",
            "sandbox request",
            "worker request",
            "dialog open",
            "something new",
        ] {
            assert_eq!(
                State::from_status(Some("waiting"), Some(w)),
                State::NeedsYou(w.to_string())
            );
        }
    }

    #[test]
    fn state_unknown_status_stays_raw() {
        assert_eq!(
            State::from_status(Some("thinking"), None),
            State::Unknown("thinking".to_string())
        );
        // No guessing from internal names or case.
        assert_eq!(
            State::from_status(Some("running"), None),
            State::Unknown("running".to_string())
        );
        assert_eq!(
            State::from_status(Some("Busy"), None),
            State::Unknown("Busy".to_string())
        );
        assert_eq!(
            State::from_status(Some(""), None),
            State::Unknown(String::new())
        );
        assert_eq!(
            State::from_status(None, None),
            State::Unknown(String::new())
        );
        assert_eq!(
            State::from_status(None, Some("permission prompt")),
            State::Unknown(String::new())
        );
        assert_eq!(State::default(), State::Unknown(String::new()));
    }

    #[test]
    fn state_badges() {
        let needs = |w: &str| State::NeedsYou(w.to_string()).badge();
        assert_eq!(needs("permission prompt"), "! permission");
        assert_eq!(needs("input needed"), "? question");
        assert_eq!(needs("sandbox request"), "! sandbox request");
        assert_eq!(needs("goal proposal"), "! goal proposal");
        assert_eq!(needs(""), "! waiting");
        assert_eq!(State::Working.badge(), "working");
        assert_eq!(State::Shell.badge(), "$ shell");
        assert_eq!(State::Idle.badge(), "· idle");
        assert_eq!(State::Unknown("thinking".into()).badge(), "? thinking");
        assert_eq!(State::Unknown(String::new()).badge(), "? unknown");
        assert_eq!(State::Gone.badge(), "gone");
    }

    #[test]
    fn state_ranks_keys_and_needs_you() {
        let states = [
            State::NeedsYou("input needed".into()),
            State::Working,
            State::Shell,
            State::Idle,
            State::Unknown("x".into()),
            State::Gone,
        ];
        let ranks: Vec<u8> = states.iter().map(State::rank).collect();
        assert_eq!(ranks, [1, 2, 3, 4, 5, 6]);
        let keys: Vec<&str> = states.iter().map(State::key).collect();
        assert_eq!(
            keys,
            ["needs_you", "working", "shell", "idle", "unknown", "gone"]
        );
        let needs: Vec<bool> = states.iter().map(State::needs_you).collect();
        assert_eq!(needs, [true, false, false, false, false, false]);
    }

    // --- title ---

    #[test]
    fn title_fallback_chain() {
        let mut i = Instance {
            custom_title: Some("Custom".into()),
            ai_title: Some("Issue 882".into()),
            pane: Some(PaneInfo {
                title: "✳ Pane title".into(),
                ..Default::default()
            }),
            name: "my-name".into(),
            name_source: "user".into(),
            last_prompt: Some("fix the login\nthen the tests".into()),
            cwd: "/home/me/projects/shop-api".into(),
            session_id: "1f4e2d3c-5b6a-4c7d-8e9f-0a1b2c3d4e5f".into(),
            ..Default::default()
        };
        assert_eq!(i.title(), "Custom");
        i.custom_title = Some("   ".into());
        assert_eq!(i.title(), "Issue 882");
        i.ai_title = None;
        assert_eq!(i.title(), "Pane title");
        i.pane.as_mut().unwrap().title = "✳ Claude Code".into();
        assert_eq!(i.title(), "my-name");
        i.name_source = "derived".into();
        assert_eq!(i.title(), "fix the login");
        i.last_prompt = Some(" \n ".into());
        assert_eq!(i.title(), "my-name");
        i.name = "  ".into();
        assert_eq!(i.title(), "shop-api");
        i.cwd = PathBuf::new();
        assert_eq!(i.title(), "1f4e2d3c");
        i.session_id = String::new();
        assert_eq!(i.title(), "");
    }

    #[test]
    fn title_is_trimmed() {
        let i = Instance {
            ai_title: Some("  Issue 882 \n".into()),
            ..Default::default()
        };
        assert_eq!(i.title(), "Issue 882");
    }

    #[test]
    fn title_from_last_prompt() {
        let title = |p: &str| {
            Instance {
                last_prompt: Some(p.to_string()),
                ..Default::default()
            }
            .title()
        };
        assert_eq!(title("\n\n  first line  \nsecond"), "first line");

        let exact = "x".repeat(80);
        assert_eq!(title(&exact), exact);

        let long = title(&"é".repeat(100));
        assert_eq!(long.chars().count(), 80);
        assert!(long.ends_with('…'));
        assert_eq!(long, format!("{}…", "é".repeat(79)));
    }

    #[test]
    fn clean_pane_title_cases() {
        let c = clean_pane_title;
        assert_eq!(c("✳ Spec.md review").as_deref(), Some("Spec.md review"));
        assert_eq!(c("✳ Claude Code"), None);
        assert_eq!(c("⠂ Issue 882").as_deref(), Some("Issue 882"));
        assert_eq!(c("⠐ Issue 882").as_deref(), Some("Issue 882"));
        assert_eq!(c("Issue 882").as_deref(), Some("Issue 882"));
        for glyph in ["✶", "✻", "✽", "✢", "·"] {
            assert_eq!(c(&format!("{glyph} Busy")).as_deref(), Some("Busy"));
        }
        // Glyph with a variation selector is still two chars.
        assert_eq!(c("✳\u{fe0f} Title").as_deref(), Some("Title"));
        assert_eq!(c("  ✳   spaced  ").as_deref(), Some("spaced"));
        assert_eq!(c("✳"), None);
        assert_eq!(c(""), None);
        assert_eq!(c("   "), None);
        assert_eq!(c("Claude Code"), None);
        assert_eq!(c("claude code").as_deref(), Some("claude code"));
        // First word kept when it has a letter or digit, or is long.
        assert_eq!(c("v2 plan").as_deref(), Some("v2 plan"));
        assert_eq!(c("é plan").as_deref(), Some("é plan"));
        assert_eq!(c("--> next").as_deref(), Some("--> next"));
        assert_eq!(c("Fix").as_deref(), Some("Fix"));
    }

    // --- location and groups ---

    #[test]
    fn location_from_pane() {
        let mut i = Instance::default();
        assert_eq!(i.location(), None);
        i.pane = Some(pane("shop-api", 3, "claude"));
        assert_eq!(i.location().as_deref(), Some("shop-api:3"));
    }

    #[test]
    fn group_key_and_name_tmux() {
        let mut i = Instance::default();
        assert_eq!(i.group_key(GroupBy::Tmux), "(no tmux)");
        assert_eq!(i.group_name(GroupBy::Tmux), "(no tmux)");
        i.pane = Some(pane("shop-api", 1, "claude"));
        assert_eq!(i.group_key(GroupBy::Tmux), "shop-api");
        assert_eq!(i.group_name(GroupBy::Tmux), "shop-api");
    }

    #[test]
    fn group_key_and_name_repo() {
        let mut i = Instance {
            cwd: "/home/me/projects/web-front/.claude/worktrees/feat-x".into(),
            repo_root: Some("/home/me/projects/web-front".into()),
            ..Default::default()
        };
        assert_eq!(i.group_key(GroupBy::Repo), "/home/me/projects/web-front");
        assert_eq!(i.group_name(GroupBy::Repo), "web-front");

        // Outside a repo: the folder.
        i.repo_root = None;
        i.cwd = "/tmp/scratch/".into();
        assert_eq!(i.group_key(GroupBy::Repo), "/tmp/scratch/");
        assert_eq!(i.group_name(GroupBy::Repo), "scratch");
    }

    #[test]
    fn group_key_and_name_none() {
        let i = Instance {
            pane: Some(pane("shop-api", 1, "claude")),
            cwd: "/home/me/projects/shop-api".into(),
            ..Default::default()
        };
        assert_eq!(i.group_key(GroupBy::None), "");
        assert_eq!(i.group_name(GroupBy::None), "");
    }

    // --- filter ---

    fn filter_target() -> Instance {
        Instance {
            alive: true,
            ai_title: Some("Checkout Redesign".into()),
            recap: Some("Two failing checks are a CI outage".into()),
            last_prompt: Some("rerun the storybook build".into()),
            branch: Some("feat/checkout-v2".into()),
            pr: Some(PrLink {
                number: 98,
                url: "https://github.com/acme/web-front/pull/98".into(),
                repository: "acme/web-front".into(),
            }),
            pane: Some(pane("frontend", 2, "editor")),
            cwd: "/home/me/projects/web-front/.claude/worktrees/feat-x".into(),
            repo_root: Some("/home/me/projects/web-front-main".into()),
            ..Default::default()
        }
    }

    #[test]
    fn filter_matches_each_field() {
        let i = filter_target();
        let m = |w: &str, by: GroupBy| matches_filter(&i, &words(w), by);
        assert!(m("redesign", GroupBy::Tmux)); // title
        assert!(m("outage", GroupBy::Tmux)); // recap
        assert!(m("storybook", GroupBy::Tmux)); // last prompt
        assert!(m("checkout-v2", GroupBy::Tmux)); // branch
        assert!(m("#98", GroupBy::Tmux)); // PR number
        assert!(m("acme/web", GroupBy::Tmux)); // PR repository
        assert!(m("frontend", GroupBy::Tmux)); // session, also the group
        assert!(m("editor", GroupBy::Tmux)); // window name
        assert!(m("front-main", GroupBy::Repo)); // repo group name
        assert!(!m("front-main", GroupBy::Tmux));
        assert!(!m("pull", GroupBy::Tmux)); // PR url isn't searched
        assert!(!m("nothing", GroupBy::Tmux));
    }

    #[test]
    fn filter_case_and_words() {
        let i = filter_target();
        let m = |w: &str| matches_filter(&i, &words(w), GroupBy::Tmux);
        assert!(m(""));
        assert!(matches_filter(&i, &[], GroupBy::Tmux));
        assert!(m("CHECKOUT"));
        assert!(m("ReDeSiGn"));
        // Every word must match, each in any field.
        assert!(m("checkout outage editor"));
        assert!(!m("checkout missing"));
        assert!(!m("missing checkout"));

        // Matching uses the computed title, not just the raw fields.
        let derived = Instance {
            name: "Shop-API-38".into(),
            ..Default::default()
        };
        assert!(matches_filter(&derived, &words("shop-api"), GroupBy::None));
    }

    // --- counts ---

    #[test]
    fn counts_by_state() {
        let mut dead = row(7, "dead", State::Working, None);
        dead.alive = false;
        let instances = [
            row(1, "a", State::NeedsYou("input needed".into()), None),
            row(2, "b", State::Working, None),
            row(3, "c", State::Working, None),
            row(4, "d", State::Shell, None),
            row(5, "e", State::Idle, None),
            row(6, "f", State::Unknown("x".into()), None),
            dead,
            row(8, "g", State::Gone, None),
        ];
        let c = Counts::of(&instances);
        assert_eq!(
            c,
            Counts {
                needs_you: 1,
                working: 2,
                shell: 1,
                idle: 1,
                unknown: 1,
                gone: 2,
            }
        );
        assert_eq!(c.running(), 6);
        assert_eq!(Counts::of(&[]).running(), 0);
    }

    // --- build_groups ---

    #[test]
    fn sort_within_group() {
        let instances = [
            row(1, "needs 300", State::NeedsYou("".into()), Some(300)),
            row(2, "needs 100", State::NeedsYou("".into()), Some(100)),
            row(3, "needs none", State::NeedsYou("".into()), None),
            row(4, "work 100", State::Working, Some(100)),
            row(5, "work 500", State::Working, Some(500)),
            row(6, "work none", State::Working, None),
            row(7, "shell", State::Shell, Some(10)),
            row(8, "idle 200", State::Idle, Some(200)),
            row(9, "idle 900", State::Idle, Some(900)),
            row(10, "idle none", State::Idle, None),
            row(11, "unknown", State::Unknown("x".into()), Some(50)),
        ];
        let groups = build_groups(&instances, GroupBy::None, "", false, 1000);
        assert_eq!(
            titles(&instances, &groups),
            [[
                "needs 100",
                "needs 300",
                "needs none",
                "work 500",
                "work 100",
                "work none",
                "shell",
                "idle 900",
                "idle 200",
                "idle none",
                "unknown",
            ]]
        );
    }

    #[test]
    fn sort_ties_by_title_then_pid() {
        let instances = [
            row(9, "beta", State::Idle, Some(100)),
            row(5, "alpha", State::Idle, Some(100)),
            row(3, "beta", State::Idle, Some(100)),
            row(1, "alpha", State::Idle, None),
        ];
        let groups = build_groups(&instances, GroupBy::None, "", false, 0);
        assert_eq!(groups[0].rows, [1, 2, 0, 3]);
    }

    #[test]
    fn gone_rows_hidden_unless_shown() {
        let mut dead = row(2, "dead", State::Working, Some(900));
        dead.alive = false;
        let instances = [
            row(1, "idle", State::Idle, Some(100)),
            dead,
            row(3, "gone", State::Gone, Some(500)),
            row(4, "work", State::Working, Some(100)),
        ];

        let groups = build_groups(&instances, GroupBy::None, "", false, 0);
        assert_eq!(titles(&instances, &groups), [["work", "idle"]]);
        assert_eq!(groups[0].counts.gone, 0);

        // Shown, both kinds of gone sort last, newest first.
        let groups = build_groups(&instances, GroupBy::None, "", true, 0);
        assert_eq!(
            titles(&instances, &groups),
            [["work", "idle", "dead", "gone"]]
        );
        assert_eq!(groups[0].counts.gone, 2);
        assert_eq!(groups[0].counts.working, 1);

        // A group of only gone rows ranks after the others.
        let instances = [
            in_session(row(1, "dead", State::Gone, None), "aaa"),
            in_session(row(2, "idle", State::Idle, None), "zzz"),
        ];
        let groups = build_groups(&instances, GroupBy::Tmux, "", true, 0);
        assert_eq!(names(&groups), ["zzz", "aaa"]);
        let groups = build_groups(&instances, GroupBy::Tmux, "", false, 0);
        assert_eq!(names(&groups), ["zzz"]);
    }

    #[test]
    fn group_by_tmux_session() {
        let instances = [
            in_session(row(1, "alpha idle", State::Idle, Some(10)), "alpha"),
            in_session(row(2, "mid idle", State::Idle, Some(10)), "mid"),
            in_session(row(3, "zeta work", State::Working, Some(10)), "zeta"),
            in_session(
                row(4, "zeta needs", State::NeedsYou("".into()), Some(10)),
                "zeta",
            ),
            row(5, "loose", State::Working, Some(10)),
            in_session(row(6, "alpha shell", State::Shell, Some(10)), "alpha"),
        ];
        let groups = build_groups(&instances, GroupBy::Tmux, "", false, 0);
        // Best rank first (zeta 1, no tmux 2, alpha 3, mid 4).
        assert_eq!(names(&groups), ["zeta", "(no tmux)", "alpha", "mid"]);
        assert_eq!(groups[0].key, "zeta");
        assert_eq!(groups[1].key, "(no tmux)");
        assert_eq!(
            titles(&instances, &groups),
            vec![
                vec!["zeta needs", "zeta work"],
                vec!["loose"],
                vec!["alpha shell", "alpha idle"],
                vec!["mid idle"],
            ]
        );
        assert_eq!(groups[0].counts.needs_you, 1);
        assert_eq!(groups[0].counts.working, 1);
        assert_eq!(groups[0].counts.running(), 2);
    }

    #[test]
    fn groups_with_same_rank_sort_by_name() {
        let instances = [
            in_session(row(1, "b", State::Idle, None), "beta"),
            in_session(row(2, "a", State::Idle, None), "Alpha"),
            in_session(row(3, "c", State::Idle, None), "alpha"),
        ];
        let groups = build_groups(&instances, GroupBy::Tmux, "", false, 0);
        assert_eq!(names(&groups), ["Alpha", "alpha", "beta"]);
    }

    #[test]
    fn group_by_repo_folds_worktrees() {
        let at = |pid, title, cwd: &str, root: Option<&str>| Instance {
            cwd: cwd.into(),
            repo_root: root.map(PathBuf::from),
            ..row(pid, title, State::Idle, Some(i64::from(pid)))
        };
        let root = Some("/home/me/projects/shop-api");
        let instances = [
            at(1, "main", "/home/me/projects/shop-api", root),
            at(
                2,
                "feat x",
                "/home/me/projects/shop-api/.claude/worktrees/feat-x",
                root,
            ),
            at(
                3,
                "feat y",
                "/home/me/projects/shop-api/.claude/worktrees/feat-y",
                root,
            ),
            at(4, "scratch", "/tmp/scratch", None),
        ];
        let groups = build_groups(&instances, GroupBy::Repo, "", false, 0);
        assert_eq!(names(&groups), ["scratch", "shop-api"]);
        assert_eq!(groups[0].key, "/tmp/scratch");
        assert_eq!(groups[1].key, "/home/me/projects/shop-api");
        assert_eq!(groups[1].rows, [2, 1, 0]);
        assert_eq!(groups[1].counts.idle, 3);
    }

    #[test]
    fn group_by_repo_same_name_different_paths() {
        let at = |pid, root: &str| Instance {
            repo_root: Some(root.into()),
            ..row(pid, "t", State::Idle, None)
        };
        let instances = [at(1, "/b/api"), at(2, "/a/api"), at(3, "/b/api")];
        let groups = build_groups(&instances, GroupBy::Repo, "", false, 0);
        let keys: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, ["/a/api", "/b/api"]);
        assert_eq!(names(&groups), ["api", "api"]);
        assert_eq!(groups[1].rows, [0, 2]);
    }

    #[test]
    fn group_by_none_is_one_list() {
        let instances = [
            in_session(row(1, "idle", State::Idle, None), "a"),
            in_session(row(2, "needs", State::NeedsYou("".into()), None), "b"),
            row(3, "work", State::Working, None),
        ];
        let groups = build_groups(&instances, GroupBy::None, "", false, 0);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].key, "");
        assert_eq!(groups[0].name, "");
        assert_eq!(groups[0].rows, [1, 2, 0]);
        assert_eq!(groups[0].counts.running(), 3);
    }

    #[test]
    fn filter_drops_rows_and_empty_groups() {
        let instances = [
            in_session(row(1, "Issue 882", State::Idle, None), "shop-api"),
            in_session(row(2, "PR 891 review", State::Working, None), "shop-api"),
            in_session(row(3, "Nightly import", State::Working, None), "etl"),
        ];
        let groups = build_groups(&instances, GroupBy::Tmux, "  ISSUE  ", false, 0);
        assert_eq!(names(&groups), ["shop-api"]);
        assert_eq!(groups[0].rows, [0]);
        // Counts cover only the rows left.
        assert_eq!(groups[0].counts.idle, 1);
        assert_eq!(groups[0].counts.working, 0);

        // A group name matches all of its rows.
        let groups = build_groups(&instances, GroupBy::Tmux, "shop", false, 0);
        assert_eq!(groups[0].rows, [1, 0]);

        assert!(build_groups(&instances, GroupBy::Tmux, "zzz", false, 0).is_empty());
        assert!(build_groups(&[], GroupBy::None, "", false, 0).is_empty());
    }

    // --- fmt_age and version_newer ---

    #[test]
    fn fmt_age_boundaries() {
        assert_eq!(fmt_age(-5_000), "0s");
        assert_eq!(fmt_age(0), "0s");
        assert_eq!(fmt_age(999), "0s");
        assert_eq!(fmt_age(12_000), "12s");
        assert_eq!(fmt_age(59_999), "59s");
        assert_eq!(fmt_age(60_000), "1m");
        assert_eq!(fmt_age(3_599_999), "59m");
        assert_eq!(fmt_age(3_600_000), "1h");
        assert_eq!(fmt_age(86_399_999), "23h");
        assert_eq!(fmt_age(86_400_000), "1d");
        assert_eq!(fmt_age(4 * 86_400_000 + 5), "4d");
    }

    #[test]
    fn version_newer_cases() {
        assert!(version_newer("2.1.323", "2.1.322"));
        assert!(version_newer("2.2.0", "2.1.322"));
        assert!(version_newer("3", "2.9.9"));
        assert!(version_newer("2.1.322.1", "2.1.322"));
        assert!(version_newer("2.1.323-beta.1", "2.1.322"));
        assert!(version_newer("2.1.323+build", "2.1.322"));
        assert!(!version_newer("2.1.322", "2.1.322"));
        assert!(!version_newer("2.1.322.0", "2.1.322"));
        assert!(!version_newer("2.1.9", "2.1.10"));
        assert!(!version_newer("2.1.321", "2.1.322"));
        assert!(!version_newer("2.1.322-beta.1", "2.1.322"));
        assert!(!version_newer("garbage", "2.1.322"));
        assert!(!version_newer("", "2.1.322"));
        assert!(version_newer(" 2.1.400 ", VERIFIED_VERSION));
    }
}
