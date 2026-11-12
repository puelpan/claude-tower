use std::process::ExitCode;

use clap::{Parser, Subcommand};

use claude_tower::config::Config;
use claude_tower::model::GroupBy;

/// A control tower for Claude Code instances running in tmux.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Run as a tmux popup: exit after a jump; Esc closes it.
    #[arg(long)]
    popup: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum GroupArg {
    Tmux,
    Repo,
    None,
}

impl From<GroupArg> for GroupBy {
    fn from(g: GroupArg) -> GroupBy {
        match g {
            GroupArg::Tmux => GroupBy::Tmux,
            GroupArg::Repo => GroupBy::Repo,
            GroupArg::None => GroupBy::None,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// One line per running instance.
    List {
        /// JSON for scripts.
        #[arg(long)]
        json: bool,
        /// Include instances whose process is gone.
        #[arg(long, short)]
        all: bool,
        /// Group by tmux session, repository, or not at all.
        #[arg(long, value_enum)]
        group: Option<GroupArg>,
    },
    /// Check the setup.
    Doctor,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("claude-tower: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let result = match cli.command {
        None => claude_tower::ui::run(&config, cli.popup),
        Some(Command::List { json, all, group }) => {
            claude_tower::list::run(&config, json, all, group.map(GroupBy::from))
        }
        Some(Command::Doctor) => {
            return if claude_tower::doctor::run(&config) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("claude-tower: {e:#}");
            ExitCode::FAILURE
        }
    }
}
