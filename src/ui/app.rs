//! Dashboard state and key handling. No terminal, no IO: `mod.rs` runs the
//! actions this returns.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;

use crate::collector::Request;
use crate::config::Layout;
use crate::model::{Group, GroupBy, Instance, PaneInfo, Snapshot, State, build_groups};

use super::ansi;

/// How long a status message replaces the key hints.
pub const STATUS_MS: i64 = 4000;

/// What the event loop should do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Jump(PaneInfo),
    Send(Request),
}

/// One line of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// Group header: index into `groups()`.
    Header(usize),
    /// Instance row: group index, instance index.
    Row(usize, usize),
    /// `└ teammates:` line under an instance; not selectable.
    Teammates(usize),
}

impl Item {
    pub fn selectable(self) -> bool {
        !matches!(self, Item::Teammates(_))
    }
}

/// The selection, by key, so it survives new snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Group(String),
    Row(String),
}

#[derive(Debug, Clone)]
struct Status {
    text: String,
    /// `None` stays until replaced.
    until_ms: Option<i64>,
}

/// Dead process or `Gone` state, as `build_groups` sees it.
pub fn is_gone(i: &Instance) -> bool {
    !i.alive || i.state == State::Gone
}

fn needs_you(i: &Instance) -> bool {
    !is_gone(i) && i.state.needs_you()
}

pub struct App {
    pub snapshot: Snapshot,
    /// Cards or the grouped list.
    pub layout: Layout,
    pub group_by: GroupBy,
    pub filter: String,
    /// The filter is being typed.
    pub editing: bool,
    /// Folded group keys.
    pub folded: HashSet<String>,
    pub show_gone: bool,
    pub show_panel: bool,
    pub help: bool,
    pub popup: bool,
    in_tmux: bool,
    status: Option<Status>,
    now_ms: i64,
    groups: Vec<Group>,
    items: Vec<Item>,
    selection: Option<Selection>,
    /// Index into `items` of the selection.
    cursor: usize,
    /// Last preview request sent; outer `None` = never sent.
    sent_preview: Option<Option<String>>,
    /// First visible list line; the renderer keeps it.
    pub scroll: Cell<usize>,
    /// The snapshot's preview, parsed once per snapshot rather than per
    /// frame, trailing blank lines dropped.
    preview: Vec<Line<'static>>,
    /// (grid row, column) of each item on the card grid; the card
    /// renderer keeps it, and ↑ ↓ move by it.
    pub card_slots: RefCell<Vec<(usize, usize)>>,
}

impl App {
    pub fn new(group_by: GroupBy, show_panel: bool, popup: bool, in_tmux: bool) -> App {
        App {
            snapshot: Snapshot::default(),
            layout: Layout::List,
            group_by,
            filter: String::new(),
            editing: false,
            folded: HashSet::new(),
            show_gone: false,
            show_panel,
            help: false,
            popup,
            in_tmux,
            status: None,
            now_ms: 0,
            groups: Vec::new(),
            items: Vec::new(),
            selection: None,
            cursor: 0,
            sent_preview: None,
            scroll: Cell::new(0),
            preview: Vec::new(),
            card_slots: RefCell::new(Vec::new()),
        }
    }

    pub fn with_layout(mut self, layout: Layout) -> App {
        self.layout = layout;
        self.rebuild();
        self
    }

    pub fn cards(&self) -> bool {
        self.layout == Layout::Cards
    }

    // --- inputs from the event loop ---

    /// Sets the clock and expires the status message. True if it expired.
    pub fn tick(&mut self, now_ms: i64) -> bool {
        self.now_ms = now_ms;
        let expired =
            matches!(&self.status, Some(Status { until_ms: Some(t), .. }) if *t <= now_ms);
        if expired {
            self.status = None;
        }
        expired
    }

    pub fn set_snapshot(&mut self, snapshot: Snapshot) {
        let old = self.snapshot.preview.as_ref().map(|p| &p.text);
        if snapshot.preview.as_ref().map(|p| &p.text) != old {
            self.preview = snapshot
                .preview
                .as_ref()
                .map(|p| ansi::to_lines(&p.text))
                .unwrap_or_default();
            while self
                .preview
                .last()
                .is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
            {
                self.preview.pop();
            }
        }
        self.snapshot = snapshot;
        self.rebuild();
    }

    /// The last `room` lines of the preview, if it's for this pane.
    pub fn preview_lines(&self, pane_id: &str, room: usize) -> Vec<Line<'static>> {
        let ours = self
            .snapshot
            .preview
            .as_ref()
            .is_some_and(|p| p.pane_id == pane_id);
        if !ours {
            return Vec::new();
        }
        let skip = self.preview.len().saturating_sub(room);
        self.preview[skip..].to_vec()
    }

    /// A message shown in the footer for `STATUS_MS`.
    pub fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            until_ms: Some(self.now_ms + STATUS_MS),
        });
    }

    /// A message that stays until another replaces it.
    pub fn set_sticky_status(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            until_ms: None,
        });
    }

    /// A preview request when the selected pane changed since the last one.
    pub fn take_preview_request(&mut self) -> Option<Request> {
        let pane = self
            .selected_instance()
            .and_then(|i| i.pane.as_ref())
            .map(|p| p.pane_id.clone());
        if self.sent_preview.as_ref() == Some(&pane) {
            return None;
        }
        self.sent_preview = Some(pane.clone());
        Some(Request::Preview(pane))
    }

    // --- reads for the renderer ---

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Index into `items()` of the selection.
    pub fn cursor(&self) -> Option<usize> {
        self.current_item().map(|_| self.cursor)
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_ref().map(|s| s.text.as_str())
    }

    pub fn is_folded(&self, group: &Group) -> bool {
        self.group_by != GroupBy::None && self.folded.contains(&group.key)
    }

    pub fn selected_instance(&self) -> Option<&Instance> {
        match self.current_item()? {
            Item::Row(_, i) => self.snapshot.instances.get(i),
            _ => None,
        }
    }

    /// The selected header's group, or the selected row's.
    pub fn selected_group(&self) -> Option<&Group> {
        match self.current_item()? {
            Item::Header(g) | Item::Row(g, _) => self.groups.get(g),
            Item::Teammates(_) => None,
        }
    }

    /// A visible row is working, so the spinner needs redraws.
    pub fn animating(&self) -> bool {
        self.items.iter().any(|item| match *item {
            Item::Row(_, i) => {
                let inst = &self.snapshot.instances[i];
                !is_gone(inst) && inst.state == State::Working
            }
            _ => false,
        })
    }

    // --- keys ---

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if self.help {
            self.help = false;
            return Action::None;
        }
        if self.editing {
            self.edit_key(key);
            return Action::None;
        }
        if ctrl || key.modifiers.contains(KeyModifiers::ALT) {
            return Action::None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.down(),
            KeyCode::Char('k') | KeyCode::Up => self.up(),
            KeyCode::Char('l') | KeyCode::Right if self.cards() => self.step(true),
            KeyCode::Char('h') | KeyCode::Left if self.cards() => self.step(false),
            KeyCode::Char('v') => {
                self.layout = match self.layout {
                    Layout::Cards => Layout::List,
                    Layout::List => Layout::Cards,
                };
                self.rebuild();
                let what = if self.cards() { "cards" } else { "list" };
                self.set_status(format!("{what} view"));
            }
            KeyCode::Home => self.select_first(),
            KeyCode::End => self.select_last(),
            KeyCode::Enter => return self.enter(),
            KeyCode::Tab => self.tab(),
            KeyCode::Char('n') => self.next_needing_you(),
            KeyCode::Char('/') => self.editing = true,
            KeyCode::Char('g') => {
                self.group_by = self.group_by.next();
                self.rebuild();
                self.set_status(format!("group by {}", self.group_by.label()));
            }
            KeyCode::Char('p') => self.show_panel = !self.show_panel,
            KeyCode::Char('a') => {
                self.show_gone = !self.show_gone;
                self.rebuild();
                let what = if self.show_gone { "showing" } else { "hiding" };
                self.set_status(format!("{what} gone instances"));
            }
            KeyCode::Char('r') => return Action::Send(Request::RefreshNow),
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.rebuild();
                } else if self.popup {
                    return Action::Quit;
                }
            }
            _ => {}
        }
        Action::None
    }

    fn edit_key(&mut self, key: KeyEvent) {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if plain => {
                self.filter.push(c);
                self.rebuild();
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.rebuild();
            }
            KeyCode::Enter => self.editing = false,
            KeyCode::Esc => {
                self.filter.clear();
                self.editing = false;
                self.rebuild();
            }
            KeyCode::Down => self.down(),
            KeyCode::Up => self.up(),
            _ => {}
        }
    }

    fn enter(&mut self) -> Action {
        match self.current_item() {
            Some(Item::Header(g)) => self.toggle_fold(g),
            Some(Item::Row(_, i)) => {
                let inst = &self.snapshot.instances[i];
                match inst.pane.clone() {
                    None => self.set_status("no tmux pane for this instance"),
                    Some(p) if !self.in_tmux => {
                        self.set_status(format!("run: tmux attach -t {}", p.session_name));
                    }
                    Some(p) => return Action::Jump(p),
                }
            }
            _ => {}
        }
        Action::None
    }

    /// Tab: toggle a header; on a row, fold its group and select the header.
    fn tab(&mut self) {
        if self.cards() {
            return;
        }
        match self.current_item() {
            Some(Item::Header(g)) => self.toggle_fold(g),
            Some(Item::Row(g, _)) if self.group_by != GroupBy::None => {
                let key = self.groups[g].key.clone();
                self.folded.insert(key.clone());
                self.selection = Some(Selection::Group(key));
                self.rebuild();
            }
            _ => {}
        }
    }

    fn toggle_fold(&mut self, g: usize) {
        let key = self.groups[g].key.clone();
        if !self.folded.remove(&key) {
            self.folded.insert(key.clone());
        }
        self.selection = Some(Selection::Group(key));
        self.rebuild();
    }

    /// Next row that needs you, in list order, wrapping. Unfolds its group.
    fn next_needing_you(&mut self) {
        let order: Vec<(usize, usize)> = self
            .groups
            .iter()
            .enumerate()
            .flat_map(|(g, group)| group.rows.iter().map(move |&i| (g, i)))
            .collect();
        let start = match self.current_item() {
            Some(Item::Row(_, i)) => order.iter().position(|&(_, j)| j == i).map_or(0, |p| p + 1),
            Some(Item::Header(g)) => order.iter().position(|&(h, _)| h == g).unwrap_or(0),
            _ => 0,
        };
        let found = (0..order.len())
            .map(|k| order[(start + k) % order.len()])
            .find(|&(_, i)| needs_you(&self.snapshot.instances[i]));
        match found {
            Some((g, i)) => {
                self.folded.remove(&self.groups[g].key);
                self.selection = Some(Selection::Row(self.snapshot.instances[i].key()));
                self.rebuild();
            }
            None => self.set_status("nothing needs you"),
        }
    }

    // --- selection ---

    fn current_item(&self) -> Option<Item> {
        self.selection.as_ref()?;
        self.items.get(self.cursor).copied()
    }

    /// Down a line, or to the nearest card in the next grid row.
    fn down(&mut self) {
        if self.cards() {
            self.card_row(true);
        } else {
            self.step(true);
        }
    }

    /// Up a line, or to the nearest card in the grid row above.
    fn up(&mut self) {
        if self.cards() {
            self.card_row(false);
        } else {
            self.step(false);
        }
    }

    fn card_row(&mut self, down: bool) {
        if self.current_item().is_none() {
            return self.select_first();
        }
        let target = {
            let slots = self.card_slots.borrow();
            let Some(&(row, col)) = slots.get(self.cursor) else {
                // Not drawn yet: move in list order.
                drop(slots);
                return self.step(down);
            };
            let next = if down {
                slots.iter().map(|s| s.0).filter(|&r| r > row).min()
            } else {
                slots.iter().map(|s| s.0).filter(|&r| r < row).max()
            };
            next.and_then(|r| {
                (0..slots.len())
                    .filter(|&k| slots[k].0 == r)
                    .min_by_key(|&k| slots[k].1.abs_diff(col))
            })
        };
        if let Some(k) = target {
            self.select_index(k);
        }
    }

    fn step(&mut self, down: bool) {
        if self.current_item().is_none() {
            self.select_first();
            return;
        }
        let next = if down {
            (self.cursor + 1..self.items.len()).find(|&i| self.items[i].selectable())
        } else {
            (0..self.cursor).rev().find(|&i| self.items[i].selectable())
        };
        if let Some(i) = next {
            self.select_index(i);
        }
    }

    fn select_first(&mut self) {
        if let Some(i) = self.items.iter().position(|it| it.selectable()) {
            self.select_index(i);
        }
    }

    fn select_last(&mut self) {
        if let Some(i) = self.items.iter().rposition(|it| it.selectable()) {
            self.select_index(i);
        }
    }

    fn select_index(&mut self, i: usize) {
        self.cursor = i;
        self.selection = match self.items[i] {
            Item::Header(g) => Some(Selection::Group(self.groups[g].key.clone())),
            Item::Row(_, i) | Item::Teammates(i) => {
                Some(Selection::Row(self.snapshot.instances[i].key()))
            }
        };
    }

    fn find(&self, sel: &Selection) -> Option<usize> {
        self.items.iter().position(|item| match (*item, sel) {
            (Item::Header(g), Selection::Group(key)) => self.groups[g].key == *key,
            (Item::Row(_, i), Selection::Row(key)) => self.snapshot.instances[i].key() == *key,
            _ => false,
        })
    }

    /// Regroups and rebuilds the visible lines, keeping the selection.
    fn rebuild(&mut self) {
        let by = self.group_by;
        self.groups = build_groups(
            &self.snapshot.instances,
            by,
            &self.filter,
            self.show_gone,
            self.now_ms,
        );
        // Cards show group names above the cards instead of header items.
        let headers = by != GroupBy::None && !self.cards();
        self.items.clear();
        for (g, group) in self.groups.iter().enumerate() {
            if headers {
                self.items.push(Item::Header(g));
                if self.folded.contains(&group.key) {
                    continue;
                }
            }
            for &i in &group.rows {
                self.items.push(Item::Row(g, i));
                if !self.cards() && !self.snapshot.instances[i].teammates.is_empty() {
                    self.items.push(Item::Teammates(i));
                }
            }
        }
        self.restore_selection();
    }

    fn restore_selection(&mut self) {
        if let Some(i) = self.selection.as_ref().and_then(|s| self.find(s)) {
            self.cursor = i;
            return;
        }
        // A row hidden in a folded group: select its header.
        if let Some(Selection::Row(key)) = &self.selection {
            let header = self.items.iter().position(|item| match *item {
                Item::Header(g) => self.groups[g]
                    .rows
                    .iter()
                    .any(|&i| self.snapshot.instances[i].key() == *key),
                _ => false,
            });
            if let Some(i) = header {
                self.select_index(i);
                return;
            }
        }
        // First time: the first row. Otherwise whatever is now where the
        // selection was.
        let target = if self.selection.is_none() {
            self.items.iter().position(|it| matches!(it, Item::Row(..)))
        } else {
            None
        };
        match target.or_else(|| self.nearest_selectable(self.cursor)) {
            Some(i) => self.select_index(i),
            // Nothing to show: keep the key, so clearing a filter that
            // matched nothing brings the selection back.
            None => self.cursor = 0,
        }
    }

    fn nearest_selectable(&self, at: usize) -> Option<usize> {
        let at = at.min(self.items.len().checked_sub(1)?);
        (0..=at)
            .rev()
            .chain(at..self.items.len())
            .find(|&i| self.items[i].selectable())
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::ui::fixture::{self, NOW};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn app_with(popup: bool, in_tmux: bool) -> App {
        let mut app = App::new(GroupBy::Tmux, true, popup, in_tmux);
        app.tick(NOW);
        app.set_snapshot(fixture::snapshot());
        app
    }

    fn app() -> App {
        app_with(false, true)
    }

    fn selected_title(app: &App) -> Option<String> {
        app.selected_instance().map(|i| i.title())
    }

    fn selected_group_name(app: &App) -> Option<String> {
        match app.current_item()? {
            Item::Header(g) => Some(app.groups()[g].name.clone()),
            _ => None,
        }
    }

    /// The visible lines as short labels, for asserting the list shape.
    fn lines(app: &App) -> Vec<String> {
        app.items()
            .iter()
            .map(|item| match *item {
                Item::Header(g) => format!("# {}", app.groups()[g].name),
                Item::Row(_, i) => app.snapshot.instances[i].title(),
                Item::Teammates(i) => format!("└ {}", app.snapshot.instances[i].title()),
            })
            .collect()
    }

    #[test]
    fn first_row_selected_and_preview_requested() {
        let mut app = app();
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::QUESTION));
        assert_eq!(
            app.take_preview_request(),
            Some(Request::Preview(Some("%6".into())))
        );
        assert_eq!(app.take_preview_request(), None);
        app.handle_key(ch('j'));
        assert_eq!(
            app.take_preview_request(),
            Some(Request::Preview(Some("%5".into())))
        );
    }

    #[test]
    fn preview_none_on_header_and_paneless_row() {
        let mut app = app();
        app.take_preview_request();
        app.handle_key(ch('k'));
        assert_eq!(selected_group_name(&app).as_deref(), Some("etl-jobs"));
        assert_eq!(app.take_preview_request(), Some(Request::Preview(None)));
        app.handle_key(key(KeyCode::End));
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::DETACHED));
        assert_eq!(app.take_preview_request(), None);
    }

    #[test]
    fn movement_skips_teammate_lines() {
        let mut app = app();
        let teammates = app
            .items()
            .iter()
            .position(|i| matches!(i, Item::Teammates(_)))
            .unwrap();
        // Walk down over every line and back up: never on the teammate line.
        let mut seen = Vec::new();
        for _ in 0..app.items().len() {
            app.handle_key(key(KeyCode::Down));
            seen.push(app.cursor().unwrap());
        }
        for _ in 0..app.items().len() {
            app.handle_key(ch('k'));
            seen.push(app.cursor().unwrap());
        }
        assert!(!seen.contains(&teammates));
        // From the row with teammates, down goes to the next header.
        app.selection = Some(Selection::Row(fixture::key_of(&app, fixture::REVIEW)));
        app.rebuild();
        app.handle_key(ch('j'));
        assert_eq!(app.cursor(), Some(teammates + 1));
        assert_eq!(selected_group_name(&app).as_deref(), Some("(no tmux)"));
        app.handle_key(ch('k'));
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::REVIEW));
    }

    #[test]
    fn movement_stops_at_the_ends() {
        let mut app = app();
        app.handle_key(key(KeyCode::Home));
        assert_eq!(app.cursor(), Some(0));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.cursor(), Some(0));
        app.handle_key(key(KeyCode::End));
        let last = app.items().len() - 1;
        assert_eq!(app.cursor(), Some(last));
        app.handle_key(ch('j'));
        assert_eq!(app.cursor(), Some(last));
    }

    #[test]
    fn fold_and_unfold() {
        let mut app = app();
        let before = lines(&app);
        // Tab on a row folds its group and selects the header.
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(selected_group_name(&app).as_deref(), Some("etl-jobs"));
        assert!(app.folded.contains("etl-jobs"));
        let folded = lines(&app);
        assert_eq!(folded.len(), before.len() - 2);
        assert_eq!(folded[1], "# web-front");
        // Enter on the header unfolds it.
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        assert_eq!(lines(&app), before);
        assert_eq!(selected_group_name(&app).as_deref(), Some("etl-jobs"));
        // Tab on the header folds it again, Enter unfolds.
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(lines(&app), folded);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(lines(&app), before);
    }

    #[test]
    fn folding_shop_api_hides_its_teammate_line() {
        let mut app = app();
        app.folded.insert("shop-api".into());
        app.rebuild();
        assert!(!app.items().iter().any(|i| matches!(i, Item::Teammates(_))));
        assert!(lines(&app).contains(&"# shop-api".to_string()));
    }

    #[test]
    fn filter_editing() {
        let mut app = app();
        app.handle_key(ch('/'));
        assert!(app.editing);
        for c in "issue".chars() {
            app.handle_key(ch(c));
        }
        assert_eq!(app.filter, "issue");
        assert_eq!(lines(&app), ["# shop-api", fixture::ISSUE]);
        // Keys are text while editing.
        app.handle_key(ch('q'));
        assert_eq!(app.filter, "issueq");
        assert!(app.items().is_empty());
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(lines(&app), ["# shop-api", fixture::ISSUE]);
        // Enter keeps the filter.
        app.handle_key(key(KeyCode::Enter));
        assert!(!app.editing);
        assert_eq!(app.filter, "issue");
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::ISSUE));
        // Editing again and Esc clears it.
        app.handle_key(ch('/'));
        app.handle_key(ch('x'));
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.editing);
        assert_eq!(app.filter, "");
        assert_eq!(lines(&app).len(), 13);
        // The selection stayed on the filtered row.
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::ISSUE));
    }

    #[test]
    fn esc_clears_filter_then_quits_only_in_popup() {
        for popup in [false, true] {
            let mut app = app_with(popup, true);
            app.handle_key(ch('/'));
            app.handle_key(ch('a'));
            app.handle_key(key(KeyCode::Enter));
            assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
            assert_eq!(app.filter, "");
            let second = app.handle_key(key(KeyCode::Esc));
            let expected = if popup { Action::Quit } else { Action::None };
            assert_eq!(second, expected, "popup = {popup}");
        }
    }

    #[test]
    fn quit_keys() {
        let mut app = app();
        assert_eq!(app.handle_key(ch('q')), Action::Quit);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.handle_key(ch('/'));
        assert_eq!(app.handle_key(ctrl_c), Action::Quit);
    }

    #[test]
    fn next_needing_you_wraps_and_unfolds() {
        let mut app = app();
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::QUESTION));
        app.folded.insert("web-front".into());
        app.rebuild();
        app.handle_key(ch('n'));
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::CHECKOUT));
        assert!(!app.folded.contains("web-front"));
        // Wraps back to the first one.
        app.handle_key(ch('n'));
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::QUESTION));
        // From a header, starts with that group's own rows.
        app.handle_key(key(KeyCode::Home));
        assert_eq!(selected_group_name(&app).as_deref(), Some("etl-jobs"));
        app.handle_key(ch('n'));
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::QUESTION));
        // From the last row, wraps to the top.
        app.handle_key(key(KeyCode::End));
        app.handle_key(ch('n'));
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::QUESTION));
    }

    #[test]
    fn next_needing_you_with_none() {
        let mut app = app();
        let mut snap = fixture::snapshot();
        for i in &mut snap.instances {
            if i.state.needs_you() {
                i.state = State::Idle;
            }
        }
        app.set_snapshot(snap);
        let before = app.selection.clone();
        app.handle_key(ch('n'));
        assert_eq!(app.selection.clone(), before);
        assert_eq!(app.status(), Some("nothing needs you"));
    }

    #[test]
    fn selection_kept_across_snapshots() {
        let mut app = app();
        app.selection = Some(Selection::Row(fixture::key_of(&app, fixture::REVIEW)));
        app.rebuild();
        // The instance moves: B finishes, so the order inside shop-api changes.
        let mut snap = fixture::snapshot();
        let issue = snap
            .instances
            .iter_mut()
            .find(|i| i.title() == fixture::ISSUE)
            .unwrap();
        issue.state = State::Idle;
        issue.state_since_ms = Some(NOW);
        app.set_snapshot(snap);
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::REVIEW));
        // A selected header survives too.
        app.handle_key(key(KeyCode::Home));
        app.set_snapshot(fixture::snapshot());
        assert_eq!(selected_group_name(&app).as_deref(), Some("etl-jobs"));
    }

    #[test]
    fn selection_moves_to_a_neighbor_when_its_row_disappears() {
        let mut app = app();
        app.selection = Some(Selection::Row(fixture::key_of(&app, fixture::ISSUE)));
        app.rebuild();
        let at = app.cursor().unwrap();
        let mut snap = fixture::snapshot();
        snap.instances.retain(|i| i.title() != fixture::ISSUE);
        app.set_snapshot(snap);
        // Same place in the list: the next row of the group moved up.
        assert_eq!(app.cursor(), Some(at));
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::REVIEW));
        // Everything gone: no selection, no panic.
        app.set_snapshot(Snapshot::default());
        assert_eq!(app.cursor(), None);
        app.handle_key(ch('j'));
        app.handle_key(ch('n'));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
    }

    #[test]
    fn selected_row_in_a_folded_group_selects_its_header() {
        let mut app = app();
        app.selection = Some(Selection::Row(fixture::key_of(&app, fixture::REVIEW)));
        app.folded.insert("shop-api".into());
        app.rebuild();
        assert_eq!(selected_group_name(&app).as_deref(), Some("shop-api"));
    }

    #[test]
    fn jump() {
        let mut app = app();
        // The paneless row.
        app.handle_key(key(KeyCode::End));
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        assert_eq!(app.status(), Some("no tmux pane for this instance"));
        // The status goes away after a few seconds.
        assert!(!app.tick(NOW + STATUS_MS - 1));
        assert!(app.tick(NOW + STATUS_MS));
        assert_eq!(app.status(), None);
        // A row with a pane.
        app.handle_key(key(KeyCode::Home));
        app.handle_key(ch('j'));
        match app.handle_key(key(KeyCode::Enter)) {
            Action::Jump(p) => assert_eq!(p.pane_id, "%6"),
            other => panic!("expected a jump, got {other:?}"),
        }
    }

    #[test]
    fn jump_outside_tmux_shows_attach_command() {
        let mut app = app_with(false, false);
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        assert_eq!(app.status(), Some("run: tmux attach -t etl-jobs"));
    }

    #[test]
    fn group_modes_and_gone_rows() {
        let mut app = app();
        assert_eq!(app.handle_key(ch('g')), Action::None);
        assert_eq!(app.group_by, GroupBy::Repo);
        app.handle_key(ch('g'));
        assert_eq!(app.group_by, GroupBy::None);
        // No headers in one list; the selection is still an instance.
        assert!(!app.items().iter().any(|i| matches!(i, Item::Header(_))));
        assert_eq!(selected_title(&app).as_deref(), Some(fixture::QUESTION));
        // Tab does nothing without groups.
        app.handle_key(key(KeyCode::Tab));
        assert!(app.folded.is_empty());
        app.handle_key(ch('g'));
        assert_eq!(app.group_by, GroupBy::Tmux);

        assert!(!lines(&app).contains(&fixture::GONE.to_string()));
        app.handle_key(ch('a'));
        assert!(lines(&app).contains(&fixture::GONE.to_string()));
        app.handle_key(ch('a'));
        assert!(!lines(&app).contains(&fixture::GONE.to_string()));
    }

    #[test]
    fn other_keys() {
        let mut app = app();
        assert_eq!(app.handle_key(ch('r')), Action::Send(Request::RefreshNow));
        app.handle_key(ch('p'));
        assert!(!app.show_panel);
        app.handle_key(ch('?'));
        assert!(app.help);
        // Any key closes help and does nothing else.
        assert_eq!(app.handle_key(ch('q')), Action::None);
        assert!(!app.help);
    }

    #[test]
    fn animating_only_with_visible_working_rows() {
        let mut app = app();
        assert!(app.animating());
        app.folded.insert("payments-dashboard".into());
        app.folded.insert("shop-api".into());
        app.rebuild();
        assert!(!app.animating());
    }
}
