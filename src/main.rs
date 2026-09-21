//! pkgq: inventory and search OS applications across package managers as JSON.

mod cli;
mod model;
mod provider;
mod providers;
mod query;
mod run;
mod shell;
mod timefmt;

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser as _;

fn main() -> ExitCode {
    let parsed = cli::Cli::parse();
    let selected = parsed.command.selected_managers();

    let output = match &parsed.command {
        cli::Command::List { .. } => run::run_list(selected.as_deref()),
        cli::Command::Outdated { .. } => run::run_outdated(selected.as_deref()),
        cli::Command::Search {
            query,
            installed_only,
            available_only,
            ..
        } => run::run_search(
            query,
            selected.as_deref(),
            run::SearchFilters {
                installed_only: *installed_only,
                available_only: *available_only,
            },
        ),
    };

    let json = if parsed.command.wants_compact() {
        serde_json::to_string(&output)
    } else {
        serde_json::to_string_pretty(&output)
    };

    match json {
        Ok(body) => {
            let stdout = io::stdout();
            let mut lock = stdout.lock();
            let _ = writeln!(lock, "{body}");
            let _ = lock.flush();
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("pkgq: failed to serialize output: {e}");
            ExitCode::FAILURE
        }
    }
}
