//! Drawing: header, list, details panel, footer and help. Reads `App` and
//! the clock it's given; changes nothing but the scroll offset. The card
//! grid is in `cards.rs`.

use ratatui::Frame;
use ratatui::buffer::CellWidth;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::model::{Counts, Group, GroupBy, Instance, Snapshot, State, fmt_age};

use super::app::{App, Item, is_gone};
use super::cards;

/// The details panel shows from this terminal width.
pub const PANEL_MIN_WIDTH: u16 = 110;
/// Rows show `session:window` from this list width.
pub const LOCATION_MIN_WIDTH: u16 = 100;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Time per spinner frame. Each frame is a redraw, so it is kept slow.
pub(crate) const SPINNER_MS: i64 = 250;
/// Badge column, gap included.
const BADGE_W: usize = 13;
/// `new` column, gap included.
const NEW_W: usize = 4;
const AGE_W: usize = 4;
const LOCATION_W: usize = 18;
const RECAP_LINES: usize = 6;
const PROMPT_LINES: usize = 3;

const LIST_HINTS: [(&str, &str); 9] = [
    ("enter", "jump"),
    ("/", "filter"),
    ("g", "group"),
    ("n", "next needing you"),
    ("tab", "fold"),
    ("v", "cards"),
    ("p", "panel"),
    ("a", "gone"),
    ("q", "quit"),
];

const CARD_HINTS: [(&str, &str); 9] = [
    ("enter", "jump"),
    ("←↓↑→", "move"),
    ("/", "filter"),
    ("g", "group"),
    ("v", "list"),
    ("n", "next needing you"),
    ("p", "panel"),
    ("a", "gone"),
    ("q", "quit"),
];

const HELP: [(&str, &str); 15] = [
    ("j k ↓ ↑", "move"),
    ("h l ← →", "move between cards"),
    ("enter", "jump to the pane; on a group, fold it"),
    ("n", "next instance that needs you"),
    ("/", "filter; enter keeps it, esc clears it"),
    ("esc", "clear the filter; in a popup, close"),
    ("v", "switch between cards and the list"),
    ("g", "group by tmux session, repo, or none"),
    ("tab", "fold or unfold the group (list)"),
    ("p", "show or hide the details panel"),
    ("a", "show or hide gone instances"),
    ("r", "refresh now"),
    ("home end", "first or last line"),
    ("?", "this help"),
    ("q ctrl-c", "quit"),
];

pub fn draw(frame: &mut Frame, app: &App, now_ms: i64) {
    let area = frame.area();
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(frame, top, &app.snapshot);
    if app.show_panel && area.width >= PANEL_MIN_WIDTH {
        let [list, panel] =
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                .areas(body);
        draw_main(frame, list, app, now_ms);
        draw_panel(frame, panel, app, now_ms);
    } else {
        draw_main(frame, body, app, now_ms);
    }
    draw_footer(frame, bottom, app);
    if app.help {
        draw_help(frame, area);
    }
}

fn draw_main(frame: &mut Frame, area: Rect, app: &App, now_ms: i64) {
    if app.cards() {
        cards::draw(frame, area, app, now_ms);
    } else {
        draw_list(frame, area, app, now_ms);
    }
}

// --- header ---

fn draw_header(frame: &mut Frame, area: Rect, snap: &Snapshot) {
    let width = area.width as usize;
    let right = join_fitting(warnings(snap), "  ", width / 2);
    let right_w = spans_width(&right);

    let counts = Counts::of(&snap.instances);
    let mut parts = vec![Span::raw(format!("{} running", counts.running()))];
    parts.extend(count_parts(&counts, false));
    let mut left = vec![Span::styled(" claude-tower", bold()), Span::raw("   ")];
    let left_max = width.saturating_sub(16 + if right_w > 0 { right_w + 3 } else { 0 });
    left.extend(join_fitting(parts, " · ", left_max));
    frame.render_widget(Paragraph::new(Line::from(left)), area);

    if right_w > 0 && right_w < width {
        let x = area.right() - right_w as u16 - 1;
        let rect = Rect {
            x,
            width: right_w as u16,
            ..area
        };
        frame.render_widget(Paragraph::new(Line::from(right)), rect);
    }
}

/// Header warnings, most important first.
fn warnings(snap: &Snapshot) -> Vec<Span<'static>> {
    let warn = Style::new().fg(Color::Yellow);
    let mut out = Vec::new();
    if let Some(e) = &snap.tmux_error {
        out.push(Span::styled(format!("⚠ tmux: {}", clean(e)), warn));
    }
    if let Some(e) = &snap.registry_error {
        out.push(Span::styled(format!("⚠ registry: {}", clean(e)), warn));
    }
    if !snap.unverified_versions.is_empty() {
        let versions = snap.unverified_versions.join(", ");
        out.push(Span::styled(
            format!("⚠ Claude {versions} not verified"),
            warn,
        ));
    }
    if snap.transcripts_pending > 0 {
        out.push(Span::styled("reading transcripts…", dim()));
    }
    out
}

/// Nonzero counts, colored by state: `2 needs you`, `1 working`, …
pub(super) fn count_parts(c: &Counts, with_gone: bool) -> Vec<Span<'static>> {
    let gone = if with_gone { c.gone } else { 0 };
    [
        (c.needs_you, "needs you", bold().fg(Color::Yellow)),
        (c.working, "working", Style::new().fg(Color::Cyan)),
        (c.shell, "shell", Style::new().fg(Color::Blue)),
        (c.idle, "idle", Style::new()),
        (c.unknown, "unknown", Style::new().fg(Color::Magenta)),
        (gone, "gone", dim()),
    ]
    .into_iter()
    .filter(|(n, _, _)| *n > 0)
    .map(|(n, label, style)| Span::styled(format!("{n} {label}"), style))
    .collect()
}

/// Joins `parts` with `sep` in at most `max` columns. Parts that don't fit
/// are dropped and replaced by `…`; a lone part is cut instead.
pub(super) fn join_fitting(
    parts: Vec<Span<'static>>,
    sep: &'static str,
    max: usize,
) -> Vec<Span<'static>> {
    let sep_w = text_width(sep);
    let total: usize = parts.iter().map(|p| text_width(&p.content)).sum::<usize>()
        + sep_w * parts.len().saturating_sub(1);
    let mut out = Vec::new();
    if total <= max {
        for (k, p) in parts.into_iter().enumerate() {
            if k > 0 {
                out.push(Span::styled(sep, dim()));
            }
            out.push(p);
        }
        return out;
    }
    let mut w = 0;
    for p in parts {
        let pw = text_width(&p.content);
        let sep_here = if out.is_empty() { 0 } else { sep_w };
        // Room for this part plus the trailing ` · …`.
        if w + sep_here + pw + sep_w + 1 > max {
            if out.is_empty() {
                let style = p.style;
                out.push(Span::styled(truncate(&p.content, max), style));
            } else {
                out.push(Span::styled(format!("{sep}…"), dim()));
            }
            break;
        }
        if sep_here > 0 {
            out.push(Span::styled(sep, dim()));
        }
        w += sep_here + pw;
        out.push(p);
    }
    out
}

// --- list ---

/// Column widths of an instance row.
struct Columns {
    width: usize,
    indent: usize,
    title: usize,
    branch: usize,
    location: usize,
}

impl Columns {
    fn new(width: usize, wide: bool, grouped: bool) -> Columns {
        let indent = if grouped { 3 } else { 1 };
        let branch = match width {
            100.. => 20,
            70..=99 => 14,
            50..=69 => 10,
            _ => 0,
        };
        let location = if wide { LOCATION_W } else { 0 };
        let gap = |n: usize| if n > 0 { n + 1 } else { 0 };
        let fixed = indent + BADGE_W + NEW_W + 1 + AGE_W + 1;
        let title = width.saturating_sub(fixed + gap(branch) + gap(location));
        Columns {
            width,
            indent,
            title,
            branch,
            location,
        }
    }

    fn title_x(&self) -> usize {
        self.indent + BADGE_W + NEW_W
    }
}

fn draw_list(frame: &mut Frame, area: Rect, app: &App, now_ms: i64) {
    let mut block = Block::bordered().title(" Instances ");
    if !app.filter.is_empty() && !app.editing {
        let max = (area.width as usize).saturating_sub(24);
        let filter = Line::from(format!(" filter: {} ", truncate(&app.filter, max)));
        block = block.title_top(filter.right_aligned());
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let items = app.items();
    if items.is_empty() {
        draw_empty(frame, inner, app);
        return;
    }

    // Scroll just enough to keep the selection visible.
    let height = inner.height as usize;
    let cursor = app.cursor();
    let mut top = app.scroll.get().min(items.len().saturating_sub(height));
    if let Some(c) = cursor {
        if c < top {
            top = c;
        } else if c >= top + height {
            top = c + 1 - height;
        }
    }
    app.scroll.set(top);

    let cols = Columns::new(
        inner.width as usize,
        area.width >= LOCATION_MIN_WIDTH,
        app.group_by != GroupBy::None,
    );
    for (k, item) in items.iter().enumerate().skip(top).take(height) {
        let line = match *item {
            Item::Header(g) => header_line(app, &app.groups()[g], cols.width),
            Item::Row(_, i) => row_line(&app.snapshot.instances[i], &cols, now_ms),
            Item::Teammates(i) => teammates_line(&app.snapshot.instances[i], &cols),
        };
        let mut row = Paragraph::new(line);
        if cursor == Some(k) {
            row = row.style(Style::new().add_modifier(Modifier::REVERSED));
        }
        let rect = Rect {
            y: inner.y + (k - top) as u16,
            height: 1,
            ..inner
        };
        frame.render_widget(row, rect);
    }
}

/// Why there's nothing to show.
pub(super) fn draw_empty(frame: &mut Frame, area: Rect, app: &App) {
    let msg = if !app.filter.is_empty() {
        "nothing matches the filter"
    } else if app.snapshot.instances.is_empty() {
        "no Claude Code instances running"
    } else {
        "no running instances; a shows gone ones"
    };
    frame.render_widget(Paragraph::new(Line::styled(format!(" {msg}"), dim())), area);
}

/// `▾ name` with the group's counts right-aligned.
fn header_line(app: &App, group: &Group, width: usize) -> Line<'static> {
    let arrow = if app.is_folded(group) { "▸" } else { "▾" };
    let counts = join_fitting(count_parts(&group.counts, true), " · ", width / 2);
    let counts_w = spans_width(&counts);
    let name = truncate(&group.name, width.saturating_sub(3 + counts_w + 2 + 1));
    let gap = width.saturating_sub(3 + text_width(&name) + counts_w + 1);
    // The `(no tmux)` group.
    let no_tmux = app.group_by == GroupBy::Tmux
        && group
            .rows
            .iter()
            .all(|&i| app.snapshot.instances[i].pane.is_none());
    let name_style = if no_tmux { dim() } else { bold() };
    let mut spans = vec![
        Span::raw(format!(" {arrow} ")),
        Span::styled(name, name_style),
        Span::raw(" ".repeat(gap)),
    ];
    spans.extend(counts);
    spans.push(Span::raw(" "));
    Line::from(spans)
}

fn row_line(inst: &Instance, cols: &Columns, now_ms: i64) -> Line<'static> {
    let gone = is_gone(inst);
    let mut spans = vec![
        Span::raw(" ".repeat(cols.indent)),
        Span::styled(pad(&badge(inst, now_ms), BADGE_W - 1), state_style(inst)),
        Span::raw(" "),
    ];
    if inst.is_new && !gone {
        spans.push(Span::styled("new", bold().fg(Color::Green)));
    } else {
        spans.push(Span::raw("   "));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::raw(pad(&inst.title(), cols.title)));
    if cols.branch > 0 {
        let branch = inst.branch.as_deref().unwrap_or_default();
        spans.push(Span::raw(" "));
        spans.push(Span::raw(pad(branch, cols.branch)));
    }
    if cols.location > 0 {
        spans.push(Span::raw(" "));
        spans.push(match inst.location() {
            Some(loc) => Span::raw(pad(&loc, cols.location)),
            None => Span::styled(pad("—", cols.location), dim()),
        });
    }
    let age = inst
        .state_since_ms
        .map(|t| fmt_age(now_ms - t))
        .unwrap_or_default();
    spans.push(Span::raw(" "));
    spans.push(Span::raw(pad_left(&age, AGE_W)));
    spans.push(Span::raw(" "));
    let line = Line::from(spans);
    if gone { line.style(dim()) } else { line }
}

fn teammates_line(inst: &Instance, cols: &Columns) -> Line<'static> {
    let x = cols.title_x();
    let text = format!("└ teammates: {}", inst.teammates.join(", "));
    Line::from(vec![
        Span::raw(" ".repeat(x)),
        Span::styled(truncate(&text, cols.width.saturating_sub(x + 1)), dim()),
    ])
}

/// Badge with a spinner frame before `working`; `gone` for a dead process.
pub(super) fn badge(inst: &Instance, now_ms: i64) -> String {
    if is_gone(inst) {
        return State::Gone.badge();
    }
    match inst.state {
        State::Working => format!("{} {}", spinner(now_ms), inst.state.badge()),
        _ => inst.state.badge(),
    }
}

fn spinner(now_ms: i64) -> &'static str {
    SPINNER[spinner_frame(now_ms).rem_euclid(SPINNER.len() as i64) as usize]
}

/// Changes when the spinner needs a redraw.
pub(crate) fn spinner_frame(now_ms: i64) -> i64 {
    now_ms.div_euclid(SPINNER_MS)
}

pub(super) fn state_style(inst: &Instance) -> Style {
    if is_gone(inst) {
        return dim();
    }
    match &inst.state {
        State::NeedsYou(w) if w == "permission prompt" => bold().fg(Color::Red),
        State::NeedsYou(_) => bold().fg(Color::Yellow),
        State::Working => Style::new().fg(Color::Cyan),
        State::Shell => Style::new().fg(Color::Blue),
        State::Idle => Style::new(),
        State::Unknown(_) => Style::new().fg(Color::Magenta),
        State::Gone => dim(),
    }
}

// --- details panel ---

fn draw_panel(frame: &mut Frame, area: Rect, app: &App, now_ms: i64) {
    let title = match app.selected_group() {
        Some(g) if !g.name.is_empty() => format!(" {} · {} ", g.name, g.rows.len()),
        _ => " Details ".to_string(),
    };
    let block = Block::bordered().title(truncate(&title, (area.width as usize).saturating_sub(2)));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // One column of margin on each side.
    let text = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    if text.is_empty() {
        return;
    }
    let (width, height) = (text.width as usize, text.height as usize);

    let mut lines = match (app.selected_instance(), app.selected_group()) {
        (Some(inst), _) => instance_details(inst, width, now_ms),
        (None, Some(group)) => group_details(app, group, width),
        _ => vec![Line::styled("nothing selected", dim())],
    };
    let pane = app.selected_instance().and_then(|i| i.pane.as_ref());
    if let Some(pane) = pane
        && lines.len() < height
    {
        lines.push(divider(width));
        let room = height - lines.len();
        lines.extend(app.preview_lines(&pane.pane_id, room));
    }
    lines.truncate(height);
    frame.render_widget(Paragraph::new(lines), text);
}

fn instance_details(inst: &Instance, width: usize, now_ms: i64) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = wrap_limited(&inst.title(), width, 2)
        .into_iter()
        .map(|l| Line::styled(l, bold()))
        .collect();

    let mut meta = Vec::new();
    meta.extend(inst.branch.clone());
    meta.extend(inst.pr.as_ref().map(|pr| format!("PR #{}", pr.number)));
    meta.extend(inst.permission_mode.clone());
    if !meta.is_empty() {
        lines.push(Line::raw(truncate(&meta.join(" · "), width)));
    }

    let badge = badge(inst, now_ms);
    let mut rest = String::new();
    if let Some(t) = inst.state_since_ms {
        rest += &format!(" for {}", fmt_age(now_ms - t));
    }
    rest += &format!(" · {}", inst.location().unwrap_or_else(|| "no tmux".into()));
    if inst.pid > 0 {
        rest += &format!(" · pid {}", inst.pid);
    }
    let rest = truncate(&rest, width.saturating_sub(text_width(&badge)));
    lines.push(Line::from(vec![
        Span::styled(badge, state_style(inst)),
        Span::raw(rest),
    ]));

    let said = match (&inst.recap, &inst.last_said) {
        (Some(recap), _) => Some(("Recap:", recap)),
        (None, Some(said)) => Some(("Last said:", said)),
        _ => None,
    };
    if let Some((label, text)) = said {
        lines.push(Line::default());
        lines.extend(labeled(label, text, width, RECAP_LINES, Style::new()));
    }
    if let Some(prompt) = &inst.last_prompt {
        lines.push(Line::default());
        lines.extend(labeled(
            "Last prompt:",
            prompt,
            width,
            PROMPT_LINES,
            Style::new(),
        ));
    }
    if !inst.teammates.is_empty() {
        let names = inst.teammates.join(", ");
        lines.extend(labeled("Teammates:", &names, width, 2, Style::new()));
    }
    if let Some(error) = &inst.error {
        lines.extend(labeled(
            "Error:",
            error,
            width,
            3,
            Style::new().fg(Color::Red),
        ));
    }
    lines
}

fn group_details(app: &App, group: &Group, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(truncate(&group.name, width), bold()),
        Line::from(join_fitting(count_parts(&group.counts, true), " · ", width)),
    ];
    if app.is_folded(group) {
        lines.push(Line::default());
        lines.push(Line::styled(
            truncate("folded · tab or enter unfolds", width),
            dim(),
        ));
    }
    lines
}

/// `Label: text`, wrapped, at most `max` lines, the label bold.
fn labeled(label: &str, text: &str, width: usize, max: usize, style: Style) -> Vec<Line<'static>> {
    wrap_limited(&format!("{label} {text}"), width, max)
        .into_iter()
        .enumerate()
        .map(|(k, line)| match line.strip_prefix(label) {
            Some(rest) if k == 0 => Line::from(vec![
                Span::styled(label.to_string(), bold()),
                Span::styled(rest.to_string(), style),
            ]),
            _ => Line::styled(line, style),
        })
        .collect()
}

fn divider(width: usize) -> Line<'static> {
    let head = "── pane ";
    let rest = width.saturating_sub(text_width(head));
    Line::styled(
        truncate(&format!("{head}{}", "─".repeat(rest)), width),
        dim(),
    )
}

// --- footer and help ---

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    if app.editing {
        let shown = tail(&app.filter, width.saturating_sub(3));
        let shown_w = text_width(&shown);
        let line = Line::from(vec![Span::styled(" /", bold()), Span::raw(shown)]);
        frame.render_widget(Paragraph::new(line), area);
        frame.set_cursor_position((area.x + 2 + shown_w as u16, area.y));
        let hint = "enter keep · esc clear";
        let hint_w = text_width(hint);
        if 3 + shown_w + 2 + hint_w < width {
            let rect = Rect {
                x: area.right() - hint_w as u16 - 1,
                width: hint_w as u16,
                ..area
            };
            frame.render_widget(Paragraph::new(Line::styled(hint, dim())), rect);
        }
        return;
    }
    if let Some(msg) = app.status() {
        let line = Line::raw(format!(" {}", truncate(msg, width.saturating_sub(2))));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    let hints: &[(&str, &str)] = if app.cards() {
        &CARD_HINTS
    } else {
        &LIST_HINTS
    };
    frame.render_widget(Paragraph::new(hints_line(hints, width, app.popup)), area);
}

/// Key hints that fit; `? help` always last.
fn hints_line(hints: &[(&'static str, &'static str)], width: usize, popup: bool) -> Line<'static> {
    let hint = |key: &'static str, what: &'static str| {
        vec![
            Span::styled(key, bold()),
            Span::styled(format!(" {what}"), dim()),
        ]
    };
    let help = hint("?", "help");
    let sep_w = 3;
    let mut budget = width.saturating_sub(1 + spans_width(&help) + 1);
    let mut spans = vec![Span::raw(" ")];
    let extra = popup.then_some(("esc", "close"));
    for &(key, what) in hints.iter().chain(extra.as_ref()) {
        let h = hint(key, what);
        let w = spans_width(&h) + sep_w;
        if w > budget {
            break;
        }
        budget -= w;
        spans.extend(h);
        spans.push(Span::styled(" · ", dim()));
    }
    spans.extend(help);
    Line::from(spans)
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = HELP
        .iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!(" {} ", pad(key, 9)), bold()),
                Span::raw(*what),
            ])
        })
        .collect();
    lines.push(Line::default());
    lines.push(Line::styled(" any key closes", dim()));
    let content_w = lines
        .iter()
        .map(|l| spans_width(&l.spans))
        .max()
        .unwrap_or(0);
    let w = (content_w as u16 + 3).min(area.width);
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Keys ")),
        rect,
    );
}

// --- text helpers ---

pub(super) fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

pub(super) fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// Control chars become spaces, so they can't shift the layout.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The graphemes of `s` with their widths, split and measured as the
/// buffer does. Graphemes holding control chars are skipped.
fn graphemes(s: &str) -> Vec<(&str, usize)> {
    let span = Span::raw(s);
    let base = s.as_ptr() as usize;
    span.styled_graphemes(Style::default())
        .map(|g| {
            // `g.symbol` borrows the span; slice the same bytes out of `s`.
            let start = g.symbol.as_ptr() as usize - base;
            (
                &s[start..start + g.symbol.len()],
                g.symbol.cell_width() as usize,
            )
        })
        .collect()
}

pub(crate) fn text_width(s: &str) -> usize {
    graphemes(s).iter().map(|(_, w)| w).sum()
}

pub(super) fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| text_width(&s.content)).sum()
}

/// Cuts `s` to `max` columns, ending with `…` when cut.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    let s = clean(s);
    let parts = graphemes(&s);
    if parts.iter().map(|(_, w)| w).sum::<usize>() <= max {
        return parts.into_iter().map(|(g, _)| g).collect();
    }
    let mut out = String::new();
    let mut w = 0;
    for (g, gw) in parts {
        if w + gw + 1 > max {
            break;
        }
        out.push_str(g);
        w += gw;
    }
    if max > 0 {
        out.push('…');
    }
    out
}

/// The end of `s` that fits in `max` columns.
fn tail(s: &str, max: usize) -> String {
    let s = clean(s);
    let mut parts = Vec::new();
    let mut w = 0;
    for (g, gw) in graphemes(&s).into_iter().rev() {
        if w + gw > max {
            break;
        }
        w += gw;
        parts.push(g);
    }
    parts.into_iter().rev().collect()
}

/// `s` cut and padded to exactly `width` columns.
fn pad(s: &str, width: usize) -> String {
    let s = truncate(s, width);
    let w = text_width(&s);
    format!("{s}{}", " ".repeat(width.saturating_sub(w)))
}

/// `s` right-aligned in `width` columns.
pub(super) fn pad_left(s: &str, width: usize) -> String {
    let s = truncate(s, width);
    let w = text_width(&s);
    format!("{}{s}", " ".repeat(width.saturating_sub(w)))
}

/// Word-wraps to `width` columns; words longer than a line are cut.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    if width == 0 {
        return out;
    }
    for para in text.lines() {
        let mut line = String::new();
        let mut w = 0;
        for word in para.split_whitespace() {
            let word_w = text_width(word);
            if w > 0 && w + 1 + word_w <= width {
                line.push(' ');
                line.push_str(word);
                w += 1 + word_w;
                continue;
            }
            if w > 0 {
                out.push(std::mem::take(&mut line));
                w = 0;
            }
            for (g, gw) in graphemes(word) {
                if w > 0 && w + gw > width {
                    out.push(std::mem::take(&mut line));
                    w = 0;
                }
                line.push_str(g);
                w += gw;
            }
        }
        out.push(line);
    }
    out
}

/// `wrap`, keeping at most `max` lines; the last ends in `…` if cut.
fn wrap_limited(text: &str, width: usize, max: usize) -> Vec<String> {
    let mut lines = wrap(text, width);
    if lines.len() > max {
        lines.truncate(max);
        if let Some(last) = lines.last_mut() {
            *last = truncate(&format!("{last}…"), width);
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::ui::fixture::{self, NOW};

    fn app() -> App {
        let mut app = App::new(GroupBy::Tmux, true, false, true);
        app.tick(NOW);
        app.set_snapshot(fixture::snapshot());
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn render(app: &App, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app, NOW)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Buffer rows as text; cells hidden by a wide char are skipped.
    fn rows(buf: &Buffer) -> Vec<String> {
        let width = buf.area.width as usize;
        buf.content
            .chunks(width)
            .map(|cells| {
                let mut row = String::new();
                let mut skip = 0;
                for cell in cells {
                    if skip == 0 {
                        row.push_str(cell.symbol());
                    }
                    skip = skip.max(cell.cell_width() as usize).saturating_sub(1);
                }
                row
            })
            .collect()
    }

    /// The row containing `needle`.
    fn row_with(rows: &[String], needle: &str) -> String {
        rows.iter()
            .find(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("no row with {needle:?} in\n{}", rows.join("\n")))
            .clone()
    }

    /// Display column where `needle` starts in `row`.
    fn col_of(row: &str, needle: &str) -> usize {
        text_width(&row[..row.find(needle).unwrap()])
    }

    /// Position of `needle` on screen, as (x, y) cells.
    fn find(buf: &Buffer, needle: &str) -> (u16, u16) {
        let rows = rows(buf);
        let y = rows.iter().position(|r| r.contains(needle)).unwrap();
        (col_of(&rows[y], needle) as u16, y as u16)
    }

    fn style_at(buf: &Buffer, needle: &str) -> Style {
        buf[find(buf, needle)].style()
    }

    #[test]
    fn full_screen_80x24() {
        let expected = [
            " claude-tower   7 running · 2 needs you · …         ⚠ Claude 2.2.0 not verified ",
            "┌ Instances ───────────────────────────────────────────────────────────────────┐",
            "│ ▾ etl-jobs                                             1 needs you · 1 shell │",
            "│   ? question       Schema question                       main            30s │",
            "│   $ shell          Nightly import fix                    main             2h │",
            "│ ▾ web-front                                                      1 needs you │",
            "│   ! permission     Checkout redesign as storybook compo… feat/checkout…   3m │",
            "│ ▾ payments-dashboard                                               1 working │",
            "│   ⠋ working        日本語のタイトル 🚀 wide chars        feature/very-…   5m │",
            "│ ▾ shop-api                                                1 working · 1 idle │",
            "│   ⠋ working        Issue 882                             main            12s │",
            "│   · idle       new PR 891 review                         pr-891          41m │",
            "│                    └ teammates: reviewer, api-tests                          │",
            "│ ▾ (no tmux)                                                        1 unknown │",
            "│   ? compacting     Detached run                                           7m │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "│                                                                              │",
            "└──────────────────────────────────────────────────────────────────────────────┘",
            " enter jump · / filter · g group · n next needing you · tab fold · ? help       ",
        ];
        assert_eq!(rows(&render(&app(), 80, 24)), expected);
    }

    #[test]
    fn full_screen_160x40_with_panel_and_preview() {
        let mut app = app();
        press(&mut app, KeyCode::Char('n'));
        let list = [
            "┌ Instances ───────────────────────────────────────────────────────────────────────────────────┐",
            "│ ▾ etl-jobs                                                             1 needs you · 1 shell │",
            "│   ? question       Schema question                                       main            30s │",
            "│   $ shell          Nightly import fix                                    main             2h │",
            "│ ▾ web-front                                                                      1 needs you │",
            "│   ! permission     Checkout redesign as storybook components             feat/checkout…   3m │",
            "│ ▾ payments-dashboard                                                               1 working │",
            "│   ⠋ working        日本語のタイトル 🚀 wide chars                        feature/very-…   5m │",
            "│ ▾ shop-api                                                                1 working · 1 idle │",
            "│   ⠋ working        Issue 882                                             main            12s │",
            "│   · idle       new PR 891 review                                         pr-891          41m │",
            "│                    └ teammates: reviewer, api-tests                                          │",
            "│ ▾ (no tmux)                                                                        1 unknown │",
            "│   ? compacting     Detached run                                                           7m │",
        ];
        let panel = [
            "┌ web-front · 1 ───────────────────────────────────────────────┐",
            "│ Checkout redesign as storybook components                    │",
            "│ feat/checkout-v2 · PR #98 · auto                             │",
            "│ ! permission for 3m · web-front:1 · pid 101                  │",
            "│                                                              │",
            "│ Recap: The checkout flow is in PR #98; its two failing       │",
            "│ checks are a CI runner outage, not the code.                 │",
            "│                                                              │",
            "│ Last prompt: rerun the checks and tell me what failed        │",
            "│ then fix it                                                  │",
            "│ ── pane ──────────────────────────────────────────────────── │",
            "│ old line                                                     │",
            "│ ⏺ Bash(gh pr checks 98)                                      │",
            "│   ⎿ 2 failing, 1 pending                                     │",
        ];
        let blank_list = format!("│{}│", " ".repeat(94));
        let blank_panel = format!("│{}│", " ".repeat(62));
        let mut expected = vec![format!(
            " claude-tower   7 running · 2 needs you · 2 working · 1 shell · 1 idle · 1 unknown{}⚠ Claude 2.2.0 not verified ",
            " ".repeat(50)
        )];
        for k in 0..38 {
            let left = list.get(k).map_or(blank_list.clone(), |s| s.to_string());
            let right = panel.get(k).map_or(blank_panel.clone(), |s| s.to_string());
            expected.push(left + &right);
        }
        expected[38] = format!("└{}┘└{}┘", "─".repeat(94), "─".repeat(62));
        expected.push(format!(
            " enter jump · / filter · g group · n next needing you · tab fold · v cards · p panel · a gone · q quit · ? help{}",
            " ".repeat(49)
        ));
        assert_eq!(rows(&render(&app, 160, 40)), expected);
    }

    #[test]
    fn row_colors_and_markers() {
        let mut app = app();
        app.show_gone = true;
        app.set_snapshot(fixture::snapshot());
        let buf = render(&app, 120, 30);
        let permission = style_at(&buf, "! permission");
        assert_eq!(permission.fg, Some(Color::Red));
        assert!(permission.add_modifier.contains(Modifier::BOLD));
        assert_eq!(style_at(&buf, "$ shell").fg, Some(Color::Blue));
        assert_eq!(style_at(&buf, "? compacting").fg, Some(Color::Magenta));
        assert_eq!(style_at(&buf, "⠋ working").fg, Some(Color::Cyan));
        let new = style_at(&buf, "new");
        assert_eq!(new.fg, Some(Color::Green));
        assert!(new.add_modifier.contains(Modifier::BOLD));
        // The selected row (the question) is reversed, edge to edge.
        let (x, y) = find(&buf, "? question");
        assert_eq!(buf[(x, y)].fg, Color::Yellow);
        assert!(buf[(x, y)].modifier.contains(Modifier::REVERSED));
        // The list is 72 wide here; its last inner column is 70.
        assert!(buf[(70, y)].modifier.contains(Modifier::REVERSED));
        assert!(!buf[(71, y)].modifier.contains(Modifier::REVERSED));
        assert!(!buf[(x, y + 1)].modifier.contains(Modifier::REVERSED));
        // Gone: dimmed, with a `gone` badge.
        let (x, y) = find(&buf, "Old experiment");
        assert!(buf[(x, y)].modifier.contains(Modifier::DIM));
        assert!(rows(&buf)[y as usize].contains("gone "));
    }

    #[test]
    fn gone_rows_hidden_by_default() {
        let mut app = app();
        assert!(
            !rows(&render(&app, 120, 30))
                .join("\n")
                .contains("Old experiment")
        );
        press(&mut app, KeyCode::Char('a'));
        let rows = rows(&render(&app, 120, 30));
        assert!(row_with(&rows, "▾ shop-api").contains("1 working · 1 idle · 1 gone │"));
        assert_eq!(
            row_with(&rows, " showing gone"),
            format!(" showing gone instances{}", " ".repeat(97))
        );
    }

    #[test]
    fn spinner_follows_the_clock() {
        let app = app();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, NOW + SPINNER_MS)).unwrap();
        let rows = rows(terminal.backend().buffer());
        assert!(row_with(&rows, "Issue 882").contains("⠙ working"));
        // The age moved too.
        assert!(row_with(&rows, "Issue 882").ends_with(" 12s │"));
        terminal
            .draw(|f| draw(f, &app, NOW + 9 * SPINNER_MS))
            .unwrap();
        let rows = super::tests::rows(terminal.backend().buffer());
        assert!(row_with(&rows, "Issue 882").contains("⠏ working"));
    }

    #[test]
    fn location_only_when_wide() {
        let mut app = app();
        let narrow = rows(&render(&app, 99, 24));
        assert!(!row_with(&narrow, "Issue 882").contains("shop-api:1"));
        app.show_panel = false;
        let wide = rows(&render(&app, 120, 24));
        assert!(row_with(&wide, "Issue 882").contains("shop-api:1"));
        // No pane: a dash.
        let detached = row_with(&wide, "Detached run");
        assert_eq!(
            col_of(&detached, "—"),
            col_of(&row_with(&wide, "Issue 882"), "shop-api:1")
        );
    }

    #[test]
    fn wide_title_keeps_columns_aligned() {
        let app = app();
        for width in [60, 61, 80, 97] {
            let rows = rows(&render(&app, width, 24));
            let wide = row_with(&rows, "日本");
            let issue = row_with(&rows, "Issue 882");
            assert_eq!(text_width(&wide), width as usize, "width {width}");
            assert_eq!(
                col_of(&wide, "feature"),
                col_of(&issue, "main"),
                "width {width}"
            );
            assert!(wide.ends_with("5m │"), "width {width}: {wide}");
        }
        // At 60 the title is cut, with an ellipsis.
        let rows = rows(&render(&app, 60, 24));
        assert!(row_with(&rows, "日本").contains('…'));
    }

    #[test]
    fn truncation_by_display_width() {
        assert_eq!(truncate("日本語", 6), "日本語");
        assert_eq!(truncate("日本語", 5), "日本…");
        assert_eq!(truncate("日本語", 4), "日…");
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("a\tb\nc", 10), "a b c");
        assert_eq!(pad("日本語", 4), "日… ");
        assert_eq!(pad_left("3m", 4), "  3m");
        assert_eq!(tail("abc日本", 5), "c日本");
        assert_eq!(wrap("aaa bbb cccc", 7), ["aaa bbb", "cccc"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_limited("one two three four", 9, 1), ["one two…"]);
    }

    #[test]
    fn panel_hidden_when_narrow_or_toggled() {
        let mut app = app();
        assert!(!rows(&render(&app, 109, 24))[1].contains("etl-jobs · 2"));
        assert!(rows(&render(&app, 110, 24))[1].contains("etl-jobs · 2"));
        press(&mut app, KeyCode::Char('p'));
        assert!(!rows(&render(&app, 160, 24))[1].contains("etl-jobs · 2"));
    }

    #[test]
    fn panel_preview_only_for_its_pane() {
        let mut app = app();
        let rows_q = rows(&render(&app, 160, 40));
        // The question has a pane but the preview is for another one.
        assert!(rows_q.iter().any(|r| r.contains("── pane")));
        assert!(!rows_q.join("\n").contains("Bash(gh pr checks"));
        press(&mut app, KeyCode::Char('n'));
        let buf = render(&app, 160, 40);
        assert_eq!(style_at(&buf, "2 failing").fg, Some(Color::Red));
        assert!(
            style_at(&buf, "⏺ Bash")
                .add_modifier
                .contains(Modifier::BOLD)
        );
        // Blank trailing lines of the capture are dropped.
        let (_, y) = find(&buf, "2 failing");
        assert!(rows(&buf)[y as usize + 1].ends_with(&format!("││{}│", " ".repeat(62))));
    }

    #[test]
    fn panel_for_a_group_header() {
        let mut app = app();
        press(&mut app, KeyCode::Char('k'));
        let rows = rows(&render(&app, 160, 40));
        assert!(rows[1].contains("┌ etl-jobs · 2 "));
        assert!(
            rows[2].ends_with("│ etl-jobs                                                     │")
        );
        assert!(rows[3].contains("│ 1 needs you · 1 shell "));
    }

    #[test]
    fn panel_last_said_teammates_and_error() {
        let mut app = app();
        let pick = |app: &mut App, title: &str| {
            let key = fixture::key_of(app, title);
            while app.selected_instance().map(|i| i.key()) != Some(key.clone()) {
                press(app, KeyCode::Down);
            }
        };
        pick(&mut app, fixture::ISSUE);
        let text = rows(&render(&app, 160, 40)).join("\n");
        assert!(text.contains("Last said: Running the test suite now."));
        assert!(!text.contains("Recap:"));
        pick(&mut app, fixture::REVIEW);
        let text = rows(&render(&app, 160, 40)).join("\n");
        assert!(text.contains("Teammates: reviewer, api-tests"));
        pick(&mut app, fixture::DETACHED);
        let buf = render(&app, 160, 40);
        let text = rows(&buf).join("\n");
        assert!(text.contains("? compacting for 7m · no tmux · pid 107"));
        assert!(text.contains("Error: transcript not found"));
        assert_eq!(style_at(&buf, "transcript not found").fg, Some(Color::Red));
        // No pane: no divider.
        assert!(!text.contains("── pane"));
    }

    #[test]
    fn header_warnings() {
        let mut app = app();
        let mut snap = fixture::snapshot();
        snap.tmux_error = Some("no server running".into());
        snap.registry_error = Some("permission denied".into());
        snap.transcripts_pending = 3;
        app.set_snapshot(snap);
        let header = rows(&render(&app, 240, 10))[0].clone();
        assert!(header.starts_with(" claude-tower   7 running · 2 needs you"));
        assert!(header.ends_with(
            "⚠ tmux: no server running  ⚠ registry: permission denied  ⚠ Claude 2.2.0 not verified  reading transcripts… "
        ));
        // Narrow: the most important warning stays, cut.
        let header = rows(&render(&app, 60, 10))[0].clone();
        assert!(header.contains("⚠ tmux: no server running"), "{header}");
    }

    #[test]
    fn header_omits_zero_counts() {
        let mut app = app();
        let mut snap = fixture::snapshot();
        snap.instances.retain(|i| i.state == State::Working);
        snap.unverified_versions.clear();
        app.set_snapshot(snap);
        let header = rows(&render(&app, 80, 10))[0].clone();
        assert_eq!(header.trim_end(), " claude-tower   2 running · 2 working");
        app.set_snapshot(Snapshot::default());
        let rows = rows(&render(&app, 80, 10));
        assert_eq!(rows[0].trim_end(), " claude-tower   0 running");
        assert!(rows[2].contains("no Claude Code instances running"));
    }

    #[test]
    fn filter_input_and_title() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        for c in "iss".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app, NOW)).unwrap();
        let rows = rows(terminal.backend().buffer());
        assert!(rows[23].starts_with(" /iss "));
        assert!(rows[23].ends_with("enter keep · esc clear "));
        terminal.backend_mut().assert_cursor_position((5, 23));
        assert!(row_with(&rows, "Issue 882").contains("working"));
        assert!(!rows.join("\n").contains("Schema question"));
        // Done editing: the filter shows in the list's title.
        press(&mut app, KeyCode::Enter);
        let rows = super::tests::rows(&render(&app, 80, 24));
        assert!(rows[1].ends_with(" filter: iss ┐"));
        assert!(rows[23].starts_with(" enter jump"));
        // Nothing matches.
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('z'));
        let rows = super::tests::rows(&render(&app, 80, 24));
        assert!(rows[2].contains("nothing matches the filter"));
    }

    #[test]
    fn status_replaces_hints() {
        let mut app = app();
        press(&mut app, KeyCode::Char('g'));
        let rows = rows(&render(&app, 80, 24));
        assert!(rows[23].starts_with(" group by repo"));
    }

    #[test]
    fn popup_hints_mention_esc() {
        let mut app = App::new(GroupBy::Tmux, true, true, true);
        app.set_snapshot(fixture::snapshot());
        let rows = rows(&render(&app, 160, 10));
        assert!(rows[9].contains("q quit · esc close · ? help"));
    }

    #[test]
    fn no_headers_without_groups() {
        let mut app = app();
        app.group_by = GroupBy::None;
        app.set_snapshot(fixture::snapshot());
        let rows = rows(&render(&app, 80, 24));
        assert!(!rows.join("\n").contains('▾'));
        assert!(rows[2].starts_with("│ ! permission     Checkout"));
        assert!(rows[3].starts_with("│ ? question       Schema"));
    }

    #[test]
    fn folded_header_shows_counts() {
        let mut app = app();
        press(&mut app, KeyCode::Tab);
        let rows = rows(&render(&app, 80, 24));
        assert!(rows[2].starts_with("│ ▸ etl-jobs "));
        assert!(rows[2].ends_with(" 1 needs you · 1 shell │"));
        assert!(rows[3].starts_with("│ ▾ web-front "));
    }

    #[test]
    fn scrolls_to_keep_the_selection_visible() {
        let mut app = app();
        press(&mut app, KeyCode::End);
        let rows = rows(&render(&app, 80, 8));
        // 4 lines of list: the last row is on the last of them.
        assert!(rows[5].contains("Detached run"), "{}", rows.join("\n"));
        assert!(rows[2].contains("PR 891 review"));
        press(&mut app, KeyCode::Home);
        let rows = super::tests::rows(&render(&app, 80, 8));
        assert!(rows[2].contains("▾ etl-jobs"));
    }

    #[test]
    fn help_overlay() {
        let mut app = app();
        press(&mut app, KeyCode::Char('?'));
        let text = rows(&render(&app, 80, 24)).join("\n");
        assert!(text.contains("┌ Keys "));
        assert!(text.contains("next instance that needs you"));
        assert!(text.contains("any key closes"));
    }

    #[test]
    fn tiny_terminals_dont_panic() {
        let mut app = app();
        press(&mut app, KeyCode::Char('?'));
        for (w, h) in [(1, 1), (5, 3), (20, 5), (40, 10), (110, 4)] {
            render(&app, w, h);
        }
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('/'));
        for (w, h) in [(1, 1), (5, 3), (20, 5), (40, 10), (110, 4)] {
            render(&app, w, h);
        }
    }
}
