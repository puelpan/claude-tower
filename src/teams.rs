//! Agent teams: `<claude>/teams/<team>/config.json` (§3.4).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Map, Value};

use crate::registry::{get_str, get_str_or_int, get_u32};

/// One `members` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub agent_type: String,
    /// `in-process` or `tmux`.
    pub backend_type: String,
    /// `leader`, `in-process`, or a tmux pane id.
    pub tmux_pane_id: String,
    pub cwd: String,
}

impl Member {
    pub fn is_lead(&self) -> bool {
        self.agent_type == "team-lead" || self.name == "team-lead"
    }
}

/// One team config. Every field is optional (§10).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Team {
    /// Folder name, e.g. `session-1f4e2d3c`.
    pub dir_name: String,
    pub lead_session_id: Option<String>,
    /// `leadProcess.pid`.
    pub lead_pid: Option<u32>,
    /// `leadProcess.procStart`.
    pub lead_proc_start: Option<String>,
    pub members: Vec<Member>,
}

/// Reads `<claude>/teams/*/config.json`, re-reading a file only when its
/// mtime or size changed.
#[derive(Debug)]
pub struct Teams {
    dir: PathBuf,
    /// By folder name. Holds only configs seen in the last scan.
    cache: HashMap<String, Cached>,
}

#[derive(Debug)]
struct Cached {
    mtime: SystemTime,
    len: u64,
    /// `None` if it didn't parse.
    team: Option<Team>,
}

impl Teams {
    /// `dir` is `<claude>/teams`.
    pub fn new(dir: PathBuf) -> Teams {
        Teams {
            dir,
            cache: HashMap::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every team whose config parses; a missing folder gives an empty list.
    pub fn scan(&mut self) -> Vec<Team> {
        let mut old = std::mem::take(&mut self.cache);
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = rd
            .filter_map(|d| d.ok()?.file_name().into_string().ok())
            .collect();
        names.sort();

        let mut out = Vec::new();
        for name in names {
            let path = self.dir.join(&name).join("config.json");
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let len = meta.len();
            let cached = old
                .remove(&name)
                .filter(|c| c.mtime == mtime && c.len == len);
            let team = match cached {
                Some(c) => c.team,
                None => match fs::read(&path) {
                    Ok(bytes) => parse_team(&name, &bytes).ok(),
                    // Gone or unreadable: try again next scan.
                    Err(_) => continue,
                },
            };
            if let Some(t) = &team {
                out.push(t.clone());
            }
            self.cache.insert(name, Cached { mtime, len, team });
        }
        out
    }
}

pub fn parse_team(dir_name: &str, bytes: &[u8]) -> Result<Team, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let Value::Object(m) = value else {
        return Err("not a JSON object".to_string());
    };
    let lead = m.get("leadProcess").and_then(Value::as_object);
    let members = match m.get("members") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_object).map(member).collect(),
        _ => Vec::new(),
    };
    Ok(Team {
        dir_name: dir_name.to_string(),
        lead_session_id: get_str(&m, "leadSessionId"),
        lead_pid: lead.and_then(|l| get_u32(l, "pid")),
        lead_proc_start: lead.and_then(|l| get_str_or_int(l, "procStart")),
        members,
    })
}

fn member(m: &Map<String, Value>) -> Member {
    let s = |key| get_str(m, key).unwrap_or_default();
    Member {
        name: s("name"),
        agent_type: s("agentType"),
        backend_type: s("backendType"),
        tmux_pane_id: s("tmuxPaneId"),
        cwd: s("cwd"),
    }
}

/// The team led by an instance. Matches `leadProcess` (pid and procStart)
/// first, because after `/clear` the registry gets a new `sessionId` while
/// the team keeps the old `leadSessionId`; falls back to `leadSessionId`.
pub fn team_of<'a>(
    teams: &'a [Team],
    pid: u32,
    proc_start: Option<&str>,
    session_id: &str,
) -> Option<&'a Team> {
    let same_session =
        |t: &Team| !session_id.is_empty() && t.lead_session_id.as_deref() == Some(session_id);
    let by_process: Vec<&Team> = teams
        .iter()
        .filter(|t| {
            t.lead_pid == Some(pid)
                // Pid alone only when one side has no procStart.
                && match (t.lead_proc_start.as_deref(), proc_start) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                }
        })
        .collect();
    // Two teams for one process: prefer the one with this session id.
    let found = by_process.iter().find(|t| same_session(t));
    found
        .or(by_process.first())
        .copied()
        .or_else(|| teams.iter().find(|t| same_session(t)))
}

/// Names of the team's members other than the lead, in config order.
/// Members without a name are left out.
pub fn teammate_names(team: &Team) -> Vec<String> {
    team.members
        .iter()
        .filter(|m| !m.is_lead() && !m.name.is_empty())
        .map(|m| m.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/2.1.322");

    fn fixture(dir: &str) -> Team {
        let bytes = fs::read(format!("{FIXTURES}/teams/{dir}/config.json")).unwrap();
        parse_team(dir, &bytes).unwrap()
    }

    fn lead(cwd: &str) -> Member {
        Member {
            name: "team-lead".into(),
            agent_type: "team-lead".into(),
            backend_type: "in-process".into(),
            tmux_pane_id: "leader".into(),
            cwd: cwd.into(),
        }
    }

    #[test]
    fn real_shape() {
        assert_eq!(
            fixture("session-1f4e2d3c"),
            Team {
                dir_name: "session-1f4e2d3c".into(),
                lead_session_id: Some("1f4e2d3c-5b6a-4c7d-8e9f-0a1b2c3d4e5f".into()),
                lead_pid: Some(1228510),
                lead_proc_start: Some("13982212".into()),
                members: vec![lead("/home/me/projects/shop-api")],
            }
        );
    }

    #[test]
    fn teammates_in_process_and_tmux() {
        let t = fixture("session-7c1e5a90");
        assert_eq!(t.members.len(), 3);
        assert!(t.members[0].is_lead());
        let reviewer = &t.members[1];
        assert_eq!(reviewer.backend_type, "in-process");
        assert_eq!(reviewer.tmux_pane_id, "in-process");
        assert!(!reviewer.is_lead());
        let tmux = &t.members[2];
        assert_eq!(tmux.name, "api-tests");
        assert_eq!(tmux.backend_type, "tmux");
        assert_eq!(tmux.tmux_pane_id, "%55");
        assert_eq!(teammate_names(&t), ["reviewer", "api-tests"]);
    }

    #[test]
    fn tmux_teammate_inline() {
        let t = parse_team(
            "session-1",
            br#"{"members":[
                {"name":"team-lead","agentType":"team-lead","tmuxPaneId":"leader"},
                {"name":"worker","agentType":"general-purpose",
                 "backendType":"tmux","tmuxPaneId":"%55","cwd":"/w"}]}"#,
        )
        .unwrap();
        assert_eq!(
            t.members[1],
            Member {
                name: "worker".into(),
                agent_type: "general-purpose".into(),
                backend_type: "tmux".into(),
                tmux_pane_id: "%55".into(),
                cwd: "/w".into(),
            }
        );
        assert_eq!(teammate_names(&t), ["worker"]);
    }

    #[test]
    fn lenient_parse() {
        let t = parse_team(
            "x",
            br#"{"leadSessionId":5,"leadProcess":{"pid":"1","procStart":42},
                "members":[{}, "junk", null, {"name":"a","cwd":7,"extra":true}],
                "unknown":{"a":1}}"#,
        )
        .unwrap();
        assert_eq!(t.dir_name, "x");
        assert_eq!(t.lead_session_id, None);
        assert_eq!(t.lead_pid, None);
        assert_eq!(t.lead_proc_start.as_deref(), Some("42"));
        assert_eq!(
            t.members,
            [
                Member::default(),
                Member {
                    name: "a".into(),
                    ..Member::default()
                }
            ]
        );
        assert_eq!(teammate_names(&t), ["a"]);

        assert_eq!(
            parse_team("x", b"{}").unwrap(),
            Team {
                dir_name: "x".into(),
                ..Team::default()
            }
        );
        let t = parse_team("x", br#"{"members":{},"leadProcess":[]}"#).unwrap();
        assert!(t.members.is_empty());
        assert_eq!(t.lead_pid, None);
    }

    #[test]
    fn invalid() {
        assert!(parse_team("x", b"").is_err());
        assert!(parse_team("x", b"{").is_err());
        assert!(parse_team("x", b"[]").is_err());
        assert!(parse_team("x", b"null").is_err());
    }

    #[test]
    fn lead_excluded_by_type_or_name() {
        let mk = |name: &str, agent_type: &str| Member {
            name: name.into(),
            agent_type: agent_type.into(),
            ..Member::default()
        };
        let t = Team {
            members: vec![
                mk("team-lead", "team-lead"),
                mk("boss", "team-lead"),
                mk("team-lead", "other"),
                mk("b", "x"),
                mk("", "x"),
                mk("a", "x"),
            ],
            ..Team::default()
        };
        assert_eq!(teammate_names(&t), ["b", "a"]);
    }

    fn team(dir: &str, sid: Option<&str>, pid: Option<u32>, ps: Option<&str>) -> Team {
        Team {
            dir_name: dir.into(),
            lead_session_id: sid.map(Into::into),
            lead_pid: pid,
            lead_proc_start: ps.map(Into::into),
            members: Vec::new(),
        }
    }

    fn dir(t: Option<&Team>) -> Option<&str> {
        t.map(|t| t.dir_name.as_str())
    }

    #[test]
    fn team_of_by_process() {
        let teams = [
            team("a", Some("s-a"), Some(10), Some("100")),
            team("b", Some("s-b"), Some(20), Some("200")),
        ];
        assert_eq!(dir(team_of(&teams, 20, Some("200"), "other")), Some("b"));
        // Missing procStart on either side: pid alone.
        assert_eq!(dir(team_of(&teams, 20, None, "other")), Some("b"));
        let loose = [team("c", None, Some(30), None)];
        assert_eq!(dir(team_of(&loose, 30, Some("300"), "")), Some("c"));
    }

    #[test]
    fn team_of_by_session_id() {
        let teams = [
            team("a", Some("s-a"), None, None),
            team("b", Some("s-b"), Some(20), Some("200")),
        ];
        assert_eq!(dir(team_of(&teams, 99, Some("1"), "s-a")), Some("a"));
        assert_eq!(dir(team_of(&teams, 99, Some("1"), "s-b")), Some("b"));
        assert_eq!(dir(team_of(&teams, 99, Some("1"), "s-c")), None);
        // An empty session id never matches.
        let blank = [team("e", Some(""), None, None)];
        assert_eq!(dir(team_of(&blank, 99, None, "")), None);
    }

    #[test]
    fn team_of_after_clear() {
        // The team keeps the old session id; the process still matches.
        let teams = [
            team("old", Some("s-old"), Some(10), Some("100")),
            team("x", Some("s-x"), Some(11), Some("110")),
        ];
        assert_eq!(dir(team_of(&teams, 10, Some("100"), "s-new")), Some("old"));
    }

    #[test]
    fn team_of_reused_pid() {
        // Same pid, different procStart: another process.
        let teams = [team("a", Some("s-a"), Some(10), Some("100"))];
        assert_eq!(dir(team_of(&teams, 10, Some("999"), "s-b")), None);
        // But the session id still matches.
        assert_eq!(dir(team_of(&teams, 10, Some("999"), "s-a")), Some("a"));
    }

    #[test]
    fn team_of_prefers_process_then_session() {
        let teams = [
            team("p1", Some("s-1"), Some(10), Some("100")),
            team("p2", Some("s-2"), Some(10), Some("100")),
            team("s", Some("s-3"), Some(50), Some("500")),
        ];
        assert_eq!(dir(team_of(&teams, 10, Some("100"), "s-2")), Some("p2"));
        assert_eq!(dir(team_of(&teams, 10, Some("100"), "s-9")), Some("p1"));
        // A process match beats another team's session id.
        assert_eq!(dir(team_of(&teams, 10, Some("100"), "s-3")), Some("p1"));
    }

    #[test]
    fn fixtures_end_to_end() {
        let mut teams = Teams::new(PathBuf::from(format!("{FIXTURES}/teams")));
        let teams = teams.scan();
        let dirs: Vec<&str> = teams.iter().map(|t| t.dir_name.as_str()).collect();
        assert_eq!(
            dirs,
            ["session-1f4e2d3c", "session-4a8b2c1d", "session-7c1e5a90"]
        );

        let mut reg = Registry::new(PathBuf::from(format!("{FIXTURES}/registry")));
        let got: Vec<(u32, Option<&str>)> = reg
            .scan()
            .unwrap()
            .into_iter()
            .map(|f| {
                let e = f.entry.unwrap();
                let t = team_of(
                    &teams,
                    f.pid,
                    e.proc_start.as_deref(),
                    e.session_id.as_deref().unwrap_or(""),
                );
                (f.pid, dir(t))
            })
            .collect();
        assert_eq!(
            got,
            [
                (1228510, Some("session-1f4e2d3c")),
                // After /clear: new session id, same process.
                (1230001, Some("session-7c1e5a90")),
                // Stale team with this pid but another procStart.
                (1230002, None),
                (1230003, None),
            ]
        );
    }

    #[test]
    fn scan_cache_and_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut teams = Teams::new(tmp.path().join("teams"));
        assert!(teams.scan().is_empty());

        let d = tmp.path().join("teams");
        fs::create_dir_all(d.join("session-a")).unwrap();
        fs::create_dir_all(d.join("session-b")).unwrap();
        fs::create_dir_all(d.join("empty")).unwrap();
        fs::write(d.join("stray.json"), "{}").unwrap();
        fs::write(
            d.join("session-a/config.json"),
            r#"{"leadSessionId":"s-a"}"#,
        )
        .unwrap();
        fs::write(d.join("session-b/config.json"), "{broken").unwrap();
        let got = teams.scan();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].dir_name, "session-a");
        assert_eq!(teams.cache.len(), 2);

        // Same mtime and length: cached.
        let path = d.join("session-a/config.json");
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, r#"{"leadSessionId":"s-z"}"#).unwrap();
        let f = fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(mtime).unwrap();
        assert_eq!(teams.scan()[0].lead_session_id.as_deref(), Some("s-a"));
        f.set_modified(mtime + std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(teams.scan()[0].lead_session_id.as_deref(), Some("s-z"));

        // Removed folders drop out of the cache.
        fs::remove_dir_all(d.join("session-b")).unwrap();
        teams.scan();
        assert_eq!(teams.cache.len(), 1);
        fs::remove_dir_all(&d).unwrap();
        assert!(teams.scan().is_empty());
        assert!(teams.cache.is_empty());
    }
}
