# claude-tower — Specification

Status: v1 implemented (milestones 1–3), plus a card view (§6.1) · 2026-11-11 · data formats checked against Claude Code 2.1.322

A control tower for Claude Code instances: it sees every one that's running, shows which are waiting for clearance, and takes you to any of them.

## 1. Problem

Claude Code runs in tmux: one tmux session per project, one instance per window or pane, some leading teammates. With more than a handful of instances it's hard to keep track of:
- which projects have instances running;
- what each one is working on;
- which ones are waiting for me (a permission prompt, a question) and which are still working.

Today the only way to find out is to visit each pane and read back through its chat.

## 2. Goals and non-goals

### Goals
- One screen listing every running Claude Code instance of the current user.
- For each instance: what it's working on (title, recap, branch, PR) and its state (working, waiting for me, or idle).
- Instances that need me sort to the top.
- Group by project, filter by text.
- Jump to an instance's tmux pane with one key.
- Runs in its own tmux session, or as a tmux popup.
- Rust, terminal UI.

### Decisions already made
- **Standalone.** It runs as the user who runs Claude Code: no daemon, no socket, no other process to talk to. Desktop notifications are left to other tools.
- **Read-only.** It reads Claude Code's files and asks tmux. It never writes under `~/.claude` and never types into a Claude pane.
- **No hooks.** Claude Code already keeps a live registry of running instances with their state (§3.2), so nothing is added to `settings.json`.
- **Libraries:** ratatui + crossterm, std threads. No async runtime.

### Non-goals (v1)
- Approving permissions or sending prompts from the dashboard.
- Starting, stopping or resuming instances.
- Background sessions (`claude --bg`). `claude agents` already shows those (§15 has them as a later addition).
- Finished sessions and history: only what's running now.
- Desktop notifications: left to other tools (Claude Code's hooks can send them).
- Other users' sessions, and remote or cloud sessions.

## 3. Data sources

§3.2–3.5 are Claude Code's internal files, not a documented API. The formats below were read on 2026-11-11 from version 2.1.322, from live files and from Claude Code's own code. §10 covers surviving changes, §14 lists what's still unverified.

### 3.1 Claude's folder

`$CLAUDE_CONFIG_DIR` if set, otherwise `~/.claude`. Written `<claude>` below.

### 3.2 Session registry: `<claude>/sessions/<pid>.json`

One file per running Claude process. Claude writes it at startup, updates it whenever its state changes, and removes it on a normal exit. The folder is mode 0700.

A real file (trimmed, values changed):
```json
{
  "pid": 1228510,
  "procStart": "13982212",
  "sessionId": "1f4e2d3c-5b6a-4c7d-8e9f-0a1b2c3d4e5f",
  "cwd": "/home/me/projects/shop-api",
  "kind": "interactive",
  "tmux": "shop-api:@3.%3",
  "name": "shop-api-38",
  "nameSource": "derived",
  "status": "idle",
  "startedAt": 1794368660331,
  "statusUpdatedAt": 1794415203184,
  "updatedAt": 1794415203184,
  "version": "2.1.322"
}
```

Fields used:

| Field | Meaning | Used for |
|---|---|---|
| `pid` | Claude's process id | liveness (§3.7) |
| `procStart` | process start time in clock ticks since boot; equals field 22 of `/proc/<pid>/stat` | telling a live process from a reused PID |
| `sessionId` | conversation id | finding the transcript |
| `cwd` | Claude's working folder | project, transcript folder |
| `kind` | `interactive` (in a terminal) or `background` (`claude --bg`) | v1 shows `interactive` only |
| `tmux` | `<session name>:<window id>.<pane id>`, set when Claude started inside tmux | finding the pane (§3.6) |
| `name`, `nameSource` | short name such as `shop-api-38`; `derived` or `user` | fallback label |
| `status` | see below | state (§4) |
| `waitingFor` | why it's waiting; only set when `status` is `waiting` | state detail |
| `statusUpdatedAt` | ms since epoch of the last status change | "for 3m" |
| `startedAt` | ms since epoch | age |
| `version` | Claude Code version that wrote the file | format warning (§10) |

`status` values. Internally Claude maps `running` → `busy`, `requires_action` → `waiting`, `idle` → `idle`, and adds `shell`:

| `status` | Meaning |
|---|---|
| `busy` | working on a turn |
| `waiting` | stopped until I act; `waitingFor` says why |
| `idle` | turn finished, waiting for my next prompt |
| `shell` | turn finished, but a background shell it started is still running (the pane footer says "1 shell still running") |

`waitingFor` values:
- `permission prompt`: a tool needs my approval.
- `input needed`: Claude asked me a question (AskUserQuestion or another dialog tool), or an MCP server wants input.
- `goal proposal`, `sandbox request` (network access), `worker request`, `dialog open`: rarer; shown as-is.

Ignore all other fields. Never open the `<pid>.<hash>.key` files next to the JSON files: they aren't needed and look like secrets for Claude's messaging sockets.

### 3.3 Transcript: `<claude>/projects/<slug>/<sessionId>.jsonl`

One JSON object per line, appended as the conversation goes. `<slug>` is the session's starting `cwd` with every character other than ASCII letters and digits replaced by `-`. That rule was checked for `/` and `.`: `/home/me/projects/web-front/.claude/worktrees/feat-x` becomes `-home-me-projects-web-front--claude-worktrees-feat-x`. Claude's own function works on UTF-16 units (an emoji gives two dashes), and above 200 characters keeps the first 200 and appends `-` and a base-36 hash of the cwd. If the file isn't there, look for `<claude>/projects/*/<sessionId>.jsonl` (newest match), because `cwd` can change after the session starts.

Real transcripts are up to 8 MB and 100–2,300 lines. Most bytes are `user` tool results (up to 800 KB a line), then `assistant`, `api-request-blob` and `attachment` records that the dashboard doesn't need. Subagent transcripts are separate files under `<slug>/<sessionId>/subagents/`.

Records used (for each kind, the last one in the file wins):

| Record | Field(s) | Shown as |
|---|---|---|
| `"type":"custom-title"` | `customTitle` | title I set with `/rename` |
| `"type":"ai-title"` | `aiTitle` | title Claude generated, e.g. "Issue 882"; the same text Claude puts in the pane title |
| `"type":"system"`, `"subtype":"away_summary"` | `content` | recap: one or two sentences on where the work stands (the `※ recap:` line in the pane) |
| `"type":"last-prompt"` | `lastPrompt` | my last prompt (already cut to ~200 chars by Claude) |
| `"type":"pr-link"` | `prNumber`, `prUrl`, `prRepository` | PR badge |
| `"type":"permission-mode"` | `permissionMode` | mode badge (`auto`, `plan`, …) |
| any record with `gitBranch` | `gitBranch` | branch, when the repo's HEAD can't be read |
| `"type":"assistant"` | last `text` block in `message.content` | "last said", shown when there's no recap |
| any record with `timestamp` | `timestamp` | last activity (the newest one: records within a turn are written a few seconds out of order) |

A missing or blank value never replaces an earlier one. This matters for `last-prompt`: over a quarter of real ones have no `lastPrompt`, often the last one in the file.

Each 2.1.322 `assistant` record holds one content block (`tool_use`, `thinking` or `text`), so the newest one usually has no text; "last said" is the last text block of the newest assistant record that has one.

Branch: the repo's live `HEAD` first (§5, `git.rs`), then `gitBranch`. Claude caches `gitBranch` per session, so it can be stale; one session started before its repo's first commit had `"HEAD"` on every line, which is ignored.

Title, in order of preference: `customTitle`, `aiTitle`, the pane title without its leading status glyph (§3.6; Claude's default `Claude Code` doesn't count), a registry `name` I set (`nameSource: "user"`), the first line of `lastPrompt`, the derived registry `name`, the folder name. A derived name like `shop-api-38` says less than the last prompt, so it comes after it. Two of 18 real transcripts had no `ai-title`, and a session right after `/clear` has none.

Reading:
- The first time a transcript is seen, read it all once on the collector thread (§9). Rows appear right away from the registry; titles fill in when the scan finishes. An 8 MB file takes about 6 ms.
- After that, each refresh reads only the bytes added since the saved offset. An incomplete last line is kept until its newline arrives.
- If the file got shorter or its inode changed, read it all again.
- Before calling serde on a line, check with a byte search that it holds one of the record types in the table, then confirm the top-level `type` after parsing. `gitBranch` and `timestamp` are taken by byte search without parsing. For `assistant` records, parse only the newest line in each read that contains a text block, and fall back to older ones if it has no text.
- Skip lines that aren't valid JSON, and lines with `"isSidechain":true`.
- Strip escape sequences and control characters from every value shown.

### 3.4 Teams: `<claude>/teams/<team>/config.json`

Each session has a team named `session-<first 8 characters of sessionId>`, with the session as its lead. Teammates are listed in `members` while they run and removed when they finish.

```json
{
  "leadSessionId": "1f4e2d3c-5b6a-4c7d-8e9f-0a1b2c3d4e5f",
  "members": [
    { "name": "team-lead", "agentType": "team-lead", "backendType": "in-process",
      "tmuxPaneId": "leader", "cwd": "/home/me/projects/shop-api" }
  ]
}
```

- Match a team to its lead by `leadProcess` (`pid` and `procStart`, the same values as the registry), then by `leadSessionId`, never by the folder name. After `/clear` the registry gets a new `sessionId`, but the team folder and its `leadSessionId` keep the old one (seen live).
- Finished teammates leave `members`; their inbox files stay in `<team>/inboxes/`.
- `backendType: "in-process"` (`tmuxPaneId: "in-process"`): the teammate runs inside the lead's process and shows in the lead's pane. Jumping goes to the lead.
- `backendType: "tmux"`: the teammate has its own pane (`tmuxPaneId`) and probably its own registry file. Not seen live yet (§14).

v1 shows teammate names under the lead's row. The state of in-process teammates isn't available in v1.

### 3.5 Task lists: `<claude>/tasks/<team>/`

Task list files per team. All were empty when this was written, so their format is unknown. Not used in v1 (§15).

### 3.6 tmux

One call per refresh:
```
tmux list-panes -a -F '#{pane_id}\t#{pane_pid}\t#{session_id}\t#{session_name}\t#{window_id}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_active}\t#{window_active}\t#{session_attached}\t#{pane_title}'
```
`pane_title` comes last because a title can contain tabs; the line is split into at most 12 fields.

- Find an instance's pane by the pane id at the end of its registry `tmux` field (`%3`). Take session and window names from this list, not from the registry: the registry keeps the session name from when Claude started, which goes stale if the session is renamed.
- If the `tmux` field is missing or its pane is gone, walk up Claude's parent processes (field 4 of `/proc/<pid>/stat`) until one is a `pane_pid`.
- If no pane is found, the row shows location `—` and Enter shows a message.
- Claude sets `pane_title` to `<glyph> <aiTitle>` (for example `✳ Issue 882`); it's a fallback title. Before there's an `aiTitle` it's `✳ Claude Code`, which is ignored.
- Preview: `tmux capture-pane -p -e -J -t %3 -S -<lines>`, for the selected row only, at most once per refresh.

Run tmux with an argument vector, never through a shell, and with a timeout.

### 3.7 Liveness

A registry file can outlive its process (crash, `kill -9`). An instance is alive only if `/proc/<pid>/stat` exists and its field 22 equals `procStart`. Count fields after the last `)` on the line, since the process name can contain spaces and parentheses.

Dead instances are hidden; `a` shows them dimmed as "gone". Never delete their files: that's Claude's job.

### 3.8 `claude agents --json`

The only interface Claude Code documents for scripts. It lists interactive and background sessions with `pid`, `cwd`, `kind`, `sessionId`, `name` and `status`, but no tmux location and no `waitingFor`. Each call starts Claude Code's 340 MB binary, so it isn't used on refresh; only `claude-tower doctor` uses it, to cross-check the registry (§11).

## 4. States

Each row gets one state, from §3.2 and §3.7:

| State | Source | Badge | Sort |
|---|---|---|---|
| Needs you | `waiting` | `! permission` / `? question` / `! <waitingFor>` | 1 |
| Working | `busy` | spinner | 2 |
| Shell | `shell` | `$ shell` | 3 |
| Idle | `idle` | `· idle` | 4 |
| Unknown | any other `status` | `? <raw value>` | 5 |
| Gone | process dead (§3.7) | dimmed | hidden by default |

`permission prompt` shows as `! permission`, `input needed` as `? question`, anything else as its raw value.

Within a group, rows sort by state, then by time in state: the longest-waiting "needs you" first, the most recently finished idle row first.

**New since I looked.** On each refresh, a pane counts as seen if it's the active pane of the active window of an attached session. A row whose last status change happened after the pane was last seen gets a `new` marker: it finished, or started waiting, while I was elsewhere. This is kept in memory only.

## 5. Grouping and filtering

Group modes, cycled with `g`:
1. **tmux session** (default). Matches the one-session-per-project habit, and it's where Enter takes me.
2. **Repository**: the git repo of `cwd`. Worktrees (such as `.claude/worktrees/feat-x`) fold into their main repo, by following the `.git` file to its `gitdir` and then `commondir`. Instances outside a repo group by folder.
3. **None**: one list sorted by state. Answers "what needs me right now".

Groups sort by their most urgent row, then by name. Tab folds a group; a folded group still shows its counts by state.

Filter (`/`): case-insensitive. Every word typed must match somewhere in the title, recap, last prompt, branch, PR, group, tmux session name or window name. The list updates while typing; Esc clears.

## 6. Screen

```
 claude-tower   5 running · 1 needs you · 2 working · 2 idle            ⚠ Claude 2.2.0 not verified
┌ Instances ────────────────────────────────────────────────┐┌ web-front · 2 ────────────────────────────┐
│ ▾ web-front                                  1 needs you  ││ Checkout redesign as storybook components │
│   ! permission    Checkout redesign as st…  feat/che… 3m  ││ feat/checkout-v2 · PR #98 · auto          │
│ ▾ shop-api                                                ││                                           │
│   ⠋ working       Issue 882                 main     12s  ││ Recap: The checkout flow is in PR #98;    │
│   · idle   new    PR 891 review             pr-891   41m  ││ its two failing checks are a CI runner    │
│                   └ teammates: reviewer, api-tests        ││ outage, not the code.                     │
│ ▾ etl-jobs                                                ││                                           │
│   $ shell         Nightly import fix        main      2h  ││ Last prompt: "rerun the checks and …"     │
│ ▸ payments-dashboard                         1 working    ││ ── pane ───────────────────────────────── │
│                                                           ││ ⏺ Bash(gh pr checks 98)                   │
│                                                           ││   ⎿ 2 failing, 1 pending                  │
└───────────────────────────────────────────────────────────┘└───────────────────────────────────────────┘
 enter jump · / filter · g group · n next needing you · tab fold · p panel · a gone · ? help · q quit
```

- **Rows:** badge, `new` marker, title, branch (shortened), time in state. The tmux location (`session:window`) is added when the terminal is wide enough.
- **Details panel** for the selected row: title, branch, PR, mode, recap (or "last said"), last prompt, teammates, then the last lines of the pane. Hidden below 110 columns; `p` toggles it.
- **Header:** counts by state, and a warning when an instance runs a Claude Code version newer than the last verified one (§10).
- **Colors:** needs you in bold red or yellow, working in cyan, idle in the default color, gone dimmed. Use the terminal's 16 colors so it follows the tmux and terminal theme.

Keys:

| Key | Action |
|---|---|
| `j` / `k`, ↓ / ↑ | move |
| Enter | jump to the pane (§7) |
| `n` | move to the next row that needs you |
| `/` | filter; Enter keeps it, Esc clears it |
| `g` | cycle group mode |
| Tab | fold or unfold the group |
| `p` | show or hide the details panel |
| `a` | show or hide gone instances |
| `r` | refresh now |
| `?` | help |
| `q` | quit |

Esc clears the filter when one is set. Otherwise, in popup mode only, it quits.

The spinner turns at 4 frames a second, and the screen is redrawn only on a key, a new snapshot, or a spinner frame change. Faster spinning cost over 1% CPU.

### 6.1 Card view

Added after v1, and the default (`layout = "cards"` in the config; `v` switches between cards and the list above).

```
 claude-tower   5 running · 1 needs you · 2 working · 2 idle
 shop-api ──────────────────────────────────────────── 1 working · 1 idle ─
╭ window 1 ──────────────╮╭ window 2 ──────────────╮
│ ⠋ working          12s ││ · idle new         41m │
│ Issue 882              ││ PR 891 review          │
│ main                   ││ pr-891 · 2 teammates   │
╰────────────────────────╯╰────────────────────────╯
 web-front ─────────────────────────────────────────────────────────────────
┏━━━━━━━━━━━━━━━━━━━━━━━━┓
┃ ! permission        3m ┃
┃ Checkout redesign as … ┃
┃ feat/checkout-v2       ┃
┗━━━━━━━━━━━━━━━━━━━━━━━━┛
```

- One card per instance: state and age, title, branch (and teammate count). The border color follows the state; the selected card has a thick border.
- Grouped (`g`: project → repo → none), each group gets its own row under a heading across the width, with counts when it has several cards. Cards in a shared session are labeled by window. Ungrouped, the cards flow in one grid labeled by session.
- Cards are at least 26 columns wide. Arrows or `hjkl` move: left and right in order, up and down to the nearest card in the next row. The grid scrolls by rows and shows "↓ N more".
- The details panel starts hidden here; `p` shows it.

## 7. Jumping

Enter switches my tmux client to the instance's pane in one tmux call:

```
tmux switch-client -c <client> -t <session_id> \; select-window -t <window_id> \; select-pane -t <pane_id>
```

- `<client>` is the client showing the dashboard: `tmux list-clients -t <dashboard's session>`, most recent `client_activity` first. In popup mode, use `tmux display-message -p '#{client_name}'`, which should return the client that opened the popup (§14).
- In popup mode, exit after jumping; that closes the popup.
- Outside tmux (`$TMUX` unset), Enter shows `tmux attach -t <session>` in the status line instead.
- To get back to the dashboard: tmux's `switch-client -l` (prefix `L` by default), or a key binding (§8).

## 8. Running it

```sh
tmux new-session -d -s tower claude-tower
```

Suggested lines for `~/.tmux.conf` (the user adds them; claude-tower never edits tmux config):
```tmux
bind-key T   switch-client -t tower                                     # go to the dashboard
bind-key C-t display-popup -E -w 90% -h 85% 'claude-tower --popup'      # or open it as a popup
```

Commands:
```
claude-tower            the dashboard
claude-tower --popup    the dashboard; exits after a jump, Esc closes it
claude-tower list       one line per instance, plain text (--json for scripts)
claude-tower doctor     checks the setup (§11)
```

`list` is also milestone 1: it proves the data layer before any UI exists.

## 9. Structure

```
src/
  main.rs        CLI (clap)
  registry.rs    §3.2  sessions/*.json → Instance
  proc.rs        §3.7  liveness, parent pids
  transcript.rs  §3.3  incremental reader → Digest
  teams.rs       §3.4
  tmux.rs        §3.6, §7  list panes, capture, jump
  git.rs         §5    branch fallback, repo root, worktree folding
  model.rs       §4–5  merge sources into rows; state, sort, group, filter
  config.rs      §11
  ui/            ratatui: list, details panel, help, input
```

Two threads:
- **Collector:** every `refresh_ms` (default 1000) builds a full snapshot: registry → liveness → tmux panes → transcripts → teams → rows. It sends the snapshot to the UI over a channel and keeps transcript offsets and digests between rounds. It also captures the preview for the pane the UI has selected.
- **UI:** waits for key events with a 100 ms timeout and redraws on a key or a new snapshot. It never calls tmux or reads files itself, so it never blocks.

Work per refresh with 20 instances: one directory read, up to 20 small JSON reads (skipped when mtime is unchanged), one `tmux list-panes`, up to 20 `stat` calls plus the appended transcript bytes, and at most one `capture-pane`.

Dependencies: ratatui, crossterm, serde, serde_json, clap, anyhow, toml.

## 10. When Claude Code changes

Claude Code updates often and §3 is internal. So:
- Every field is optional (`#[serde(default)]`); unknown fields and record types are ignored. A missing field empties one column, it doesn't drop the row.
- A file that fails to parse affects only its row, which shows a `?` badge and the error in the details panel.
- Unknown `status` and `waitingFor` values are shown raw, never mapped to a guess.
- The code holds a `VERIFIED_VERSION` (now 2.1.322). If an instance reports a newer `version`, the header shows "⚠ Claude <version> not verified", and `doctor` says what to recheck.
- Tests use fixtures copied from real files, one folder per verified version (`tests/fixtures/2.1.322/`), with conversation text replaced.

## 11. Config and doctor

Optional `~/.config/claude-tower/config.toml`. Every key has a default:
```toml
refresh_ms    = 1000
group_by      = "tmux"     # tmux | repo | none
details_panel = true
preview_lines = 15
# claude_dir  = "~/.claude"   # default: $CLAUDE_CONFIG_DIR, then ~/.claude
```

`claude-tower doctor` prints one line per check:
- Claude's folder and the registry folder exist and are readable.
- Registry files parse; versions seen compared with `VERIFIED_VERSION`.
- The set of interactive sessions matches `claude agents --json` (§3.8).
- Each instance's transcript is found.
- tmux is reachable and each instance maps to a pane.

## 12. Reused code

The tmux runner (argument vectors, timeout, the one-call jump), the branch reader that follows worktree `.git` files, and the JSONL line handling were adapted from an earlier tool. claude-tower reads transcripts forward from an offset rather than backward from the end.

## 13. Security and privacy

- Runs as the current user and reads only that user's files.
- Read-only: no writes under `<claude>`, no `send-keys` to any pane.
- Never opens `<pid>.<hash>.key` files or `.credentials.json`.
- No network access.
- Transcripts can contain secrets. Show only the fields in §3.3, cut to the panel's width, and never log transcript content.

## 14. Verified so far (2026-11-11, Claude Code 2.1.322)

Checked on the live system:
- One registry file per running instance. Files disappear on exit: two instances closed during the check and their files went away.
- `procStart` equals field 22 of `/proc/<pid>/stat`.
- The `tmux` field has the form `session:@window.%pane` and matches `tmux list-panes`.
- Statuses seen live: `busy`, `idle`, `shell`. `shell` matched a pane whose footer said "1 shell still running".
- Real transcripts contain `ai-title`, `last-prompt`, `pr-link`, `permission-mode` and `system`/`away_summary` records, and `gitBranch` on each message line.
- The slug rule holds for `/` and `.`.
- `claude agents` (its interactive view) lists only background sessions. With 5 instances running in tmux it showed "0 awaiting input · 0 working". `claude agents --json` includes the interactive ones, without tmux location or `waitingFor`.
- Team configs exist per session, with a lead entry (`backendType: "in-process"`, `tmuxPaneId: "leader"`).

Checked during milestone 1 (same day):
- `waiting` with `waitingFor: "input needed"`, live, from an instance that asked a question; it showed as `? question` at the top.
- `/clear` gives the registry a new `sessionId` and starts a new transcript; the team keeps the old id (§3.4). The registry is re-read every round, so the lookup follows it. `<claude>/tasks/<new session>/.session-list` names the old team.
- Inside `display-popup`, `tmux display-message -p '#{client_name}'` returns the client that opened the popup. Enter from the popup switched that client to the exact pane and closed the popup; Enter from the dashboard session did the same without closing.
- `claude agents --json` lists the same interactive sessions as the registry (`doctor`).
- A real worktree under `.claude/worktrees/` folds into its main repo.
- Registry files carry more fields than §3.2 lists (`peerProtocol`, `peerFeatures`, `entrypoint`, `pidDomain`, `messagingSocketPath`, `nameSince`, sometimes `bridgeSessionId`); they're ignored.
- tmux session names can be plain numbers (`0:@14.%14`).
- Idle CPU with 6 instances (3 working): 0.78% (schedstat, 30 s). A collector round costs about 1 ms of CPU.

Read from Claude Code's code, not seen live yet:
- `waitingFor: "permission prompt"` and the rarer values.
- `custom-title` / `customTitle`.
- `backendType: "tmux"` for teammates.

Still to check:
1. A permission prompt: `waiting` with `permission prompt` should show as `! permission` within 2 s.
2. `/resume` in a running instance (`/clear` is done).
3. A teammate with `backendType: "tmux"`: the format of its `tmuxPaneId`, and whether it gets its own registry file. If it does, nest it under the lead rather than listing it twice.
4. `/rename`: `customTitle` appears in the transcript, and the registry shows `nameSource: "user"`.
5. Task list file format in `<claude>/tasks/`, once a session has tasks.

## 15. Milestones

1. **Data layer and `list`:** registry, liveness, tmux mapping, transcript digest, grouping. `claude-tower list` prints the table. Do the §14 checks here.
2. **TUI:** grouped list with states, details panel with pane preview, Enter jump, filter, `n`.
3. **Polish:** teammates under their lead, `new` marker, popup mode, `doctor`, config file.
4. **Later, not v1:**
   - Background sessions (`kind: background`): list them; Enter opens a tmux window running `claude attach <id>`.
   - Task progress from `<claude>/tasks/`, such as "3/7 done".
   - A desktop notification when an instance has needed me for more than N minutes.
   - inotify instead of polling.

## 16. Testing

Unit tests with fixtures:
- Registry: every `status` and `waitingFor` value, missing fields, unknown fields, unknown status, invalid JSON.
- `/proc/<pid>/stat` parsing with a process name that contains spaces and `)`.
- Transcript: each record type in §3.3, last-one-wins, a partial last line completed by a later read, a file that shrinks, malformed lines.
- Slug function.
- tmux `list-panes` parsing and jump arguments.
- Model: state mapping, sort order, every group mode, worktree folding, filter matching.

UI: ratatui `TestBackend` snapshots of a fixed snapshot, at narrow and wide sizes.

Live checks, with a few real instances:
- A permission prompt shows as "needs you" within 2 s and sorts first.
- `kill -9` on an instance: its row disappears within 2 s and shows dimmed with `a`.
- Renaming a tmux session updates the group name.
- Enter works from the dashboard's own session and from the popup.

## 17. Acceptance criteria

- Every running instance appears within 1 s of starting the dashboard, in the right group. In repository mode, worktree instances fold into their repo.
- An instance waiting for a permission or an answer shows as "needs you" within 2 s, at the top of its group and in the header count.
- Enter lands on the exact pane; in popup mode the popup closes.
- A dead process stops showing as running within 2 s, even if its registry file remains.
- A broken registry file or transcript line never crashes the program.
- With 10 instances, idle CPU stays under 1% (`pidstat -p <pid> 10`).
- Nothing under `<claude>` is ever written, and `*.key` files are never opened.

## 18. Open questions

My default is in bold; change any of them before milestone 2.
1. Default grouping: **tmux session** or repository?
2. Main way to open it: its own session, a popup, or **both, each on a key**?
3. In-process teammates: **names under the lead**, or their own rows (needs reading their transcripts, and their state is unknown)?
4. Should the dashboard ring the terminal bell when something starts needing me? **No**: desktop notifications are better left to a notifier.
