//! Spawn the Wavedash Dev subprocess and shuttle the IPC contract.
//!
//! IPC contract (mirrored in cli/dev-app/src/main.ts):
//!   - CLI → dev-app stdin: one JSON line of [`DevAppConfig`].
//!   - dev-app → CLI stdout: one JSON object per line.
//!     - `{"type":"ready"}` after first did-finish-load.
//!     - `{"type":"closed"}` immediately before dev-app app.quit().
//!   - dev-app stderr: free-form logs forwarded as-is. By default only the
//!     `serve_local` access lines reach the user; the dev-app itself gates
//!     `synth`, `passthrough`, "listening on …", etc. behind `--verbose`,
//!     and `--verbose` here also adds CLI-side ready/exit echoes plus the
//!     raw stdout fallback.
//!
//! Liveness: closing our stdin end signals the dev-app to quit. We rely on
//! that for clean shutdown when the user hits Ctrl+C.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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

pub async fn run_dev_app(
    launch: DevAppLaunch,
    config: &DevAppConfig,
    user_data_dir: &str,
) -> Result<()> {
    let verbose = config.verbose;

    let mut cmd = tokio::process::Command::new(&launch.command);
    // The dev-app needs the user-data-dir synchronously at module load (see
    // dev-app/src/main.ts) — pass it as a CLI flag, not via stdin, so it's
    // available before Electron's internal path resolver readies itself.
    cmd.args(&launch.args)
        .arg(format!("--user-data-dir={}", user_data_dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &launch.cwd {
        cmd.current_dir(cwd);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn dev-app: {:?}", launch.command))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("dev-app child stdin missing"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("dev-app child stdout missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("dev-app child stderr missing"))?;

    let line = serde_json::to_string(config)? + "\n";
    stdin
        .write_all(line.as_bytes())
        .await
        .with_context(|| "Failed to write dev-app config")?;
    stdin.flush().await?;

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

    // Closing our end of stdin signals the dev-app to quit.
    drop(stdin);

    let exit = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
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
