//! claude-tower: a control tower for the Claude Code instances running in
//! tmux. See `spec.md` for the design; section numbers (§) refer to it.

pub mod collector;
pub mod config;
pub mod doctor;
pub mod git;
pub mod list;
pub mod model;
pub mod proc;
pub mod registry;
pub mod teams;
pub mod tmux;
pub mod transcript;
pub mod ui;
