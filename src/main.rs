//! bash-cli: inventory and search OS applications across package managers as JSON.

mod cli;
mod model;
mod provider;
mod providers;
mod run;
mod shell;
mod timefmt;

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser as _;

fn main() -> ExitCode {
    let parsed = cli::Cli::parse();

    let output = match &parsed.command {
        cli::Command::List { manager, .. } => run::run_list(selected(manager).as_deref()),
        cli::Command::Search {
            query,
            manager,
            installed_only,
            available_only,
            ..
        } => run::run_search(
            query,
            selected(manager).as_deref(),
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
            eprintln!("bash-cli: failed to serialize output: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Map the CLI manager arguments to domain kinds; `None` means every detected manager.
fn selected(args: &[cli::ManagerArg]) -> Option<Vec<model::ManagerKind>> {
    (!args.is_empty()).then(|| {
        args.iter()
            .map(|arg| model::ManagerKind::from(*arg))
            .collect()
    })
}
