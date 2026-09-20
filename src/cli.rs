//! Command-line interface definition.

use clap::{Parser, Subcommand, ValueEnum};

use crate::model::ManagerKind;

/// CLI spelling of [`ManagerKind`] for clap value parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum ManagerArg {
    Apt,
    Dpkg,
    Flatpak,
    Snap,
    Brew,
    Pacman,
    Dnf,
}

impl From<ManagerArg> for ManagerKind {
    fn from(arg: ManagerArg) -> Self {
        match arg {
            ManagerArg::Apt => ManagerKind::Apt,
            ManagerArg::Dpkg => ManagerKind::Dpkg,
            ManagerArg::Flatpak => ManagerKind::Flatpak,
            ManagerArg::Snap => ManagerKind::Snap,
            ManagerArg::Brew => ManagerKind::Brew,
            ManagerArg::Pacman => ManagerKind::Pacman,
            ManagerArg::Dnf => ManagerKind::Dnf,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "bash-cli",
    version,
    about = "Inventory and search OS applications across package managers, as JSON"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List installed applications across detected package managers.
    List {
        /// Only consult these comma-separated managers
        /// (apt,dpkg,flatpak,snap,brew,pacman,dnf).
        #[arg(long, value_delimiter = ',')]
        manager: Vec<ManagerArg>,
        /// Emit single-line JSON instead of pretty-printed.
        #[arg(long)]
        compact: bool,
    },
    /// Search applications, installed or available, across detected package managers.
    Search {
        /// Text matched against package names and descriptions.
        query: String,
        /// Only consult these comma-separated managers
        /// (apt,dpkg,flatpak,snap,brew,pacman,dnf).
        #[arg(long, value_delimiter = ',')]
        manager: Vec<ManagerArg>,
        /// Emit single-line JSON instead of pretty-printed.
        #[arg(long)]
        compact: bool,
        /// Keep only installed applications.
        #[arg(long, conflicts_with = "available_only")]
        installed_only: bool,
        /// Keep only applications that are not installed.
        #[arg(long)]
        available_only: bool,
    },
}

impl Command {
    /// Managers selected through `--manager`; `None` means every detected manager.
    pub fn selected_managers(&self) -> Option<Vec<ManagerKind>> {
        let args = match self {
            Command::List { manager, .. } | Command::Search { manager, .. } => manager,
        };
        (!args.is_empty()).then(|| args.iter().map(|arg| ManagerKind::from(*arg)).collect())
    }

    /// Whether compact (single-line) JSON output was requested.
    pub fn wants_compact(&self) -> bool {
        match self {
            Command::List { compact, .. } | Command::Search { compact, .. } => *compact,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, FromArgMatches};

    fn try_parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::command()
            .try_get_matches_from(args)
            .and_then(|m| Cli::from_arg_matches(&m))
    }

    #[test]
    fn command_must_have_unique_help() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_list_without_options() {
        let cli = try_parse(&["bash-cli", "list"]).unwrap();
        assert!(matches!(cli.command, Command::List { .. }));
        assert!(cli.command.selected_managers().is_none());
        assert!(!cli.command.wants_compact());
    }

    #[test]
    fn parses_manager_list_with_delimiter() {
        let cli = try_parse(&["bash-cli", "list", "--manager", "apt,flatpak"]).unwrap();
        assert_eq!(
            cli.command.selected_managers(),
            Some(vec![ManagerKind::Apt, ManagerKind::Flatpak])
        );
    }

    #[test]
    fn rejects_unknown_manager_name() {
        assert!(try_parse(&["bash-cli", "list", "--manager", "bogus"]).is_err());
    }

    #[test]
    fn rejects_conflicting_filters() {
        assert!(try_parse(&[
            "bash-cli",
            "search",
            "curl",
            "--installed-only",
            "--available-only"
        ])
        .is_err());
    }

    #[test]
    fn parses_search_filters() {
        let cli = try_parse(&[
            "bash-cli",
            "search",
            "curl",
            "--compact",
            "--installed-only",
        ])
        .unwrap();
        let compact = cli.command.wants_compact();
        match cli.command {
            Command::Search {
                query,
                installed_only,
                available_only,
                ..
            } => {
                assert_eq!(query, "curl");
                assert!(installed_only);
                assert!(!available_only);
            }
            _ => panic!("expected search command"),
        }
        assert!(compact);
    }
}
