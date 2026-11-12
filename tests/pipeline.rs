//! End to end: a fake Claude folder → one collector round → rows.
//!
//! The registry entry points at this test process, so it counts as alive.

use std::fs;
use std::path::Path;

use claude_tower::collector::Collector;
use claude_tower::config::Config;
use claude_tower::model::{GroupBy, State, build_groups};
use claude_tower::{list, proc};

const SESSION: &str = "11111111-2222-3333-4444-555555555555";
const OLD_SESSION: &str = "99999999-2222-3333-4444-555555555555";

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn config(dir: &Path) -> Config {
    Config {
        claude_dir: Some(dir.to_string_lossy().into_owned()),
        ..Config::default()
    }
}

/// A Claude folder with this process registered as a waiting instance.
fn fake_claude(dir: &Path, cwd: &Path) {
    let me = std::process::id();
    let start = proc::read_stat(me).unwrap().start_ticks;
    let cwd = cwd.to_string_lossy();
    write(
        &dir.join(format!("sessions/{me}.json")),
        &format!(
            r#"{{"pid":{me},"procStart":"{start}","sessionId":"{SESSION}","cwd":"{cwd}",
            "kind":"interactive","name":"proj-12","nameSource":"derived","status":"waiting",
            "waitingFor":"permission prompt","statusUpdatedAt":1794415203184,
            "startedAt":1794368660331,"version":"9.0.0","futureField":[1,2]}}"#
        ),
    );
    // A dead instance: right pid, wrong start time.
    write(
        &dir.join(format!("sessions/{}.json", me + 1)),
        &format!(
            r#"{{"pid":{},"procStart":"1","sessionId":"x","cwd":"/","kind":"interactive","status":"busy"}}"#,
            me + 1
        ),
    );
    // A broken file and a key file that must be left alone.
    write(&dir.join("sessions/7.json"), "{not json");
    write(&dir.join(format!("sessions/{me}.abc.key")), "secret");

    let slug: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    write(
        &dir.join(format!("projects/{slug}/{SESSION}.jsonl")),
        concat!(
            r#"{"type":"ai-title","aiTitle":"Fix the importer","sessionId":"s"}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"hi"},"timestamp":"2026-11-11T16:56:59.538Z","gitBranch":"feat/import"}"#,
            "\n",
            r#"{"type":"pr-link","prNumber":42,"prUrl":"https://example.com/pr/42","prRepository":"o/r"}"#,
            "\n",
            r#"{"type":"system","subtype":"away_summary","content":"Importer fixed; tests pass.","timestamp":"2026-11-11T17:00:00.000Z"}"#,
            "\n",
        ),
    );
    // The team keeps the session id from before a /clear; the lead process
    // still matches.
    write(
        &dir.join(format!("teams/session-{}/config.json", &OLD_SESSION[..8])),
        &format!(
            r#"{{"leadSessionId":"{OLD_SESSION}","leadProcess":{{"pid":{me},"procStart":"{start}"}},
            "members":[{{"name":"team-lead","agentType":"team-lead","backendType":"in-process","tmuxPaneId":"leader"}},
            {{"name":"reviewer","agentType":"general-purpose","backendType":"in-process","tmuxPaneId":"in-process"}}]}}"#
        ),
    );
}

#[test]
fn one_round() {
    let claude = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    fake_claude(claude.path(), project.path());

    let snap = Collector::new(&config(claude.path())).collect(None);
    let me = std::process::id();

    let mine = snap.instances.iter().find(|i| i.pid == me).unwrap();
    assert!(mine.alive);
    assert_eq!(mine.state, State::NeedsYou("permission prompt".into()));
    assert_eq!(mine.title(), "Fix the importer");
    assert_eq!(mine.branch.as_deref(), Some("feat/import"));
    assert_eq!(mine.recap.as_deref(), Some("Importer fixed; tests pass."));
    assert_eq!(mine.pr.as_ref().map(|p| p.number), Some(42));
    assert_eq!(mine.teammates, vec!["reviewer".to_string()]);
    assert!(mine.transcript.is_some());
    assert_eq!(snap.unverified_versions, vec!["9.0.0".to_string()]);

    let dead = snap.instances.iter().find(|i| i.pid == me + 1).unwrap();
    assert!(!dead.alive);
    assert_eq!(dead.state, State::Gone);

    let broken = snap.instances.iter().find(|i| i.pid == 7).unwrap();
    assert!(broken.error.is_some());

    // Gone rows are hidden unless asked for.
    let groups = build_groups(&snap.instances, GroupBy::None, "", false, snap.taken_at_ms);
    let shown: Vec<u32> = groups[0]
        .rows
        .iter()
        .map(|&i| snap.instances[i].pid)
        .collect();
    assert!(shown.contains(&me));
    assert!(!shown.contains(&(me + 1)));
    // Needs-you sorts first.
    assert_eq!(shown[0], me);

    let text = list::text(&snap, GroupBy::None, false);
    assert!(text.contains("! permission"), "{text}");
    assert!(text.contains("Fix the importer"), "{text}");
    assert!(text.contains("9.0.0"), "{text}");

    let rows = list::rows(&snap, true);
    let json = serde_json::to_value(&rows).unwrap();
    let first = &json[0];
    assert_eq!(first["state"], "needs_you");
    assert_eq!(first["state_detail"], "permission prompt");
}

#[test]
fn missing_claude_folder() {
    let snap = Collector::new(&config(Path::new("/nonexistent/claude-tower-test"))).collect(None);
    assert!(snap.instances.is_empty());
    assert!(snap.registry_error.is_some());
}
