//! The collector: builds a `Snapshot` from every source, once per refresh,
//! on its own thread (§9). The UI never reads files or runs tmux itself.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::model::{Instance, Preview, Snapshot, State, VERIFIED_VERSION, version_newer};
use crate::registry::{self, Registry, RegistryFile};
use crate::teams::{self, Teams};
use crate::tmux::{Pane, Tmux};
use crate::{git, proc, transcript};

/// How long a tmux call may take.
const TMUX_TIMEOUT: Duration = Duration::from_secs(2);
/// How long git info for a folder is reused.
const GIT_TTL: Duration = Duration::from_secs(10);
/// How often a transcript that wasn't found is looked for again.
const LOCATE_RETRY: Duration = Duration::from_secs(5);
/// Most parent processes walked to find a pane.
const MAX_ANCESTORS: usize = 32;

/// What the UI asks of the collector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Capture this pane (or none) for the preview from now on.
    Preview(Option<String>),
    /// Start a round now instead of waiting for the timer.
    RefreshNow,
    /// Stop the thread.
    Quit,
}

/// The UI's ends of the collector's channels.
pub struct Handle {
    pub snapshots: Receiver<Snapshot>,
    pub requests: Sender<Request>,
}

/// Starts the collector thread. It sends a first snapshot right away
/// (before reading transcripts), then one per round.
pub fn spawn(config: &Config) -> Handle {
    let (snap_tx, snapshots) = mpsc::channel();
    let (requests, req_rx) = mpsc::channel();
    let config = config.clone();
    std::thread::Builder::new()
        .name("collector".into())
        .spawn(move || run(&config, &snap_tx, &req_rx))
        .expect("spawning the collector thread");
    Handle {
        snapshots,
        requests,
    }
}

fn run(config: &Config, tx: &Sender<Snapshot>, rx: &Receiver<Request>) {
    let period = Duration::from_millis(config.refresh_ms);
    let mut collector = Collector::new(config);
    let mut preview: Option<String> = None;
    // Rows first, titles once the transcripts are read (§3.3).
    if tx.send(collector.round(None, false)).is_err() {
        return;
    }
    loop {
        let mut last = collector.collect(preview.as_deref());
        if tx.send(last.clone()).is_err() {
            return;
        }
        let deadline = Instant::now() + period;
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(Request::Preview(pane)) => {
                    if pane == preview {
                        continue;
                    }
                    // Only the preview changed: capture it without a full round.
                    preview = pane;
                    last.preview = preview.as_deref().and_then(|p| collector.capture(p));
                    if tx.send(last.clone()).is_err() {
                        return;
                    }
                }
                Ok(Request::RefreshNow) | Err(RecvTimeoutError::Timeout) => break,
                Ok(Request::Quit) | Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

/// Repo root and branch of a folder, with when they were read.
struct GitInfo {
    repo_root: Option<PathBuf>,
    branch: Option<String>,
    read_at: Instant,
}

/// A session's transcript reader, or when it was last looked for.
enum TranscriptSlot {
    Found(Box<transcript::Reader>),
    Missing(Instant),
}

/// Builds snapshots; keeps transcript readers and caches between rounds.
pub struct Collector {
    claude_dir: PathBuf,
    preview_lines: u32,
    registry: Registry,
    teams: Teams,
    tmux: Tmux,
    transcripts: HashMap<String, TranscriptSlot>,
    git: HashMap<PathBuf, GitInfo>,
    /// Pane id → last time someone was looking at it (ms).
    seen: HashMap<String, i64>,
    /// When the collector started (ms); status changes before it aren't new.
    started_ms: i64,
}

impl Collector {
    pub fn new(config: &Config) -> Collector {
        let claude_dir = config.claude_dir();
        Collector {
            registry: Registry::new(claude_dir.join("sessions")),
            teams: Teams::new(claude_dir.join("teams")),
            claude_dir,
            preview_lines: config.preview_lines,
            tmux: Tmux::new(TMUX_TIMEOUT),
            transcripts: HashMap::new(),
            git: HashMap::new(),
            seen: HashMap::new(),
            started_ms: now_ms(),
        }
    }

    pub fn claude_dir(&self) -> &Path {
        &self.claude_dir
    }

    /// One full round: registry → liveness → tmux → transcripts → teams →
    /// git → rows, plus the preview for `preview_pane`.
    pub fn collect(&mut self, preview_pane: Option<&str>) -> Snapshot {
        self.round(preview_pane, true)
    }

    /// Captures a pane for the preview; `None` on any error.
    pub fn capture(&self, pane_id: &str) -> Option<Preview> {
        let text = self.tmux.capture(pane_id, self.preview_lines).ok()?;
        Some(Preview {
            pane_id: pane_id.to_string(),
            text,
        })
    }

    /// A round. With `read_new` false, transcripts not read yet are left
    /// for a later round, so the first snapshot comes fast.
    fn round(&mut self, preview_pane: Option<&str>, read_new: bool) -> Snapshot {
        let now = now_ms();
        let mut snap = Snapshot {
            taken_at_ms: now,
            ..Snapshot::default()
        };

        let files = match self.registry.scan() {
            Ok(files) => files,
            Err(e) => {
                snap.registry_error = Some(format!("{}: {e}", self.registry.dir().display()));
                Vec::new()
            }
        };
        let mut instances: Vec<Instance> = files.iter().filter_map(from_registry).collect();

        let panes = match self.tmux.list_panes() {
            Ok(panes) => panes,
            Err(e) => {
                snap.tmux_error = Some(e);
                Vec::new()
            }
        };
        self.mark_seen(&panes, now);
        let proc_starts: HashMap<u32, Option<String>> = files
            .iter()
            .filter_map(|f| Some((f.pid, f.entry.as_ref().ok()?.proc_start.clone())))
            .collect();
        let tmux_fields: HashMap<u32, Option<String>> = files
            .iter()
            .filter_map(|f| Some((f.pid, f.entry.as_ref().ok()?.tmux.clone())))
            .collect();

        let teams = self.teams.scan();
        let mut live_sessions = HashSet::new();
        for inst in &mut instances {
            let tmux_field = tmux_fields.get(&inst.pid).cloned().flatten();
            inst.pane =
                find_pane(&panes, tmux_field.as_deref(), inst.pid, inst.alive).map(Pane::info);
            inst.is_new = self.is_new(inst);
            if !inst.alive {
                continue;
            }
            if !inst.session_id.is_empty() {
                live_sessions.insert(inst.session_id.clone());
                self.fill_transcript(inst, read_new, &mut snap.transcripts_pending);
            }
            let proc_start = proc_starts.get(&inst.pid).cloned().flatten();
            if let Some(team) =
                teams::team_of(&teams, inst.pid, proc_start.as_deref(), &inst.session_id)
            {
                inst.teammates = teams::teammate_names(team);
            }
            self.fill_git(inst);
        }
        self.transcripts.retain(|id, _| live_sessions.contains(id));
        self.git
            .retain(|_, info| info.read_at.elapsed() < GIT_TTL * 6);

        let mut versions: Vec<String> = instances
            .iter()
            .filter(|i| i.alive && version_newer(&i.version, VERIFIED_VERSION))
            .map(|i| i.version.clone())
            .collect();
        versions.sort();
        versions.dedup();
        snap.unverified_versions = versions;

        snap.preview = preview_pane
            .filter(|p| panes.iter().any(|pane| pane.pane_id == *p))
            .and_then(|p| self.capture(p));
        snap.instances = instances;
        snap
    }

    /// Records who is being looked at now (§4 "new").
    fn mark_seen(&mut self, panes: &[Pane], now: i64) {
        for pane in panes.iter().filter(|p| p.is_seen()) {
            self.seen.insert(pane.pane_id.clone(), now);
        }
        self.seen
            .retain(|id, _| panes.iter().any(|p| p.pane_id == *id));
    }

    /// Finished or started waiting while its pane wasn't being looked at.
    fn is_new(&self, inst: &Instance) -> bool {
        let settled = matches!(inst.state, State::NeedsYou(_) | State::Idle | State::Shell);
        let (Some(pane), Some(since)) = (&inst.pane, inst.state_since_ms) else {
            return false;
        };
        let seen = self
            .seen
            .get(&pane.pane_id)
            .copied()
            .unwrap_or(self.started_ms);
        inst.alive && settled && since > seen
    }

    fn fill_transcript(&mut self, inst: &mut Instance, read_new: bool, pending: &mut usize) {
        let cwd = inst.cwd.to_string_lossy();
        let slot = self.transcripts.entry(inst.session_id.clone());
        let slot = match slot {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(locate(&self.claude_dir, &cwd, &inst.session_id))
            }
        };
        if let TranscriptSlot::Missing(at) = slot {
            if at.elapsed() < LOCATE_RETRY {
                return;
            }
            *slot = locate(&self.claude_dir, &cwd, &inst.session_id);
        }
        let TranscriptSlot::Found(reader) = slot else {
            return;
        };
        if (read_new || reader.is_scanned())
            && let Err(e) = reader.update()
        {
            if e.kind() == std::io::ErrorKind::NotFound {
                *slot = TranscriptSlot::Missing(Instant::now());
                return;
            }
            inst.error = Some(format!("transcript: {e}"));
        }
        if !reader.is_scanned() {
            *pending += 1;
        }
        inst.transcript = Some(reader.path().to_path_buf());
        let d = reader.digest();
        inst.custom_title = d.custom_title.clone();
        inst.ai_title = d.ai_title.clone();
        inst.recap = d.recap.clone();
        inst.last_prompt = d.last_prompt.clone();
        inst.last_said = d.last_said.clone();
        inst.pr = d.pr.clone();
        inst.permission_mode = d.permission_mode.clone();
        inst.branch = d.git_branch.clone();
        inst.last_activity_ms = d.last_activity_ms;
    }

    fn fill_git(&mut self, inst: &mut Instance) {
        if inst.cwd.as_os_str().is_empty() {
            return;
        }
        let fresh = self
            .git
            .get(&inst.cwd)
            .is_some_and(|info| info.read_at.elapsed() < GIT_TTL);
        if !fresh {
            let info = GitInfo {
                repo_root: git::repo_root(&inst.cwd),
                branch: git::branch(&inst.cwd),
                read_at: Instant::now(),
            };
            self.git.insert(inst.cwd.clone(), info);
        }
        let info = &self.git[&inst.cwd];
        inst.repo_root = info.repo_root.clone();
        // The live HEAD first: the transcript's `gitBranch` is cached by
        // Claude and can be stale, or "HEAD" for a repo without commits.
        let transcript = inst.branch.take().filter(|b| b != "HEAD");
        inst.branch = info.branch.clone().or(transcript);
    }
}

fn locate(claude_dir: &Path, cwd: &str, session_id: &str) -> TranscriptSlot {
    match transcript::locate(claude_dir, cwd, session_id) {
        Some(path) => TranscriptSlot::Found(Box::new(transcript::Reader::new(path))),
        None => TranscriptSlot::Missing(Instant::now()),
    }
}

/// The registry part of a row, with liveness. Background sessions are left
/// out in v1 (§2).
fn from_registry(file: &RegistryFile) -> Option<Instance> {
    let entry = match &file.entry {
        Ok(entry) => entry,
        Err(e) => {
            return Some(Instance {
                pid: file.pid,
                alive: proc::is_alive(file.pid, None),
                state: State::Unknown("error".into()),
                error: Some(format!("{}: {e}", file.path.display())),
                ..Instance::default()
            });
        }
    };
    let kind = entry.kind.clone().unwrap_or_default();
    if kind == "background" {
        return None;
    }
    let pid = entry.pid.unwrap_or(file.pid);
    let alive = proc::is_alive(pid, entry.proc_start.as_deref());
    let state = if alive {
        State::from_status(entry.status.as_deref(), entry.waiting_for.as_deref())
    } else {
        State::Gone
    };
    Some(Instance {
        pid,
        session_id: entry.session_id.clone().unwrap_or_default(),
        cwd: entry.cwd.clone().map(PathBuf::from).unwrap_or_default(),
        kind,
        version: entry.version.clone().unwrap_or_default(),
        name: entry.name.clone().unwrap_or_default(),
        name_source: entry.name_source.clone().unwrap_or_default(),
        state,
        state_since_ms: entry.status_updated_at,
        started_at_ms: entry.started_at,
        alive,
        ..Instance::default()
    })
}

/// The pane holding Claude: the pane id from the registry `tmux` field if
/// that pane exists, else the nearest parent process that is a pane's
/// process (§3.6).
fn find_pane<'a>(
    panes: &'a [Pane],
    tmux_field: Option<&str>,
    pid: u32,
    alive: bool,
) -> Option<&'a Pane> {
    if let Some(id) = tmux_field.and_then(registry::pane_id)
        && let Some(pane) = panes.iter().find(|p| p.pane_id == id)
    {
        return Some(pane);
    }
    if !alive {
        return None;
    }
    proc::ancestors(pid, MAX_ANCESTORS)
        .into_iter()
        .find_map(|ppid| panes.iter().find(|p| p.pane_pid == ppid))
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Entry;

    fn pane(id: &str, pid: u32) -> Pane {
        Pane {
            pane_id: id.into(),
            pane_pid: pid,
            session_id: "$1".into(),
            session_name: "s".into(),
            window_id: "@1".into(),
            ..Pane::default()
        }
    }

    #[test]
    fn pane_from_registry_field() {
        let panes = [pane("%3", 10), pane("%4", 11)];
        let found = find_pane(&panes, Some("s:@1.%4"), 999_999_999, false);
        assert_eq!(found.map(|p| p.pane_id.as_str()), Some("%4"));
    }

    #[test]
    fn pane_from_parent_process() {
        // This test process runs under some parent; pretend it's a pane.
        let parent = proc::read_stat(std::process::id()).unwrap().ppid;
        let panes = [pane("%9", parent)];
        let found = find_pane(&panes, Some("s:@1.%404"), std::process::id(), true);
        assert_eq!(found.map(|p| p.pane_id.as_str()), Some("%9"));
        // A dead process never walks parents.
        assert!(find_pane(&panes, None, std::process::id(), false).is_none());
    }

    #[test]
    fn registry_row() {
        let file = RegistryFile {
            path: "/x/1.json".into(),
            pid: 1,
            entry: Ok(Entry {
                pid: Some(std::process::id()),
                status: Some("waiting".into()),
                waiting_for: Some("permission prompt".into()),
                session_id: Some("abc".into()),
                kind: Some("interactive".into()),
                ..Entry::default()
            }),
        };
        let inst = from_registry(&file).unwrap();
        assert!(inst.alive);
        assert_eq!(inst.state, State::NeedsYou("permission prompt".into()));

        let dead = RegistryFile {
            entry: Ok(Entry {
                pid: Some(std::process::id()),
                proc_start: Some("1".into()),
                status: Some("busy".into()),
                ..Entry::default()
            }),
            ..file.clone()
        };
        let inst = from_registry(&dead).unwrap();
        assert!(!inst.alive);
        assert_eq!(inst.state, State::Gone);

        let bg = RegistryFile {
            entry: Ok(Entry {
                kind: Some("background".into()),
                ..Entry::default()
            }),
            ..file.clone()
        };
        assert!(from_registry(&bg).is_none());

        let broken = RegistryFile {
            entry: Err("expected value at line 1".into()),
            ..file
        };
        let inst = from_registry(&broken).unwrap();
        assert_eq!(inst.state, State::Unknown("error".into()));
        assert!(inst.error.unwrap().contains("expected value"));
    }
}
