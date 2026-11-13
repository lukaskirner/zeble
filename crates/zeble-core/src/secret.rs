//! Getting a password by running a shell command, e.g. `op read op://…`.

use std::{process::Stdio, time::Duration};

use anyhow::{anyhow, bail};
use tokio::process::Command;

use crate::config::ConnectionConfig;

/// How long a password command may run before it is killed.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Kills the command's whole process group when dropped, so programs the
/// shell started die with it on timeout or cancel (killing only the shell
/// would leave them running).
struct GroupKill(Option<u32>);

impl GroupKill {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: plain syscall; the group was created for this child.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

/// The user's login shell, so PATH and environment from their shell profile
/// apply even when the app was started from Finder or the Dock.
fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Run `command` with the login shell and return its output as the password.
pub async fn run_password_command(command: &str) -> anyhow::Result<String> {
    run_with_shell(&login_shell(), command, COMMAND_TIMEOUT).await
}

/// Run `shell -l -c command`. Stdout minus one trailing newline is the
/// password; a non-zero exit, empty output or timeout is an error. The child
/// is killed if this future is dropped (e.g. the user cancelled).
pub async fn run_with_shell(
    shell: &str,
    command: &str,
    timeout: Duration,
) -> anyhow::Result<String> {
    if command.trim().is_empty() {
        bail!("The password command is empty");
    }
    let mut cmd = Command::new(shell);
    cmd.arg("-l")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd
        .spawn()
        .map_err(|e| anyhow!("Could not start the password command with {shell}: {e}"))?;
    let mut group = GroupKill(child.id());
    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;
    if result.is_ok() {
        group.disarm();
    }
    drop(group);
    let output = match result {
        Ok(out) => out.map_err(|e| anyhow!("The password command failed: {e}"))?,
        Err(_) => bail!(
            "The password command did not finish within {} s",
            timeout.as_secs()
        ),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first: Vec<&str> = stderr
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(3)
            .collect();
        let code = output
            .status
            .code()
            .map_or_else(|| "a signal".to_string(), |c| format!("code {c}"));
        if first.is_empty() {
            bail!("The password command exited with {code}");
        }
        bail!(
            "The password command exited with {code}: {}",
            first.join(" / ")
        );
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| anyhow!("The password command printed something that is not UTF-8"))?;
    let password = stdout
        .strip_suffix('\n')
        .map(|s| s.strip_suffix('\r').unwrap_or(s))
        .unwrap_or(&stdout)
        .to_string();
    if password.is_empty() {
        bail!("The password command printed nothing");
    }
    Ok(password)
}

/// The password to log in with: the command's output when the connection
/// has a password command, otherwise the stored password.
pub async fn resolve_password(config: &ConnectionConfig) -> anyhow::Result<String> {
    match config.password_command() {
        Some(cmd) => run_password_command(cmd).await,
        None => Ok(config.password.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cmd: &str) -> anyhow::Result<String> {
        crate::runtime().block_on(run_with_shell("/bin/sh", cmd, Duration::from_secs(5)))
    }

    #[test]
    fn trims_only_the_trailing_newline() {
        assert_eq!(run("echo ' s3cret '").unwrap(), " s3cret ");
        assert_eq!(run("printf 'a\\r\\n'").unwrap(), "a");
        assert_eq!(run("printf 'no-newline'").unwrap(), "no-newline");
        assert_eq!(run("printf 'two\\n\\n'").unwrap(), "two\n");
    }

    #[test]
    fn failures_are_errors() {
        let e = run("echo 'not signed in' >&2; exit 3")
            .unwrap_err()
            .to_string();
        assert!(e.contains("code 3") && e.contains("not signed in"), "{e}");
        assert!(
            run("true")
                .unwrap_err()
                .to_string()
                .contains("printed nothing")
        );
        assert!(run("   ").unwrap_err().to_string().contains("empty"));
        assert!(run("definitely-not-a-command-zeble").is_err());
    }

    #[test]
    fn times_out_and_kills_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        // A child of the shell that would create the marker after 1 s.
        let cmd = format!("(sleep 1; touch '{}') & sleep 10; echo x", marker.display());
        let started = std::time::Instant::now();
        let e = crate::runtime()
            .block_on(run_with_shell("/bin/sh", &cmd, Duration::from_millis(300)))
            .unwrap_err()
            .to_string();
        assert!(e.contains("did not finish"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            !marker.exists(),
            "a process from the command outlived the timeout"
        );
    }

    #[test]
    fn cancelling_kills_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let cmd = format!("(sleep 1; touch '{}') & wait; echo x", marker.display());
        let task = crate::runtime()
            .spawn(async move { run_with_shell("/bin/sh", &cmd, Duration::from_secs(10)).await });
        std::thread::sleep(Duration::from_millis(300));
        task.abort();
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!marker.exists(), "the command kept running after cancel");
    }

    #[test]
    fn uses_a_login_shell() {
        // A fake shell that prints its arguments shows how it was called.
        let dir = tempfile::tempdir().unwrap();
        let shell = dir.path().join("fake-shell");
        std::fs::write(&shell, "#!/bin/sh\necho \"$@\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let out = crate::runtime()
            .block_on(run_with_shell(
                shell.to_str().unwrap(),
                "op read x",
                Duration::from_secs(5),
            ))
            .unwrap();
        assert_eq!(out, "-l -c op read x");
    }
}
