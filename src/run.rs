//! Orchestration: run the selected providers and shape the JSON output.

use crate::model::{App, ManagerError, ManagerKind, Output};
use crate::provider::{detect_available, registry};
use crate::timefmt;

/// Post-search filters for the `search` command.
#[derive(Debug, Clone, Copy, Default)]
pub struct SearchFilters {
    pub installed_only: bool,
    pub available_only: bool,
}

/// Run `list` over every detected (optionally filtered) manager.
pub fn run_list(selected: Option<&[ManagerKind]>) -> Output {
    let reg = registry();
    run_over(&reg, "list", None, selected, SearchFilters::default())
}

/// Run `search` over every detected (optionally filtered) manager.
pub fn run_search(query: &str, selected: Option<&[ManagerKind]>, filters: SearchFilters) -> Output {
    let reg = registry();
    run_over(&reg, "search", Some(query), selected, filters)
}

/// Shared pipeline, taking the registry as a parameter for testability.
pub fn run_over(
    reg: &[Box<dyn crate::provider::Provider>],
    command: &str,
    query: Option<&str>,
    selected: Option<&[ManagerKind]>,
    filters: SearchFilters,
) -> Output {
    let available = detect_available(reg);
    let mut results: Vec<App> = Vec::new();
    let mut errors: Vec<ManagerError> = Vec::new();
    let mut managers_detected: Vec<ManagerKind> = Vec::new();

    for provider in reg {
        let kind = provider.kind();
        // Canonical order and no duplicates come from the registry itself.
        if !available.contains(&kind) {
            continue;
        }
        if let Some(sel) = selected {
            if !sel.contains(&kind) {
                continue;
            }
        }
        managers_detected.push(kind);
        let outcome = match query {
            Some(q) => provider.search(q),
            None => provider.list_installed(),
        };
        match outcome {
            Ok(mut apps) => results.append(&mut apps),
            Err(e) => errors.push(e),
        }
    }

    if filters.installed_only {
        results.retain(|app| app.installed);
    }
    if filters.available_only {
        results.retain(|app| !app.installed);
    }
    results.sort_by(|a, b| (&a.name, a.manager).cmp(&(&b.name, b.manager)));

    Output {
        command: command.to_string(),
        query: query.map(str::to_string),
        managers_detected,
        generated_at: timefmt::now_rfc3339_utc(),
        count: results.len(),
        results,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Provider;

    struct FakeProvider {
        kind: ManagerKind,
        available: bool,
        apps: Vec<App>,
        fail: bool,
    }

    impl Provider for FakeProvider {
        fn kind(&self) -> ManagerKind {
            self.kind
        }
        fn is_available(&self) -> bool {
            self.available
        }
        fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
            if self.fail {
                Err(ManagerError {
                    manager: self.kind,
                    message: "boom".into(),
                })
            } else {
                Ok(self.apps.clone())
            }
        }
        fn search(&self, _query: &str) -> Result<Vec<App>, ManagerError> {
            self.list_installed()
        }
    }

    fn app(name: &str, manager: ManagerKind, installed: bool) -> App {
        App {
            name: name.to_string(),
            manager,
            installed,
            version: None,
            description: None,
            usage: None,
            install: None,
            installed_bytes: None,
            download_bytes: None,
            homepage: None,
            license: None,
            origin: None,
            arch: None,
            maintainer: None,
            section: None,
            depends: None,
            install_date: None,
        }
    }

    fn fixtures() -> Vec<Box<dyn Provider>> {
        vec![
            Box::new(FakeProvider {
                kind: ManagerKind::Snap,
                available: true,
                apps: vec![
                    app("zzz", ManagerKind::Snap, true),
                    app("aaa", ManagerKind::Snap, false),
                ],
                fail: false,
            }),
            Box::new(FakeProvider {
                kind: ManagerKind::Brew,
                available: false,
                apps: vec![app("hidden", ManagerKind::Brew, true)],
                fail: false,
            }),
            Box::new(FakeProvider {
                kind: ManagerKind::Apt,
                available: true,
                apps: vec![],
                fail: true,
            }),
        ]
    }

    #[test]
    fn skips_unavailable_managers_and_records_errors() {
        let out = run_over(&fixtures(), "list", None, None, SearchFilters::default());
        assert_eq!(
            out.managers_detected,
            vec![ManagerKind::Snap, ManagerKind::Apt]
        );
        assert_eq!(out.count, 2);
        assert_eq!(out.errors.len(), 1);
        assert_eq!(out.errors[0].manager, ManagerKind::Apt);
    }

    #[test]
    fn results_are_sorted_by_name_then_manager() {
        let out = run_over(&fixtures(), "list", None, None, SearchFilters::default());
        let names: Vec<&str> = out.results.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["aaa", "zzz"]);
    }

    #[test]
    fn manager_filter_restricts_consulted_managers() {
        let out = run_over(
            &fixtures(),
            "list",
            None,
            Some(&[ManagerKind::Snap]),
            SearchFilters::default(),
        );
        assert_eq!(out.managers_detected, vec![ManagerKind::Snap]);
        assert_eq!(out.count, 2);
        assert!(out.errors.is_empty());
    }

    #[test]
    fn search_filters_apply_on_installed_flag() {
        let reg = fixtures();
        let installed = run_over(
            &reg,
            "search",
            Some("q"),
            None,
            SearchFilters {
                installed_only: true,
                available_only: false,
            },
        );
        assert_eq!(installed.count, 1);
        assert!(installed.results.iter().all(|a| a.installed));

        let available = run_over(
            &reg,
            "search",
            Some("q"),
            None,
            SearchFilters {
                installed_only: false,
                available_only: true,
            },
        );
        assert_eq!(available.count, 1);
        assert!(available.results.iter().all(|a| !a.installed));
    }

    #[test]
    fn output_contract_fields_are_filled() {
        let out = run_over(&[], "list", None, None, SearchFilters::default());
        assert_eq!(out.command, "list");
        assert_eq!(out.query, None);
        assert!(out.managers_detected.is_empty());
        assert!(out.results.is_empty());
        assert!(out.errors.is_empty());
        assert_eq!(out.count, 0);
        assert_eq!(out.generated_at.len(), 20);
        assert!(out.generated_at.ends_with('Z'));
    }
}
