//! pkgq: inventory and search OS applications across package managers as JSON.

mod bootstrap;
mod cli;
mod embeddings;
mod model;
mod provider;
mod providers;
mod query;
mod run;
mod semantic;
mod shell;
mod timefmt;
mod update;

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser as _;
use serde::Serialize;

fn main() -> ExitCode {
    // Bootstrap the semantic-search assets (bge-m3 weights + tokenizer in
    // the Hugging Face cache) on every execution; verification is a cheap
    // filesystem scan and anything missing is downloaded. PKGQ_NO_BOOTSTRAP=1
    // skips it.
    bootstrap::run_if_enabled();

    let parsed = cli::Cli::parse();

    // Validate the confidence range up front for a clear CLI error.
    if let cli::Command::Search { min_confidence, .. } = &parsed.command {
        if !(0.0..=1.0).contains(min_confidence) {
            eprintln!("pkgq: --min-confidence must be between 0 and 1");
            return ExitCode::from(2);
        }
    }

    let selected = parsed.command.selected_managers();

    let serialization: Result<serde_json::Value, serde_json::Error> = match &parsed.command {
        cli::Command::List { .. } => serde_json::to_value(run::run_list(selected.as_deref())),
        cli::Command::Outdated { .. } => {
            serde_json::to_value(run::run_outdated(selected.as_deref()))
        }
        cli::Command::Search {
            query,
            installed_only,
            available_only,
            min_confidence,
            ..
        } => serde_json::to_value(run::run_search(
            query,
            selected.as_deref(),
            run::SearchFilters {
                installed_only: *installed_only,
                available_only: *available_only,
                min_confidence: *min_confidence,
                semantic: true,
            },
        )),
        cli::Command::Index { manager, .. } => {
            let index_selected: Option<Vec<model::ManagerKind>> =
                (!manager.is_empty()).then(|| {
                    manager
                        .iter()
                        .map(|arg| model::ManagerKind::from(*arg))
                        .collect()
                });
            match semantic::build_index(index_selected.as_deref()) {
                Ok(report) => Ok(report),
                Err(errors) => Ok(serde_json::json!({
                    "command": "index",
                    "indexed": 0,
                    "errors": errors
                        .iter()
                        .map(|e| serde_json::json!({
                            "manager": e.manager,
                            "message": e.message
                        }))
                        .collect::<Vec<_>>(),
                })),
            }
        }
        cli::Command::Update { .. } => match update::run_update() {
            Ok(report) => Ok(report),
            Err(report) => {
                let _ = print_json(&report, parsed.command.wants_compact());
                return ExitCode::FAILURE;
            }
        },
    };

    match serialization {
        Ok(value) => print_json(&value, parsed.command.wants_compact()),
        Err(e) => {
            eprintln!("pkgq: failed to serialize output: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_json<T: Serialize + ?Sized>(value: &T, compact: bool) -> ExitCode {
    let json = if compact {
        serde_json::to_string(value)
    } else {
        serde_json::to_string_pretty(value)
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
