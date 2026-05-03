//! Spawn the Wavedash Dev subprocess and shuttle the IPC contract.
//!
//! IPC contract (mirrored in cli/dev-app/src/main.ts):
//!   - CLI → dev-app: a JSON [`DevAppConfig`] written to a temp file whose
//!     path is passed as `--config-path=<path>`. The dev-app reads it
//!     synchronously at startup and unlinks it. Stdin is intentionally not
//!     used: on Windows the packaged dev-app is a GUI subsystem .exe, and
//!     Windows detaches GUI processes from inherited stdin pipes
//!     (electron/electron#11680, #4218, #21705), so `process.stdin` fires
//!     `end` before any data arrives.
//!   - dev-app → CLI stdout: one JSON object per line.
//!     - `{"type":"ready"}` after first did-finish-load.
//!     - `{"type":"closed"}` immediately before dev-app app.quit().
//!   - dev-app stderr: free-form logs forwarded as-is. By default only the
//!     `serve_local` access lines reach the user; the dev-app itself gates
//!     `synth`, `passthrough`, "listening on …", etc. behind `--verbose`,
//!     and `--verbose` here also adds CLI-side ready/exit echoes plus the
//!     raw stdout fallback.
//!
//! Liveness: the dev-app polls `--parent-pid=<pid>` every second and quits
//! if that process is gone, so a CLI crash doesn't leave an orphan. On a
//! clean Ctrl+C we kill the child directly — there's no in-band shutdown
//! signal anymore.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::signal;

use super::dev_app::DevAppLaunch;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevAppConfig {
    pub upload_dir: String,
    pub game_subdomain: String,
    pub playtest_url: String,
    pub verbose: bool,
}

/// Removes the temp config file when dropped, so a crash between write and
/// dev-app pickup doesn't leave a stale file in `%TEMP%`/`/tmp`. The dev-app
/// also unlinks on read; whichever fires first wins.
struct ConfigFileGuard(PathBuf);

impl Drop for ConfigFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub async fn run_dev_app(
    launch: DevAppLaunch,
    config: &DevAppConfig,
    user_data_dir: &str,
) -> Result<()> {
    let verbose = config.verbose;

    let parent_pid = std::process::id();
    let config_path = std::env::temp_dir().join(format!("wavedash-dev-config-{parent_pid}.json"));
    std::fs::write(&config_path, serde_json::to_vec(config)?).with_context(|| {
        format!(
            "Failed to write dev-app config to {}",
            config_path.display()
        )
    })?;
    let _config_guard = ConfigFileGuard(config_path.clone());

    let mut cmd = tokio::process::Command::new(&launch.command);
    // The dev-app needs the user-data-dir synchronously at module load (see
    // dev-app/src/main.ts). Both the config path and parent PID are read at
    // the same point, so they go on argv too.
    cmd.args(&launch.args)
        .arg(format!("--user-data-dir={}", user_data_dir))
        .arg(format!("--config-path={}", config_path.display()))
        .arg(format!("--parent-pid={parent_pid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &launch.cwd {
        cmd.current_dir(cwd);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn dev-app: {:?}", launch.command))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("dev-app child stdout missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("dev-app child stderr missing"))?;

    let stderr_task = tokio::spawn(async move {
        // Always passthrough — dev-app stderr is the dev-facing access log
        // (per-request lines from server.ts) plus errors. Both are worth
        // surfacing without `--verbose`.
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            // Benign macOS AppKit noise from Electron's NSMenu teardown after
            // a context menu closes; harmless and frequent.
            if line.contains("representedObject is not a WeakPtrToElectronMenuModelAsNSObject") {
                continue;
            }
            eprintln!("{line}");
        }
    });

    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<()>();
    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        let mut closed_tx = Some(closed_tx);
        while let Ok(Some(line)) = reader.next_line().await {
            let parsed: Option<serde_json::Value> = serde_json::from_str(&line).ok();
            if let Some(value) = parsed {
                match value.get("type").and_then(|t| t.as_str()) {
                    Some("ready") => {
                        if verbose {
                            eprintln!("ready");
                        }
                    }
                    Some("closed") => {
                        if let Some(tx) = closed_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                    _ => {
                        if verbose {
                            eprintln!("{line}");
                        }
                    }
                }
            } else if verbose {
                eprintln!("{line}");
            }
        }
    });

    tokio::select! {
        _ = signal::ctrl_c() => {
            println!("\nReceived Ctrl+C, shutting down...");
        }
        _ = closed_rx => {
            println!("\nWavedash dev-app closed, shutting down...");
        }
    }

    // Both shutdown paths converge here. On `closed`, app.quit() is already
    // running on the dev-app side, so the wait usually returns immediately.
    // On Ctrl+C there's no in-band quit signal, so the timeout fires and we
    // hard-kill — the dev-app's parent-PID watchdog would also catch this
    // once we exit, but killing is faster and avoids relying on it.
    let exit = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            child.wait().await?
        }
    };

    let _ = stderr_task.await;
    let _ = stdout_task.await;

    if !exit.success() {
        // Don't fail the whole CLI on a non-zero dev-app exit during shutdown
        // — the user already chose to quit. Just log under verbose.
        if verbose {
            eprintln!("dev-app exited with status {exit}");
        }
    }

    Ok(())
}
