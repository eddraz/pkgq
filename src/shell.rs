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

const DEFAULT_TIMEOUT_SECS: u64 = 60;

fn cmd_timeout() -> std::time::Duration {
    let secs = std::env::var("PKGQ_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

/// Run a command through bash with the default timeout and return its stdout.
pub fn run(command: &str) -> Result<String, ShellError> {
    run_with_timeout(command, cmd_timeout())
}

/// Run a command through bash with an explicit timeout duration.
pub fn run_with_timeout(command: &str, timeout: std::time::Duration) -> Result<String, ShellError> {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(command)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ShellError {
            command: command.to_string(),
            stderr: e.to_string(),
            exit_code: None,
        })?;

    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    let child = std::sync::Arc::new(std::sync::Mutex::new(child));
    let child_clone = std::sync::Arc::clone(&child);

    let (tx, rx) = std::sync::mpsc::channel();

    let reader_thread = std::thread::spawn(move || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        use std::io::Read;
        if let Some(mut out) = stdout_handle {
            let _ = out.read_to_end(&mut stdout);
        }
        if let Some(mut err) = stderr_handle {
            let _ = err.read_to_end(&mut stderr);
        }
        let status = child_clone.lock().unwrap().wait();
        let _ = tx.send((status, stdout, stderr));
    });

    match rx.recv_timeout(timeout) {
        Ok((status, stdout, stderr)) => {
            let _ = reader_thread.join();
            match status {
                Ok(status) if status.success() => Ok(String::from_utf8_lossy(&stdout).into_owned()),
                Ok(status) => Err(ShellError {
                    command: command.to_string(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    exit_code: status.code(),
                }),
                Err(e) => Err(ShellError {
                    command: command.to_string(),
                    stderr: e.to_string(),
                    exit_code: None,
                }),
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.lock().unwrap().kill();
            let _ = reader_thread.join();
            Err(ShellError {
                command: command.to_string(),
                stderr: format!("command timed out after {}s", timeout.as_secs()),
                exit_code: None,
            })
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            let _ = reader_thread.join();
            Err(ShellError {
                command: command.to_string(),
                stderr: "command execution channel disconnected".to_string(),
                exit_code: None,
            })
        }
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

    #[test]
    fn run_times_out_on_hung_command() {
        let err = run_with_timeout("sleep 2", std::time::Duration::from_millis(50)).unwrap_err();
        assert!(err.stderr.contains("timed out"));
    }
}
