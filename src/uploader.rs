use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::{stream, StreamExt, TryStreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use opendal::services::S3;
use opendal::Operator;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use walkdir::WalkDir;

const DEFAULT_CONCURRENCY: usize = 10;
const WRITE_BUFFER_SIZE: usize = 8 * 1024 * 1024; // 8 MiB

#[derive(Debug)]
pub struct R2Config {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
    pub endpoint: String,
}

#[derive(Debug)]
struct ManifestEntry {
    path: PathBuf,
    key: String,
    size: u64,
}

pub struct R2Uploader {
    operator: Operator,
    bucket: String,
    concurrency: usize,
}

impl R2Uploader {
    pub fn new(config: &R2Config, bucket: &str) -> Result<Self> {
        let mut builder = S3::default()
            .access_key_id(&config.access_key_id)
            .secret_access_key(&config.secret_access_key)
            .endpoint(&config.endpoint)
            .bucket(bucket)
            .region("auto");

        if !config.session_token.is_empty() {
            builder = builder.session_token(&config.session_token);
        }

        let operator = Operator::new(builder)?
            .finish();

        Ok(Self {
            operator,
            bucket: bucket.to_string(),
            concurrency: DEFAULT_CONCURRENCY,
        })
    }

    #[allow(dead_code)]
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        if concurrency > 0 {
            self.concurrency = concurrency;
        }
        self
    }

    pub async fn upload_directory(
        &self,
        source_dir: &Path,
        prefix: &str,
        verbose: bool,
    ) -> Result<()> {
        let source_dir = source_dir
            .canonicalize()
            .with_context(|| format!("Failed to resolve {}", source_dir.display()))?;

        let (manifest, total_bytes) = build_manifest(&source_dir, prefix)?;
        if manifest.is_empty() {
            anyhow::bail!("No files found in {}", source_dir.display());
        }

        if verbose {
            println!(
                "Uploading {} files ({} total) from {} to bucket '{}' with prefix '{}'",
                manifest.len(),
                format_bytes(total_bytes),
                source_dir.display(),
                self.bucket,
                prefix
            );
        }

        let pb = create_progress_bar(total_bytes);
        let uploaded_bytes = Arc::new(AtomicU64::new(0));
        let total_bytes = total_bytes;
        let operator = self.operator.clone();
        let concurrency = self.concurrency.max(1);

        stream::iter(manifest.into_iter().map(|entry| {
            let operator = operator.clone();
            let pb = pb.clone();
            let uploaded_bytes = uploaded_bytes.clone();

            async move {
                upload_file(&operator, &entry).await?;

                let new_total =
                    uploaded_bytes.fetch_add(entry.size, Ordering::Relaxed) + entry.size;
                let clamped = new_total.min(total_bytes);
                pb.set_position(clamped);
                pb.set_message(format!(
                    "{} / {}",
                    format_bytes(clamped),
                    format_bytes(total_bytes)
                ));

                Ok::<(), anyhow::Error>(())
            }
        }))
        .buffer_unordered(concurrency)
        .try_collect::<Vec<_>>()
        .await?;

        pb.finish_with_message("✓ Build uploaded successfully!");
        Ok(())
    }
}

fn build_manifest(source_dir: &Path, prefix: &str) -> Result<(Vec<ManifestEntry>, u64)> {
    let mut entries = Vec::new();
    let mut total_bytes = 0u64;

    for entry in WalkDir::new(source_dir).follow_links(false) {
        let entry = entry.with_context(|| {
            format!(
                "Failed to walk directory while scanning {}",
                source_dir.display()
            )
        })?;

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.into_path();
        let metadata = path
            .metadata()
            .with_context(|| format!("Failed to read metadata for {}", path.display()))?;
        let size = metadata.len();
        let relative = path
            .strip_prefix(source_dir)
            .with_context(|| format!("Failed to calculate relative path for {}", path.display()))?;
        let key = build_object_key(prefix, relative);

        entries.push(ManifestEntry { path, key, size });
        total_bytes = total_bytes.saturating_add(size);
    }

    Ok((entries, total_bytes))
}

fn build_object_key(prefix: &str, relative: &Path) -> String {
    let relative_key = relative
        .components()
        .map(|comp| comp.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");

    if prefix.is_empty() {
        relative_key
    } else {
        format!(
            "{}/{}",
            prefix.trim_end_matches('/'),
            relative_key.trim_start_matches('/')
        )
    }
}

async fn upload_file(operator: &Operator, entry: &ManifestEntry) -> Result<()> {
    let mut reader = File::open(&entry.path)
        .await
        .with_context(|| format!("Failed to open {}", entry.path.display()))?;
    let mut writer = operator
        .writer(&entry.key)
        .await
        .with_context(|| format!("Failed to create writer for {}", entry.key))?;

    let mut buffer = vec![0u8; WRITE_BUFFER_SIZE];
    loop {
        let bytes_read = reader
            .read(&mut buffer)
            .await
            .with_context(|| format!("Failed to read {}", entry.path.display()))?;

        if bytes_read == 0 {
            break;
        }

        writer
            .write(buffer[..bytes_read].to_vec())
            .await
            .with_context(|| format!("Failed to write {}", entry.key))?;
    }

    writer
        .close()
        .await
        .with_context(|| format!("Failed to finalize {}", entry.key))?;
    Ok(())
}

fn create_progress_bar(total_bytes: u64) -> ProgressBar {
    let pb = ProgressBar::new(total_bytes);
    pb.set_style(
        ProgressStyle::with_template("[{bar:40.cyan/blue}] {percent:>3}% {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );
    pb.set_message(format!("0 B / {}", format_bytes(total_bytes)));
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }

    let mut value = bytes as f64;
    let mut unit_index = 0usize;

    while value >= 1024.0 && unit_index < UNITS.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{:.0} {}", value, UNITS[unit_index])
    } else {
        format!("{:.2} {}", value, UNITS[unit_index])
    }
}

