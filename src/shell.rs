//! Bash adapter: every external command in this tool runs through `bash -c`.
//!
//! Hard project requirement: the CLI only works with bash, so `Command::new("bash")`
//! is the single execution path and must not be bypassed.

use std::process::Command;

use crate::model::{ManagerError, ManagerKind};

/// Failure of a `bash -c` invocation.
#[derive(Debug)]
pub struct ShellError {
    pub command: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.exit_code {
            Some(code) => write!(f, "`{}` exited with {code}", self.command)?,
            None => write!(f, "`{}` failed to spawn", self.command)?,
        };
        if !self.stderr.trim().is_empty() {
            write!(f, ": {}", self.stderr.trim())?;
        }
        Ok(())
    }
}

/// Quote a string so bash treats it as a single argument.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Run a command through bash and return its stdout.
pub fn run(command: &str) -> Result<String, ShellError> {
    let output = Command::new("bash")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|e| ShellError {
            command: command.to_string(),
            stderr: e.to_string(),
            exit_code: None,
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(ShellError {
            command: command.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code(),
        })
    }
}

/// Run a command whose failure should surface as a manager-level error.
pub fn run_managed(manager: ManagerKind, command: &str) -> Result<String, ManagerError> {
    run(command).map_err(|e| ManagerError {
        manager,
        message: e.to_string(),
    })
}

/// True when `binary` resolves on PATH, checked through bash itself.
pub fn which(binary: &str) -> bool {
    Command::new("bash")
        .arg("-c")
        .arg(format!("command -v -- {}", quote(binary)))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_executes_through_bash() {
        assert_eq!(run("echo hi").unwrap(), "hi\n");
    }

    #[test]
    fn run_reports_failure_with_stderr() {
        let err = run("echo boom >&2; exit 3").unwrap_err();
        assert_eq!(err.exit_code, Some(3));
        assert!(err.stderr.contains("boom"));
    }

    #[test]
    fn which_detects_existing_and_missing_binaries() {
        assert!(which("bash"));
        assert!(!which("definitely-not-a-real-binary-xyz"));
    }

    #[test]
    fn quote_escapes_single_quotes() {
        assert_eq!(quote("it's"), r"'it'\''s'");
    }
}
