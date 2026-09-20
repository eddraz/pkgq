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
