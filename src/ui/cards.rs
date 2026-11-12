//! Card view: one card per instance in a grid, most urgent first. A card
//! shows only the essentials: its state and age, the title and the branch.
//!
//! Grouped by project (tmux session) or repo, each group has its own row:
//! a heading line across the width, then its cards, wrapping onto more rows
//! when there are more cards than fit.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph};

use crate::model::{Group, GroupBy, Instance, State, fmt_age};

use super::app::{App, Item, is_gone};
use super::render::{
    badge, bold, count_parts, dim, draw_empty, join_fitting, spans_width, state_style, text_width,
    truncate,
};

/// Narrowest card, borders included.
pub const CARD_MIN_W: u16 = 26;
/// Border, three lines, border.
pub const CARD_H: u16 = 5;

/// Where cards and group names go, in grid rows and columns.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Grid {
    /// (row, column) per item.
    pub slots: Vec<(usize, usize)>,
    /// A group's heading: group index and the row it starts.
    pub names: Vec<(usize, usize)>,
    pub rows: usize,
}

/// Lays the items out `per_row` cards wide. Ungrouped, cards just flow.
/// Grouped, every group starts a new row and wraps onto more if needed.
pub fn layout(items: &[Item], per_row: usize, grouped: bool) -> Grid {
    let per_row = per_row.max(1);
    let mut grid = Grid::default();
    let (mut row, mut col) = (0, 0);
    let mut k = 0;
    while k < items.len() {
        let Item::Row(g, _) = items[k] else {
            grid.slots.push((row, col));
            k += 1;
            continue;
        };
        let run = items[k..]
            .iter()
            .take_while(|it| matches!(it, Item::Row(h, _) if *h == g))
            .count();
        if grouped {
            if col > 0 {
                row += 1;
                col = 0;
            }
            grid.names.push((g, row));
        }
        for _ in 0..run {
            if col == per_row {
                row += 1;
                col = 0;
            }
            grid.slots.push((row, col));
            col += 1;
        }
        k += run;
    }
    grid.rows = if items.is_empty() { 0 } else { row + 1 };
    grid
}

pub fn draw(frame: &mut Frame, area: Rect, app: &App, now_ms: i64) {
    let mut area = area;
    if !app.filter.is_empty() && !app.editing && area.height > 1 {
        let line = Line::from(vec![
            Span::styled(" filter: ", dim()),
            Span::styled(truncate(&app.filter, area.width as usize / 2), bold()),
            Span::styled("  esc clears", dim()),
        ]);
        frame.render_widget(Paragraph::new(line), Rect { height: 1, ..area });
        area.y += 1;
        area.height -= 1;
    }
    let items = app.items();
    if items.is_empty() {
        app.card_slots.borrow_mut().clear();
        draw_empty(frame, area, app);
        return;
    }

    let per_row = (area.width / CARD_MIN_W).max(1) as usize;
    let grouped = app.group_by != GroupBy::None;
    let grid = layout(items, per_row, grouped);
    // Grouped rows have a line for group names above the cards.
    let name_h = u16::from(grouped);
    let row_h = CARD_H + name_h;
    let shown_rows = ((area.height / row_h) as usize).max(1);

    // Scroll by whole rows, just enough to keep the selection visible.
    let mut top = app.scroll.get().min(grid.rows.saturating_sub(shown_rows));
    if let Some(&(row, _)) = app.cursor().and_then(|c| grid.slots.get(c)) {
        if row < top {
            top = row;
        } else if row >= top + shown_rows {
            top = row + 1 - shown_rows;
        }
    }
    app.scroll.set(top);

    // Spread the leftover columns over the first cards of each row.
    let base = area.width / per_row as u16;
    let extra = area.width % per_row as u16;
    let x_of = |col: usize| {
        let col = col as u16;
        area.x + base * col + col.min(extra)
    };
    let width_of = |col: usize| x_of(col + 1) - x_of(col);
    let y_of = |row: usize| area.y + row_h * (row - top) as u16;
    let visible = |row: usize| row >= top && row < top + shown_rows;

    for &(g, row) in &grid.names {
        if !visible(row) {
            continue;
        }
        let rect = Rect {
            y: y_of(row),
            height: 1,
            ..area
        };
        let line = group_name(app, &app.groups()[g], rect.width as usize);
        frame.render_widget(Paragraph::new(line), rect);
    }

    let labels = labels(app);
    for (k, item) in items.iter().enumerate() {
        let (row, col) = grid.slots[k];
        let Item::Row(_, i) = *item else { continue };
        if !visible(row) {
            continue;
        }
        let rect = Rect {
            x: x_of(col),
            y: y_of(row) + name_h,
            width: width_of(col),
            height: CARD_H,
        };
        if rect.bottom() > area.bottom() {
            continue;
        }
        let inst = &app.snapshot.instances[i];
        let selected = app.cursor() == Some(k);
        draw_card(frame, rect, inst, &labels[k], selected, now_ms);
    }

    // Say how many cards are below, on the spare line if there is one.
    let below = grid
        .slots
        .iter()
        .filter(|s| s.0 >= top + shown_rows)
        .count();
    let spare_y = area.y + row_h * shown_rows as u16;
    if below > 0 && spare_y < area.bottom() {
        let line = Line::styled(format!(" ↓ {below} more"), dim());
        let rect = Rect {
            y: spare_y,
            height: 1,
            ..area
        };
        frame.render_widget(Paragraph::new(line), rect);
    }
    *app.card_slots.borrow_mut() = grid.slots;
}

/// ` name ──── 1 working · 1 idle ─` across the row; counts only for
/// groups of several, since a lone card shows its own state.
fn group_name(app: &App, group: &Group, width: usize) -> Line<'static> {
    let counts = if group.rows.len() > 1 {
        join_fitting(count_parts(&group.counts, true), " · ", width / 2)
    } else {
        Vec::new()
    };
    let counts_w = spans_width(&counts);
    let counts_room = if counts_w > 0 { counts_w + 3 } else { 0 };
    let name = truncate(&group.name, width.saturating_sub(2 + counts_room + 2));
    let no_pane = group
        .rows
        .iter()
        .all(|&i| app.snapshot.instances[i].pane.is_none());
    let name_style = if no_pane && app.group_by == GroupBy::Tmux {
        dim()
    } else {
        bold()
    };
    let rule = width.saturating_sub(1 + text_width(&name) + 1 + counts_room + 1);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(name, name_style),
        Span::raw(" "),
        Span::styled("─".repeat(rule), dim()),
    ];
    if counts_w > 0 {
        spans.push(Span::raw(" "));
        spans.extend(counts);
        spans.push(Span::styled(" ─", dim()));
    }
    Line::from(spans)
}

fn draw_card(
    frame: &mut Frame,
    rect: Rect,
    inst: &Instance,
    label: &str,
    selected: bool,
    now: i64,
) {
    let gone = is_gone(inst);
    let mut border = border_style(inst);
    let mut label_style = if inst.pane.is_none() || gone {
        dim()
    } else {
        bold()
    };
    if selected {
        border = border.add_modifier(Modifier::BOLD);
        label_style = label_style.add_modifier(Modifier::REVERSED);
    }
    let label = truncate(label, (rect.width as usize).saturating_sub(4));
    let mut block = Block::bordered()
        .border_type(if selected {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(border)
        .padding(Padding::horizontal(1));
    if !label.is_empty() {
        block = block.title(Line::from(vec![
            Span::raw(" "),
            Span::styled(label, label_style),
            Span::raw(" "),
        ]));
    }
    let width = block.inner(rect).width as usize;

    // State and age on one line, `new` after the state.
    let badge = badge(inst, now);
    let age = inst
        .state_since_ms
        .map(|t| fmt_age(now - t))
        .unwrap_or_default();
    let new = inst.is_new && !gone;
    let left_w = text_width(&badge) + if new { 4 } else { 0 };
    let room = width.saturating_sub(text_width(&age) + 1);
    let mut first = vec![Span::styled(truncate(&badge, room), state_style(inst))];
    if new && left_w <= room {
        first.push(Span::styled(" new", bold().fg(Color::Green)));
    }
    let gap = width.saturating_sub(left_w.min(room) + text_width(&age));
    first.push(Span::raw(" ".repeat(gap)));
    first.push(Span::styled(age, dim()));

    let mut meta = Vec::new();
    meta.extend(inst.branch.clone());
    match inst.teammates.len() {
        0 => {}
        1 => meta.push("1 teammate".into()),
        n => meta.push(format!("{n} teammates")),
    }

    let lines = vec![
        Line::from(first),
        Line::styled(truncate(&inst.title(), width), bold()),
        Line::styled(truncate(&meta.join(" · "), width), dim()),
    ];
    let mut card = Paragraph::new(lines).block(block);
    if gone {
        card = card.style(dim());
    }
    frame.render_widget(card, rect);
}

/// Border color by state, so the grid reads at a glance. Idle and gone
/// cards stay quiet.
fn border_style(inst: &Instance) -> Style {
    if is_gone(inst) {
        return dim();
    }
    let color = match &inst.state {
        State::NeedsYou(w) if w == "permission prompt" => Color::Red,
        State::NeedsYou(_) => Color::Yellow,
        State::Working => Color::Cyan,
        State::Shell => Color::Blue,
        State::Unknown(_) => Color::Magenta,
        State::Idle | State::Gone => Color::DarkGray,
    };
    Style::new().fg(color)
}

/// Card labels. Ungrouped: the tmux session. Grouped by session: nothing,
/// or the window when the session has several cards. Grouped by repo: the
/// session. Labels that would repeat get the window added.
fn labels(app: &App) -> Vec<String> {
    let instance = |item: &Item| match *item {
        Item::Row(_, i) => Some(&app.snapshot.instances[i]),
        _ => None,
    };
    let base: Vec<String> = app
        .items()
        .iter()
        .map(|item| match (instance(item), app.group_by) {
            (None, _) => String::new(),
            (Some(_), GroupBy::Tmux) => String::new(),
            (Some(i), _) => i.group_name(GroupBy::Tmux),
        })
        .collect();
    let groups: Vec<usize> = app
        .items()
        .iter()
        .map(|item| match *item {
            Item::Row(g, _) => g,
            _ => usize::MAX,
        })
        .collect();
    app.items()
        .iter()
        .enumerate()
        .map(|(k, item)| {
            let label = &base[k];
            let shared = (0..base.len())
                .filter(|&j| j != k && base[j] == *label && groups[j] == groups[k])
                .count()
                > 0;
            match instance(item).and_then(|i| i.pane.as_ref()) {
                Some(pane) if shared && label.is_empty() => {
                    format!("window {}", pane.window_index)
                }
                Some(pane) if shared => format!("{label}:{}", pane.window_index),
                _ => label.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::config::Layout;
    use crate::ui::fixture::{self, NOW};
    use crate::ui::render;

    fn app(by: GroupBy) -> App {
        let mut app = App::new(by, false, false, true).with_layout(Layout::Cards);
        app.tick(NOW);
        app.set_snapshot(fixture::snapshot());
        app
    }

    fn render(app: &App, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render::draw(f, app, NOW)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn screen(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn selected(app: &App) -> String {
        app.selected_instance()
            .map(|i| i.title())
            .unwrap_or_default()
    }

    fn row(g: usize) -> Item {
        Item::Row(g, 0)
    }

    #[test]
    fn layout_gives_each_group_a_row() {
        // Groups of 1, 2, 1, 3 cards, 3 per row.
        let items = [row(0), row(1), row(1), row(2), row(3), row(3), row(3)];
        let grid = layout(&items, 3, true);
        assert_eq!(
            grid.slots,
            [(0, 0), (1, 0), (1, 1), (2, 0), (3, 0), (3, 1), (3, 2)]
        );
        assert_eq!(grid.names, [(0, 0), (1, 1), (2, 2), (3, 3)]);
        assert_eq!(grid.rows, 4);
    }

    #[test]
    fn layout_wraps_a_big_group_and_flows_ungrouped() {
        let items = [row(0), row(1), row(1), row(1), row(1), row(1)];
        let grid = layout(&items, 2, true);
        assert_eq!(grid.slots, [(0, 0), (1, 0), (1, 1), (2, 0), (2, 1), (3, 0)]);
        assert_eq!(grid.names, [(0, 0), (1, 1)]);
        let flat = layout(&items, 4, false);
        assert_eq!(flat.slots[4], (1, 0));
        assert!(flat.names.is_empty());
        assert_eq!(layout(&[], 3, true).rows, 0);
    }

    #[test]
    fn grouped_by_project() {
        let app = app(GroupBy::Tmux);
        let rows = screen(&render(&app, 80, 26));
        // Group names sit above their cards; the most urgent group first.
        assert!(rows[1].starts_with(" etl-jobs ─"), "{}", rows[1]);
        assert!(rows[1].contains("1 needs you · 1 shell"), "{}", rows[1]);
        assert!(
            rows[1].trim_end().ends_with("1 needs you · 1 shell ─"),
            "{}",
            rows[1]
        );
        // Each group has its own heading; a lone card's shows no counts.
        let web = rows.iter().find(|r| r.starts_with(" web-front ─")).unwrap();
        assert!(
            web.trim_end().ends_with("─") && !web.contains("needs you"),
            "{web}"
        );
        // Several cards in one session are told apart by window.
        assert!(rows.iter().any(|r| r.contains("window 1")), "{rows:#?}");
        // The selected card has a thick border.
        assert!(rows[2].starts_with("┏"), "{}", rows[2]);
        assert!(rows.iter().any(|r| r.contains("2 teammates")));
        assert!(!rows.iter().any(|r| r.contains(fixture::GONE)));
        assert!(rows[25].contains("v list"));
    }

    #[test]
    fn ungrouped_flows_with_session_labels() {
        let app = app(GroupBy::None);
        let rows = screen(&render(&app, 80, 24));
        assert!(rows[1].starts_with("┏ "), "{}", rows[1]);
        assert!(rows.iter().any(|r| r.contains("(no tmux)")));
        assert!(rows.iter().any(|r| r.contains("shop-api:")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains(" etl-jobs ─")));
    }

    #[test]
    fn up_and_down_go_to_the_nearest_card() {
        let mut app = app(GroupBy::None);
        render(&app, 80, 24);
        let order: Vec<String> = app
            .items()
            .iter()
            .map(|it| match *it {
                Item::Row(_, i) => app.snapshot.instances[i].title(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(selected(&app), order[0]);
        press(&mut app, KeyCode::Right);
        assert_eq!(selected(&app), order[1]);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(selected(&app), order[4]);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(selected(&app), order[3]);
        press(&mut app, KeyCode::Up);
        assert_eq!(selected(&app), order[0]);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(selected(&app), order[0]);
        // From the end of a full row to a shorter row: the nearest card.
        let last = order.len() - 1;
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(selected(&app), order[last]);
    }

    #[test]
    fn grouped_moves_follow_the_grid() {
        let mut app = app(GroupBy::Tmux);
        render(&app, 80, 26);
        let slots = app.card_slots.borrow().clone();
        let start = slots[0];
        press(&mut app, KeyCode::Down);
        let k = app.cursor().unwrap();
        assert_eq!(slots[k].0, start.0 + 1);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.cursor(), Some(0));
    }

    #[test]
    fn keys_on_cards() {
        let mut app = app(GroupBy::Tmux);
        let first = selected(&app);
        // Tab does nothing on cards.
        press(&mut app, KeyCode::Tab);
        assert_eq!(selected(&app), first);
        // g cycles grouping as in the list; the selection stays.
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.group_by, GroupBy::Repo);
        assert_eq!(selected(&app), first);
        assert!(app.items().iter().all(|it| matches!(it, Item::Row(..))));
        // v switches to the list, which has headers, and back.
        press(&mut app, KeyCode::Char('v'));
        assert!(!app.cards());
        assert!(app.items().iter().any(|it| matches!(it, Item::Header(_))));
        assert_eq!(selected(&app), first);
        press(&mut app, KeyCode::Char('v'));
        assert!(app.cards());
    }

    #[test]
    fn filter_line_and_empty() {
        let mut app = app(GroupBy::Tmux);
        press(&mut app, KeyCode::Char('/'));
        for c in "issue".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        let rows = screen(&render(&app, 80, 24));
        assert!(rows[1].contains("filter: issue"), "{}", rows[1]);
        assert!(rows[2].starts_with(" shop-api ─"), "{}", rows[2]);
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Enter);
        let rows = screen(&render(&app, 80, 24));
        assert!(
            rows.iter()
                .any(|r| r.contains("nothing matches the filter"))
        );
    }

    #[test]
    fn scrolls_to_the_selection() {
        let mut app = app(GroupBy::Tmux);
        // Room for one grouped row plus the spare line.
        render(&app, 80, 9);
        press(&mut app, KeyCode::End);
        let rows = screen(&render(&app, 80, 9));
        assert!(rows.iter().any(|r| r.starts_with("┏")), "{rows:#?}");
        assert!(app.scroll.get() > 0);
    }

    #[test]
    fn narrow_terminal_one_column() {
        let app = app(GroupBy::None);
        let rows = screen(&render(&app, 30, 24));
        assert!(app.card_slots.borrow().iter().all(|s| s.1 == 0));
        assert!(rows.iter().any(|r| r.contains("more")));
    }
}
