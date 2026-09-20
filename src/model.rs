//! Domain model: package-manager kinds, applications, and the JSON output contract.

use serde::Serialize;

/// Every package manager this tool knows how to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ManagerKind {
    Apt,
    Dpkg,
    Flatpak,
    Snap,
    Brew,
    Pacman,
    Dnf,
}

impl ManagerKind {
    /// The executable that must exist on PATH for this manager to be detected.
    pub fn binary(&self) -> &'static str {
        match self {
            ManagerKind::Apt => "apt-cache",
            ManagerKind::Dpkg => "dpkg-query",
            ManagerKind::Flatpak => "flatpak",
            ManagerKind::Snap => "snap",
            ManagerKind::Brew => "brew",
            ManagerKind::Pacman => "pacman",
            ManagerKind::Dnf => "dnf",
        }
    }
}

/// One application as reported by one package manager.
#[derive(Debug, Clone, Serialize)]
pub struct App {
    pub name: String,
    pub manager: ManagerKind,
    pub installed: bool,
    pub version: Option<String>,
    pub description: Option<String>,
    /// Primary executable invocation, e.g. `curl --version`; null when unknowable.
    pub usage: Option<String>,
    /// Command a user would run to install the application.
    pub install: Option<String>,
    /// On-disk installed size when installed, download size when the app is
    /// only available; bytes. Null when the manager does not expose it.
    pub installed_bytes: Option<u64>,
    /// Download size when the app is only available; null otherwise.
    pub download_bytes: Option<u64>,
    /// Project homepage; null when the manager does not expose it.
    pub homepage: Option<String>,
    /// License; null when the manager does not expose it.
    pub license: Option<String>,
    /// Repository, remote, tap or channel the app comes from.
    pub origin: Option<String>,
    /// Target architecture; null when not exposed or not applicable.
    pub arch: Option<String>,
    /// Packager or publisher; null when the manager does not expose it.
    pub maintainer: Option<String>,
    /// Package section or group (e.g. `web`, `video`); null when not exposed.
    pub section: Option<String>,
    /// Raw dependency list as reported by the manager; null when not exposed.
    pub depends: Option<String>,
    /// When the app was installed. RFC3339 when derivable, otherwise the
    /// manager-reported string; null for available-only apps.
    pub install_date: Option<String>,
    /// Newer version available for this installed app; only filled by the
    /// `outdated` command.
    pub available_version: Option<String>,
}

/// Parse human-formatted sizes such as `70.2 MB`, `77MB`, `5.36 MiB`, `512 B`.
/// Comma decimal separators are accepted; units ending in `iB` use binary
/// multiples (KiB = 1024 B), anything else uses SI decimal multiples
/// (KB = 1000 B).
pub(crate) fn parse_human_size(text: &str) -> Option<u64> {
    let cleaned = text.trim().replace(',', ".");
    let digit_index = cleaned.find(|c: char| c.is_ascii_digit())?;
    let rest = &cleaned[digit_index..];
    let number_end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    let number: f64 = rest[..number_end].parse().ok()?;
    // Units may be separated by any non-alphabetic filler (spaces, or a
    // literal `?` when glib downgrades a narrow no-break space to ASCII),
    // so skip until the unit letters begin.
    let unit_start = rest[number_end..]
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(rest.len() - number_end);
    let unit: String = rest[number_end + unit_start..]
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_uppercase();
    let multiplier = match unit.as_str() {
        "" | "B" => 1.0,
        u if u.ends_with("IB") => {
            let power = match &u[..u.len() - 2] {
                "K" => 1,
                "M" => 2,
                "G" => 3,
                "T" => 4,
                "P" => 5,
                _ => return None,
            };
            1024_f64.powi(power)
        }
        "K" | "KB" => 1_000.0,
        "M" | "MB" => 1_000_000.0,
        "G" | "GB" => 1_000_000_000.0,
        "T" | "TB" => 1_000_000_000_000.0,
        "P" | "PB" => 1_000_000_000_000_000.0,
        _ => return None,
    };
    Some((number * multiplier) as u64)
}

/// A manager-level failure; the command still exits 0 with this recorded in `errors`.
#[derive(Debug, Clone, Serialize)]
pub struct ManagerError {
    pub manager: ManagerKind,
    pub message: String,
}

/// Frozen v1 JSON output contract. Field names and order are part of the contract.
#[derive(Debug, Serialize)]
pub struct Output {
    pub command: String,
    pub query: Option<String>,
    pub managers_detected: Vec<ManagerKind>,
    pub generated_at: String,
    pub count: usize,
    pub results: Vec<App>,
    pub errors: Vec<ManagerError>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_kind_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(ManagerKind::Dpkg).unwrap(),
            serde_json::json!("dpkg")
        );
    }

    #[test]
    fn app_serializes_none_fields_as_null() {
        let app = App {
            name: "curl".into(),
            manager: ManagerKind::Apt,
            installed: false,
            version: Some("8.14.1".into()),
            description: None,
            usage: None,
            install: Some("sudo apt install curl".into()),
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
        };
        // serde_json::to_value normalizes into a sorted map, so field order
        // must be asserted against the serialized string itself.
        let s = serde_json::to_string(&app).unwrap();
        let keys = [
            "\"name\":",
            "\"manager\":",
            "\"installed\":",
            "\"version\":",
            "\"description\":",
            "\"usage\":",
            "\"install\":",
            "\"installed_bytes\":",
            "\"download_bytes\":",
            "\"homepage\":",
            "\"license\":",
            "\"origin\":",
            "\"arch\":",
            "\"maintainer\":",
            "\"section\":",
            "\"depends\":",
            "\"install_date\":",
            "\"available_version\":",
        ];
        let mut last = 0;
        for k in keys {
            let pos = s
                .find(k)
                .unwrap_or_else(|| panic!("key {k} missing in {s}"));
            assert!(pos > last, "key {k} out of order in {s}");
            last = pos;
        }
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj["description"], serde_json::json!(null));
        assert_eq!(obj["usage"], serde_json::json!(null));
        assert_eq!(obj["version"], serde_json::json!("8.14.1"));
    }

    #[test]
    fn parses_human_sizes_across_unit_conventions() {
        assert_eq!(parse_human_size("70.2 MB"), Some(70_200_000));
        assert_eq!(parse_human_size("70,2 MB"), Some(70_200_000));
        assert_eq!(parse_human_size("77MB"), Some(77_000_000));
        assert_eq!(parse_human_size("512 B"), Some(512));
        assert_eq!(parse_human_size("1 GB"), Some(1_000_000_000));
        assert_eq!(
            parse_human_size("5.36 MiB"),
            Some((5.36 * 1_048_576.0) as u64)
        );
        assert_eq!(parse_human_size("2 GiB"), Some(2 * 1_073_741_824));
        // glib downgrades the narrow no-break space to `?` in the C locale.
        assert_eq!(parse_human_size("70.2?MB"), Some(70_200_000));
        assert_eq!(parse_human_size("no size"), None);
        assert_eq!(parse_human_size(""), None);
    }

    #[test]
    fn output_field_order_matches_contract() {
        let out = Output {
            command: "list".into(),
            query: None,
            managers_detected: vec![ManagerKind::Apt],
            generated_at: "2026-01-01T00:00:00Z".into(),
            count: 0,
            results: vec![],
            errors: vec![],
        };
        let s = serde_json::to_string(&out).unwrap();
        let keys = [
            "\"command\":",
            "\"query\":",
            "\"managers_detected\":",
            "\"generated_at\":",
            "\"count\":",
            "\"results\":",
            "\"errors\":",
        ];
        let mut last = 0;
        for k in keys {
            let pos = s.find(k).expect(k);
            assert!(pos > last, "key {k} out of order in {s}");
            last = pos;
        }
    }
}
