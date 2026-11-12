//! A hand-built snapshot for UI tests: several groups, every state, a long
//! title, a wide-char title, teammates, `is_new`, a gone row and a preview.

use std::path::PathBuf;

use crate::model::{Instance, PaneInfo, PrLink, Preview, Snapshot, State};

use super::app::App;

pub const NOW: i64 = 1_800_000_000_000;

pub const CHECKOUT: &str = "Checkout redesign as storybook components";
pub const ISSUE: &str = "Issue 882";
pub const REVIEW: &str = "PR 891 review";
pub const SHELL: &str = "Nightly import fix";
pub const QUESTION: &str = "Schema question";
pub const WIDE: &str = "日本語のタイトル 🚀 wide chars";
pub const DETACHED: &str = "Detached run";
pub const GONE: &str = "Old experiment";

const SEC: i64 = 1000;
const MIN: i64 = 60 * SEC;

fn pane(id: &str, session: &str, window: u32) -> Option<PaneInfo> {
    Some(PaneInfo {
        pane_id: id.into(),
        session_id: format!("${window}"),
        session_name: session.into(),
        window_id: format!("@{window}"),
        window_index: window,
        window_name: "claude".into(),
        pane_index: 0,
        title: String::new(),
    })
}

fn instance(pid: u32, title: &str, state: State, ago_ms: i64) -> Instance {
    Instance {
        pid,
        session_id: format!("session-{pid}"),
        cwd: PathBuf::from(format!("/home/me/projects/p{pid}")),
        kind: "interactive".into(),
        version: "2.1.322".into(),
        state,
        state_since_ms: Some(NOW - ago_ms),
        alive: true,
        ai_title: Some(title.into()),
        branch: Some("main".into()),
        ..Instance::default()
    }
}

pub fn snapshot() -> Snapshot {
    let checkout = Instance {
        custom_title: Some(CHECKOUT.into()),
        ai_title: None,
        pane: pane("%1", "web-front", 1),
        branch: Some("feat/checkout-v2".into()),
        pr: Some(PrLink {
            number: 98,
            url: "https://github.com/acme/web-front/pull/98".into(),
            repository: "acme/web-front".into(),
        }),
        permission_mode: Some("auto".into()),
        recap: Some(
            "The checkout flow is in PR #98; its two failing checks are a CI runner \
             outage, not the code."
                .into(),
        ),
        last_prompt: Some("rerun the checks and tell me what failed\nthen fix it".into()),
        ..instance(
            101,
            CHECKOUT,
            State::NeedsYou("permission prompt".into()),
            3 * MIN,
        )
    };
    let issue = Instance {
        pane: pane("%3", "shop-api", 1),
        last_said: Some("Running the test suite now.".into()),
        ..instance(102, ISSUE, State::Working, 12 * SEC)
    };
    let review = Instance {
        pane: pane("%4", "shop-api", 2),
        branch: Some("pr-891".into()),
        is_new: true,
        teammates: vec!["reviewer".into(), "api-tests".into()],
        ..instance(103, REVIEW, State::Idle, 41 * MIN)
    };
    let shell = Instance {
        pane: pane("%5", "etl-jobs", 2),
        ..instance(104, SHELL, State::Shell, 2 * 60 * MIN)
    };
    let wide = Instance {
        pane: pane("%7", "payments-dashboard", 1),
        branch: Some("feature/very-long-branch-name-for-testing".into()),
        ..instance(105, WIDE, State::Working, 5 * MIN)
    };
    let question = Instance {
        pane: pane("%6", "etl-jobs", 1),
        ..instance(
            106,
            QUESTION,
            State::NeedsYou("input needed".into()),
            30 * SEC,
        )
    };
    let detached = Instance {
        branch: None,
        error: Some("transcript not found".into()),
        ..instance(107, DETACHED, State::Unknown("compacting".into()), 7 * MIN)
    };
    let gone = Instance {
        pane: pane("%8", "shop-api", 3),
        alive: false,
        ..instance(108, GONE, State::Idle, 3 * 60 * MIN)
    };
    Snapshot {
        instances: vec![checkout, issue, review, shell, wide, question, detached, gone],
        taken_at_ms: NOW,
        unverified_versions: vec!["2.2.0".into()],
        preview: Some(Preview {
            pane_id: "%1".into(),
            text: "old line\n\x1b[1m⏺ Bash(gh pr checks 98)\x1b[0m\n  ⎿ \x1b[31m2 failing\x1b[0m, 1 pending\n\n"
                .into(),
        }),
        ..Snapshot::default()
    }
}

/// The key of the instance with this title.
pub fn key_of(app: &App, title: &str) -> String {
    app.snapshot
        .instances
        .iter()
        .find(|i| i.title() == title)
        .map(|i| i.key())
        .unwrap()
}
