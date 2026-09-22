//! Orchestration: run the selected providers and shape the JSON output.

use crate::model::{App, ManagerError, ManagerKind, Output};
use crate::provider::{detect_available, query_tokens, registry};
use crate::timefmt;

/// Post-search filters for the `search` command.
#[derive(Debug, Clone, Copy, Default)]
pub struct SearchFilters {
    pub installed_only: bool,
    pub available_only: bool,
    /// Drop results whose confidence is below this value (0..1).
    pub min_confidence: f64,
    /// Blend semantic similarity into the confidence and re-rank. Defaults
    /// to off in `Default` so unit tests stay hermetic.
    pub semantic: bool,
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

    let selected_providers: Vec<&Box<dyn crate::provider::Provider>> = reg
        .iter()
        .filter(|p| {
            let kind = p.kind();
            available.contains(&kind) && selected.is_none_or(|sel| sel.contains(&kind))
        })
        .collect();

    for p in &selected_providers {
        managers_detected.push(p.kind());
    }

    let outcomes: Vec<Result<Vec<App>, ManagerError>> = std::thread::scope(|s| {
        let handles: Vec<_> = selected_providers
            .iter()
            .map(|provider| s.spawn(move || provider.outdated()))
            .collect();

        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for outcome in outcomes {
        match outcome {
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
            && dpkg_names.contains(&app.name))
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

    let selected_providers: Vec<&Box<dyn crate::provider::Provider>> = reg
        .iter()
        .filter(|p| {
            let kind = p.kind();
            available.contains(&kind) && selected.is_none_or(|sel| sel.contains(&kind))
        })
        .collect();

    for p in &selected_providers {
        managers_detected.push(p.kind());
    }

    let outcomes: Vec<Result<Vec<App>, ManagerError>> = std::thread::scope(|s| {
        let handles: Vec<_> = selected_providers
            .iter()
            .map(|provider| {
                s.spawn(move || match (query, filters.installed_only) {
                    (Some(_), true) => provider.list_installed(),
                    (Some(query), false) => provider.search(query),
                    (None, _) => provider.list_installed(),
                })
            })
            .collect();

        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for outcome in outcomes {
        match outcome {
            Ok(mut apps) => results.append(&mut apps),
            Err(e) => errors.push(e),
        }
    }

    dedup_deb_duplicates(&mut results);

    // Optional semantic layer (search only): load the index and embed the
    // query once. Used both to rescue indexed apps the lexical score rejected
    // (cross-language or synonym queries) and to re-rank the results.
    let semantic_state = if command == "search" && filters.semantic {
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
        let tokens = query_tokens(query);

        // Semantic recall first: indexed apps with high similarity become
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
                        matched_tokens: Vec::new(),
                        confidence: None,
                    });
                }
            }
        }

        // Score once: relevance points plus which query tokens matched.
        let mut scored: Vec<(i64, Vec<String>, App)> = results
            .drain(..)
            .map(|app| {
                let (score, matched) = crate::provider::score_with_matches(
                    &app.name,
                    app.description.as_deref(),
                    &tokens,
                );
                (score, matched, app)
            })
            .collect();

        // Relevance gate: zero-score results (no token hit) are dropped unless
        // the semantic layer rescues them with high similarity.
        scored.retain(|(score, _, app)| {
            *score > 0
                || semantic_state.as_ref().is_some_and(|(_, lookup, vector)| {
                    crate::semantic::similarity(lookup, &app.name, app.manager, vector)
                        >= crate::semantic::SEMANTIC_RESCUE_THRESHOLD
                })
        });

        if command == "search" {
            // Confidence: lexical score normalized against the practical
            // ceiling (every token as a whole-word name hit), blended with
            // semantic similarity (0.6 semantic / 0.4 lexical) when active.
            let max_lexical = (tokens.len() as i64 * 50).max(1);
            let mut enriched: Vec<(f64, App)> = scored
                .into_iter()
                .map(|(score, matched, mut app)| {
                    let lexical_confidence = (score as f64 / max_lexical as f64).clamp(0.0, 1.0);
                    let confidence = match &semantic_state {
                        Some((_index, lookup, vector)) => {
                            let sim =
                                crate::semantic::similarity(lookup, &app.name, app.manager, vector);
                            crate::semantic::blended_score(lexical_confidence, sim)
                        }
                        None => lexical_confidence,
                    };
                    app.matched_tokens = matched;
                    app.confidence = Some(confidence);
                    (confidence, app)
                })
                .collect();
            enriched.sort_by(|(confidence_a, app_a), (confidence_b, app_b)| {
                confidence_b
                    .partial_cmp(confidence_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| (&app_a.name, app_a.manager).cmp(&(&app_b.name, app_b.manager)))
            });
            // Min-confidence cut: drop the weak tail of the recall expansion.
            enriched.retain(|(confidence, _)| *confidence >= filters.min_confidence);
            results.extend(enriched.into_iter().map(|(_, app)| app));
        } else {
            results.extend(scored.into_iter().map(|(_, _, app)| app));
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
            matched_tokens: Vec::new(),
            confidence: None,
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
            Some("zzz"),
            None,
            SearchFilters {
                installed_only: true,
                available_only: false,
                min_confidence: 0.0,
                semantic: false,
            },
        );
        assert_eq!(installed.count, 1);
        assert!(installed.results.iter().all(|a| a.installed));

        // --available-only keeps consulting the catalogs; the installed app is
        // filtered out and the query matches the remaining one by name.
        let available = run_over(
            &reg,
            "search",
            Some("aaa"),
            None,
            SearchFilters {
                installed_only: false,
                available_only: true,
                min_confidence: 0.0,
                semantic: false,
            },
        );
        assert_eq!(available.count, 1);
        assert!(available.results.iter().all(|a| !a.installed));
    }

    #[test]
    fn min_confidence_cuts_the_weak_tail() {
        let mut strong = app("zz", ManagerKind::Dpkg, true);
        strong.description = Some("a zz helper".into()); // name word hit
        let mut weak = app("qqq", ManagerKind::Dpkg, true);
        weak.description = Some("mentions zz inside".into()); // description word hit only
        let reg: Vec<Box<dyn Provider>> = vec![
            Box::new(FakeProvider {
                kind: ManagerKind::Dpkg,
                available: true,
                apps: vec![strong],
                fail: false,
            }),
            Box::new(FakeProvider {
                kind: ManagerKind::Dpkg,
                available: true,
                apps: vec![weak],
                fail: false,
            }),
        ];
        let out = run_over(
            &reg,
            "search",
            Some("zz"),
            None,
            SearchFilters {
                min_confidence: 0.3,
                ..SearchFilters::default()
            },
        );
        // Strong: name word hit (50/50 = 1.0). Weak: description-only (10/50
        // = 0.2) falls below the cut.
        let tokens = crate::query::expand_query("zz");
        for r in &out.results {
            eprintln!(
                "DBG {} conf={:?} matched={:?} score={:?} desc={:?}",
                r.name,
                r.confidence,
                r.matched_tokens,
                crate::provider::score_with_matches(&r.name, r.description.as_deref(), &tokens),
                r.description.as_deref().unwrap_or("<none>")
            );
        }
        eprintln!("DBG count={}", out.count);
        assert_eq!(out.count, 1);
        assert_eq!(out.results[0].name, "zz");
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
                min_confidence: 0.0,
                semantic: false,
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
