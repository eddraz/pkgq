//! Provider abstraction: one implementation per package manager.

use crate::model::{App, ManagerError, ManagerKind};
use crate::shell;

/// A package manager backend the CLI can query.
pub trait Provider {
    fn kind(&self) -> ManagerKind;

    /// Whether the manager's binary is present on this system.
    fn is_available(&self) -> bool {
        shell::which(self.kind().binary())
    }

    /// Applications currently installed through this manager.
    fn list_installed(&self) -> Result<Vec<App>, ManagerError>;

    /// Search installed and available applications matching `query`.
    fn search(&self, query: &str) -> Result<Vec<App>, ManagerError>;
}

/// All providers, in canonical `ManagerKind::ALL` order.
pub fn registry() -> Vec<Box<dyn Provider>> {
    use crate::providers::{apt::Apt, dpkg::Dpkg, flatpak::Flatpak, snap::Snap};
    vec![
        Box::new(Apt),
        Box::new(Dpkg),
        Box::new(Flatpak),
        Box::new(Snap),
    ]
}

/// Kinds of the providers that are usable on this system, in registry order.
pub fn detect_available(registry: &[Box<dyn Provider>]) -> Vec<ManagerKind> {
    registry
        .iter()
        .filter(|p| p.is_available())
        .map(|p| p.kind())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProvider(ManagerKind, bool);

    impl Provider for FakeProvider {
        fn kind(&self) -> ManagerKind {
            self.0
        }
        fn is_available(&self) -> bool {
            self.1
        }
        fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
            Ok(vec![])
        }
        fn search(&self, _query: &str) -> Result<Vec<App>, ManagerError> {
            Ok(vec![])
        }
    }

    #[test]
    fn detect_available_filters_by_availability() {
        let registry: Vec<Box<dyn Provider>> = vec![
            Box::new(FakeProvider(ManagerKind::Apt, true)),
            Box::new(FakeProvider(ManagerKind::Brew, false)),
        ];
        assert_eq!(detect_available(&registry), vec![ManagerKind::Apt]);
    }
}
