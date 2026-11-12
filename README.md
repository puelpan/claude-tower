# claude-tower

A control tower for Claude Code instances running in tmux: one screen that
shows every running instance as a card, grouped by project with the most
urgent first, and jumps to any of them with Enter. A grouped list with a
details panel is one key (`v`) away.

Read-only: it reads Claude Code's files and asks tmux, and never writes under
`~/.claude` or types into a pane. Design: [spec.md](spec.md).

```
 claude-tower   5 running · 2 needs you · 1 working · 1 shell · 1 idle
 etl-jobs ───────────────────────────────────────────── 1 needs you · 1 shell ─
┏ window 1 ━━━━━━━━━━━━━━━┓╭ window 2 ───────────────╮
┃ ? question          30s ┃│ $ shell              2h │
┃ Schema question         ┃│ Nightly import fix      │
┃ main                    ┃│ main                    │
┗━━━━━━━━━━━━━━━━━━━━━━━━━┛╰─────────────────────────╯
 web-front ────────────────────────────────────────────────────────────────────
╭─────────────────────────╮
│ ! permission         3m │
│ Checkout redesign as s… │
│ feat/checkout-v2        │
╰─────────────────────────╯
 shop-api ──────────────────────────────────────────────── 1 working · 1 idle ─
╭ window 1 ───────────────╮╭ window 2 ───────────────╮
│ ⠋ working           12s ││ · idle new          41m │
│ Issue 882               ││ PR 891 review           │
│ main                    ││ pr-891 · 2 teammates    │
╰─────────────────────────╯╰─────────────────────────╯
 enter jump · ←↓↑→ move · / filter · g group · v list · ? help
```

## Install

```sh
cargo install --path .
```

## Run

As its own tmux session:
```sh
tmux new-session -d -s tower claude-tower
```

Suggested lines for `~/.tmux.conf`:
```tmux
bind-key T   switch-client -t tower                                  # go to the dashboard
bind-key C-t display-popup -E -w 90% -h 85% 'claude-tower --popup'   # or open it as a popup
```
After a jump, tmux's `switch-client -l` (prefix `L`) takes you back.

Commands:
```
claude-tower                 the dashboard
claude-tower --popup         the dashboard; exits after a jump, Esc closes it
claude-tower list            one line per instance (--json, --all, --group tmux|repo|none)
claude-tower doctor          checks the setup
```

## Keys

| Key | Action |
|---|---|
| `j` / `k`, ↓ / ↑ | move (on cards: a row up or down) |
| `h` / `l`, ← / → | move between cards |
| `v` | switch between the card grid and the list |
| Enter | jump to the pane; on a group header, fold it |
| `n` | next row that needs you |
| `/` | filter (Enter keeps it, Esc clears it) |
| `g` | group by project (tmux session) → repository → none |
| Tab | fold or unfold the group (list) |
| `p` | details panel (shown from 110 columns) |
| `a` | show gone instances |
| `r` | refresh now |
| `?` | help |
| `q` | quit; Esc also quits in popup mode when no filter is set |

## Config

Optional `~/.config/claude-tower/config.toml`; every key has a default:
```toml
refresh_ms    = 1000
group_by      = "tmux"     # tmux | repo | none
layout        = "cards"    # cards | list
details_panel = true       # list view; on cards, p shows it
preview_lines = 15
# claude_dir  = "~/.claude"   # default: $CLAUDE_CONFIG_DIR, then ~/.claude
```

## When Claude Code updates

The files read are Claude Code internals, checked against 2.1.322. A newer
version shows `⚠ Claude <version> not verified` in the header; run
`claude-tower doctor` and recheck spec §3. Missing fields empty a column, and
unknown values are shown raw.

## Development

```sh
cargo test
cargo run --release --example bench_transcript -- ~/.claude/projects/*/*.jsonl
cargo run --release --example bench_collector [pane-id]
```
