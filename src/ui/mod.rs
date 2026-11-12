//! The terminal UI (§6, §7): grouped list, details panel, filter, jump.
//!
//! `app` holds the state and handles keys, `render` draws it, and this file
//! runs the loop: terminal events, snapshots from the collector, and the
//! tmux jump on Enter.

mod ansi;
mod app;
mod cards;
#[cfg(test)]
mod fixture;
mod render;

use std::sync::mpsc::TryRecvError;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::collector::{self, Handle, Request};
use crate::config::{Config, Layout};
use crate::tmux::{self, Tmux};

use app::{Action, App};

/// How long to wait for a key before checking for snapshots again.
const POLL: Duration = Duration::from_millis(100);
const TMUX_TIMEOUT: Duration = Duration::from_secs(2);

/// Runs the dashboard until the user quits. In popup mode, exits after a
/// jump and Esc also quits.
pub fn run(config: &Config, popup: bool) -> anyhow::Result<()> {
    let handle = collector::spawn(config);
    let tmux = Tmux::new(TMUX_TIMEOUT);
    // The card view starts without the details panel; `p` shows it.
    let show_panel = config.details_panel && config.layout == Layout::List;
    let mut app =
        App::new(config.group_by, show_panel, popup, tmux::in_tmux()).with_layout(config.layout);

    // Raw mode, alternate screen, and a panic hook that restores both.
    let result = match ratatui::try_init() {
        Ok(mut terminal) => event_loop(&mut terminal, &mut app, &handle, &tmux, popup),
        Err(e) => Err(e.into()),
    };
    ratatui::restore();
    let _ = handle.requests.send(Request::Quit);
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    handle: &Handle,
    tmux: &Tmux,
    popup: bool,
) -> anyhow::Result<()> {
    let mut dirty = true;
    let mut connected = true;
    // Spinner frame last drawn; a working row needs a redraw only when it
    // changes.
    let mut drawn_frame = render::spinner_frame(now_ms());
    loop {
        let now = now_ms();
        if app.tick(now) {
            dirty = true;
        }
        if connected {
            let mut latest = None;
            loop {
                match handle.snapshots.try_recv() {
                    Ok(snapshot) => latest = Some(snapshot),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        connected = false;
                        app.set_sticky_status("collector stopped; showing the last data");
                        dirty = true;
                        break;
                    }
                }
            }
            if let Some(snapshot) = latest {
                app.set_snapshot(snapshot);
                dirty = true;
            }
        }
        if let Some(request) = app.take_preview_request() {
            let _ = handle.requests.send(request);
        }
        let frame_now = render::spinner_frame(now);
        if dirty || (app.animating() && frame_now != drawn_frame) {
            terminal.draw(|frame| render::draw(frame, app, now))?;
            dirty = false;
            drawn_frame = frame_now;
        }

        if !event::poll(POLL)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                dirty = true;
                match app.handle_key(key) {
                    Action::None => {}
                    Action::Quit => return Ok(()),
                    Action::Send(request) => {
                        let _ = handle.requests.send(request);
                    }
                    Action::Jump(pane) => {
                        let jumped = tmux
                            .dashboard_client(popup)
                            .and_then(|client| tmux.jump(&client, &pane));
                        match jumped {
                            Ok(()) if popup => return Ok(()),
                            Ok(()) => {}
                            Err(e) => app.set_status(format!("jump failed: {e}")),
                        }
                    }
                }
            }
            Event::Resize(..) => dirty = true,
            _ => {}
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
