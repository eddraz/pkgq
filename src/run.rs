//! Orchestration: run the selected providers and shape the JSON output.

use crate::model::{App, ManagerError, ManagerKind, Output};
use crate::provider::{detect_available, query_tokens, registry, relevance_score};
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

/// Run `outdated` over every detected (optionally filtered) manager.
pub fn run_outdated(selected: Option<&[ManagerKind]>) -> Output {
    let reg = registry();
    run_outdated_over(&reg, selected)
}

/// Shared outdated pipeline, taking the registry as a parameter for testability.
pub fn run_outdated_over(
    reg: &[Box<dyn crate::provider::Provider>],
    selected: Option<&[ManagerKind]>,
) -> Output {
    let available = detect_available(reg);
    let mut results: Vec<App> = Vec::new();
    let mut errors: Vec<ManagerError> = Vec::new();
    let mut managers_detected: Vec<ManagerKind> = Vec::new();

    for provider in reg {
        let kind = provider.kind();
        if !available.contains(&kind) {
            continue;
        }
        if let Some(sel) = selected {
            if !sel.contains(&kind) {
                continue;
            }
        }
        managers_detected.push(kind);
        match provider.outdated() {
            Ok(mut apps) => results.append(&mut apps),
            Err(e) => errors.push(e),
        }
    }

    results.sort_by(|a, b| (&a.name, a.manager).cmp(&(&b.name, b.manager)));

    Output {
        command: "outdated".to_string(),
        query: None,
        managers_detected,
        generated_at: timefmt::now_rfc3339_utc(),
        count: results.len(),
        results,
        errors,
    }
}

/// De-duplicate the deb world: an installed deb reported by both dpkg and apt
/// stays attributed to dpkg (the source of truth for what is installed);
/// catalog-only apt hits are unaffected.
pub(crate) fn dedup_deb_duplicates(results: &mut Vec<App>) {
    let dpkg_names: Vec<String> = results
        .iter()
        .filter(|app| app.manager == ManagerKind::Dpkg)
        .map(|app| app.name.clone())
        .collect();
    if dpkg_names.is_empty() {
        return;
    }
    results.retain(|app| {
        !(app.manager == ManagerKind::Apt
            && app.installed
            && dpkg_names.iter().any(|name| *name == app.name))
    });
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
        let outcome = match (query, filters.installed_only) {
            // Fast path: --installed-only never touches remote catalogs; the
            // local inventories are scored directly, so the command works
            // offline and returns instantly.
            (Some(_), true) => provider.list_installed(),
            (Some(query), false) => provider.search(query),
            (None, _) => provider.list_installed(),
        };
        match outcome {
            Ok(mut apps) => results.append(&mut apps),
            Err(e) => errors.push(e),
        }
    }

    dedup_deb_duplicates(&mut results);

    // Optional semantic layer (search only): load the index and embed the
    // query once. Used both to rescue indexed apps the lexical score rejected
    // (cross-language or synonym queries) and to re-rank the results.
    let semantic_state = if command == "search" {
        query.and_then(|query| {
            let index = crate::semantic::load_index()?;
            let lookup = crate::semantic::embedding_lookup(&index);
            let vector = crate::semantic::embed_texts(&[query.to_string()])
                .ok()?
                .into_iter()
                .next()?;
            Some((index, lookup, vector))
        })
    } else {
        None
    };

    if let Some(query) = query {
        // Relevance gate: zero-score results (no token hit) are dropped unless
        // the semantic layer rescues them with high similarity.
        let tokens = query_tokens(query);
        results.retain(|app| {
            if relevance_score(&app.name, app.description.as_deref(), &tokens) > 0 {
                return true;
            }
            if let Some((_, lookup, vector)) = &semantic_state {
                return crate::semantic::similarity(lookup, &app.name, app.manager, vector)
                    >= crate::semantic::SEMANTIC_RESCUE_THRESHOLD;
            }
            false
        });
        if command == "search" {
            // Semantic recall: indexed apps with high similarity become
            // candidates even when no lexical token matched (cross-language
            // queries). The similarity lookup is O(1).
            if let Some((index, lookup, vector)) = &semantic_state {
                let present: std::collections::HashSet<(String, ManagerKind)> = results
                    .iter()
                    .map(|app| (app.name.clone(), app.manager))
                    .collect();
                for item in &index.items {
                    let similarity =
                        crate::semantic::similarity(lookup, &item.name, item.manager, vector);
                    if similarity >= crate::semantic::SEMANTIC_RESCUE_THRESHOLD
                        && !present.contains(&(item.name.clone(), item.manager))
                    {
                        results.push(App {
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
                            name: item.name.clone(),
                            manager: item.manager,
                            installed: item.installed,
                            version: None,
                            description: item.description.clone(),
                            available_version: None,
                        });
                    }
                }
            }
            let mut scored: Vec<(i64, App)> = results
                .drain(..)
                .map(|app| {
                    let score = relevance_score(&app.name, app.description.as_deref(), &tokens);
                    (score, app)
                })
                .collect();
            scored.sort_by(|(score_a, app_a), (score_b, app_b)| {
                score_b
                    .cmp(&score_a)
                    .then_with(|| (&app_a.name, app_a.manager).cmp(&(&app_b.name, app_b.manager)))
            });
            // Semantic re-rank: blend similarity (0.6) with the normalized
            // lexical score (0.4) when the index and server are available.
            if let Some((_, lookup, vector)) = &semantic_state {
                let max_lexical = scored
                    .iter()
                    .map(|(score, _)| *score)
                    .max()
                    .unwrap_or(0)
                    .max(1);
                scored.sort_by(|(score_a, app_a), (score_b, app_b)| {
                    let blended_a = crate::semantic::blended_score(
                        *score_a,
                        max_lexical,
                        crate::semantic::similarity(lookup, &app_a.name, app_a.manager, vector),
                    );
                    let blended_b = crate::semantic::blended_score(
                        *score_b,
                        max_lexical,
                        crate::semantic::similarity(lookup, &app_b.name, app_b.manager, vector),
                    );
                    blended_b
                        .partial_cmp(&blended_a)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| {
                            (&app_a.name, app_a.manager).cmp(&(&app_b.name, app_b.manager))
                        })
                });
            }
            results.extend(scored.into_iter().map(|(_, app)| app));
        }
    }
    if filters.installed_only {
        results.retain(|app| app.installed);
    }
    if filters.available_only {
        results.retain(|app| !app.installed);
    }
    if command != "search" {
        results.sort_by(|a, b| (&a.name, a.manager).cmp(&(&b.name, b.manager)));
    }

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
            available_version: None,
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
        // With --installed-only the fast path matches the local inventories
        // directly; the installed fixture app is named `zzz`.
        let reg = fixtures();
        let installed = run_over(
            &reg,
            "search",
            Some("zz"),
            None,
            SearchFilters {
                installed_only: true,
                available_only: false,
            },
        );
        assert_eq!(installed.count, 1);
        assert!(installed.results.iter().all(|a| a.installed));

        // --available-only keeps consulting the catalogs; the installed app is
        // filtered out and the query matches the remaining one by name.
        let available = run_over(
            &reg,
            "search",
            Some("aa"),
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

    /// Provider that returns a marker app from its catalog search, so tests
    /// can prove the catalog was never consulted.
    struct SpyProvider {
        installed: Vec<App>,
    }

    impl Provider for SpyProvider {
        fn kind(&self) -> ManagerKind {
            ManagerKind::Flatpak
        }
        fn is_available(&self) -> bool {
            true
        }
        fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
            Ok(self.installed.clone())
        }
        fn search(&self, _query: &str) -> Result<Vec<App>, ManagerError> {
            Ok(vec![app("FROM-CATALOG", ManagerKind::Flatpak, false)])
        }
    }

    #[test]
    fn installed_only_fast_path_skips_catalog_search() {
        let mut drift = app("Drift", ManagerKind::Flatpak, true);
        drift.description = Some("Edit and export videos easily".into());
        let remote_only = app("Openshot", ManagerKind::Flatpak, false);
        let reg: Vec<Box<dyn Provider>> = vec![Box::new(SpyProvider {
            installed: vec![drift, remote_only],
        })];

        let out = run_over(
            &reg,
            "search",
            Some("video"),
            None,
            SearchFilters {
                installed_only: true,
                available_only: false,
            },
        );
        assert_eq!(out.count, 1);
        assert_eq!(out.results[0].name, "Drift");
        assert!(out.results[0].installed);
        // The remote catalog was never consulted.
        assert!(out.results.iter().all(|a| a.name != "FROM-CATALOG"));
    }

    #[test]
    fn deb_duplicates_across_apt_and_dpkg_keep_dpkg() {
        let reg: Vec<Box<dyn Provider>> = vec![
            Box::new(FakeProvider {
                kind: ManagerKind::Dpkg,
                available: true,
                apps: vec![app("curl", ManagerKind::Dpkg, true)],
                fail: false,
            }),
            Box::new(FakeProvider {
                kind: ManagerKind::Apt,
                available: true,
                apps: vec![
                    app("curl", ManagerKind::Apt, true),
                    app("curlpp", ManagerKind::Apt, false),
                ],
                fail: false,
            }),
        ];
        let out = run_over(&reg, "search", Some("curl"), None, SearchFilters::default());
        let curls: Vec<&App> = out.results.iter().filter(|a| a.name == "curl").collect();
        assert_eq!(curls.len(), 1);
        assert_eq!(curls[0].manager, ManagerKind::Dpkg);
        // Catalog-only apt hits survive.
        assert!(out.results.iter().any(|a| a.name == "curlpp"));
    }
}
