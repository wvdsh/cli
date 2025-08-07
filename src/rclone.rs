use anyhow::{Result, Context};
use directories::ProjectDirs;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::process::Command;
use std::io::{Read, Write};
use tempfile::NamedTempFile;
use zip::ZipArchive;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const RCLONE_VERSION: &str = "1.67.0";

pub struct R2Config {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
    pub endpoint: String,
}

pub struct RcloneManager {
    rclone_path: Option<PathBuf>,
    verbose: bool,
}

impl RcloneManager {
    pub fn new(verbose: bool) -> Result<Self> {
        // First, check if rclone is available in PATH
        if Command::new("rclone").arg("version").output().is_ok() {
            if verbose {
                println!("Using system rclone from PATH");
            }
            return Ok(Self {
                rclone_path: Some(PathBuf::from("rclone")),
                verbose,
            });
        }

        // Check if we have a cached rclone binary
        if let Some(project_dirs) = ProjectDirs::from("gg", "wavedash", "wvdsh") {
            let (_, _, executable_name) = Self::get_download_info();
            let cached_binary = project_dirs.cache_dir().join("bin").join(executable_name);
            
            if cached_binary.exists() {
                if verbose {
                    println!("Using cached rclone from {}", cached_binary.display());
                }
                return Ok(Self {
                    rclone_path: Some(cached_binary),
                    verbose,
                });
            }
        }

        // Fallback to downloading rclone
        println!("rclone not found in PATH, will download on first use");
        
        Ok(Self {
            rclone_path: None,
            verbose,
        })
    }

    fn get_download_info() -> (&'static str, &'static str, &'static str) {
        match std::env::consts::OS {
            "linux" => match std::env::consts::ARCH {
                "x86_64" => ("linux-amd64", "zip", "rclone"),
                "aarch64" => ("linux-arm64", "zip", "rclone"),
                _ => panic!("Unsupported Linux architecture: {}", std::env::consts::ARCH),
            },
            "macos" => match std::env::consts::ARCH {
                "x86_64" => ("osx-amd64", "zip", "rclone"),
                "aarch64" => ("osx-arm64", "zip", "rclone"),
                _ => panic!("Unsupported macOS architecture: {}", std::env::consts::ARCH),
            },
            "windows" => match std::env::consts::ARCH {
                "x86_64" => ("windows-amd64", "zip", "rclone.exe"),
                "aarch64" => ("windows-arm64", "zip", "rclone.exe"),
                _ => panic!("Unsupported Windows architecture: {}", std::env::consts::ARCH),
            },
            _ => panic!("Unsupported OS: {}", std::env::consts::OS),
        }
    }

    async fn ensure_rclone(&mut self) -> Result<&PathBuf> {
        if let Some(ref path) = self.rclone_path {
            return Ok(path);
        }

        // Download rclone if we don't have it

        let (platform, archive_type, executable_name) = Self::get_download_info();
        
        let url = format!(
            "https://github.com/rclone/rclone/releases/download/v{}/rclone-v{}-{}.{}",
            RCLONE_VERSION, RCLONE_VERSION, platform, archive_type
        );

        println!("Downloading rclone v{}...", RCLONE_VERSION);
        
        // Use a simpler approach - just download to a known location
        let response = reqwest::get(&url).await?;
        if !response.status().is_success() {
            anyhow::bail!("Failed to download rclone: HTTP {}", response.status());
        }
        
        let bytes = response.bytes().await?;
        let mut temp_archive = tempfile::NamedTempFile::new()?;
        std::io::copy(&mut std::io::Cursor::new(bytes), &mut temp_archive)?;
        
        // Extract binary based on archive type
        let binary_path = self.extract_binary(temp_archive.path(), executable_name, archive_type)?;
        
        if self.verbose {
            println!("Downloaded and extracted rclone to {}", binary_path.display());
        }
        
        self.rclone_path = Some(binary_path);
        Ok(self.rclone_path.as_ref().unwrap())
    }

    fn extract_binary(&self, archive_path: &std::path::Path, executable_name: &str, archive_type: &str) -> Result<PathBuf> {
        let project_dirs = ProjectDirs::from("gg", "wavedash", "wvdsh")
            .context("Failed to get project directories")?;
        let bin_dir = project_dirs.cache_dir().join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        
        let binary_path = bin_dir.join(executable_name);
        
        match archive_type {
            "zip" => {
                let file = std::fs::File::open(archive_path)?;
                let mut archive = ZipArchive::new(file)?;
                
                for i in 0..archive.len() {
                    let mut file = archive.by_index(i)?;
                    if file.name().ends_with(executable_name) {
                        let mut out_file = std::fs::File::create(&binary_path)?;
                        std::io::copy(&mut file, &mut out_file)?;
                        
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let mut perms = out_file.metadata()?.permissions();
                            perms.set_mode(0o755);
                            std::fs::set_permissions(&binary_path, perms)?;
                        }
                        
                        return Ok(binary_path);
                    }
                }
                anyhow::bail!("rclone binary not found in zip archive");
            }
            _ => anyhow::bail!("Unsupported archive type: {}", archive_type),
        }
    }


    fn create_rclone_config(&self, config: &R2Config) -> Result<NamedTempFile> {
        let mut config_file = NamedTempFile::new()
            .context("Failed to create temporary config file")?;

        let config_content = format!(
            r#"[r2]
type = s3
provider = Cloudflare
access_key_id = {}
secret_access_key = {}
session_token = {}
endpoint = {}
acl = private
"#,
            config.access_key_id,
            config.secret_access_key,
            config.session_token,
            config.endpoint
        );

        config_file.write_all(config_content.as_bytes())
            .context("Failed to write rclone config")?;
        
        Ok(config_file)
    }

    fn parse_rclone_progress(line: &str, verbose: bool) -> Option<u64> {
        // Strip ANSI escape codes first
        let clean_line = strip_ansi_escapes::strip_str(line);
        
        if verbose {
            println!("rclone output: '{}'", clean_line);
        }
        
        // Parse lines like "Transferred: 486.181G / 926.373 GBytes, 52%, 13.589 MBytes/s, ETA 9h12m49s"
        if clean_line.contains("Transferred:") && clean_line.contains("%") {
            // Find the percentage - it's between two commas or comma and space
            if let Some(percent_pos) = clean_line.find('%') {
                // Look backward from % to find the start of the percentage number
                let before_percent = &clean_line[..percent_pos];
                
                // Find the last comma or space before the percentage
                let start_pos = before_percent.rfind(',')
                    .or_else(|| before_percent.rfind(' '))
                    .map(|pos| pos + 1)
                    .unwrap_or(0);
                
                let percent_str = before_percent[start_pos..].trim();
                if let Ok(percent) = percent_str.parse::<f64>() {
                    if verbose {
                        println!("Parsed progress: {}%", percent);
                    }
                    return Some(percent as u64);
                }
            }
        }
        None
    }

    pub async fn sync_to_r2(
        &mut self,
        source: &str,
        bucket: &str,
        prefix: &str,
        config: &R2Config,
        verbose: bool,
    ) -> Result<()> {
        let config_file = self.create_rclone_config(config)?;
        let destination = format!("r2:{}/{}", bucket, prefix);
        let rclone_path = self.ensure_rclone().await?;
        
        // Convert source to absolute path to avoid PTY working directory issues
        let source_path = std::path::Path::new(source);
        let absolute_source = if source_path.is_absolute() {
            source.to_string()
        } else {
            std::env::current_dir()?.join(source_path).to_string_lossy().to_string()
        };

        let args = vec![
            "sync",
            &absolute_source,
            &destination,
            "--config", config_file.path().to_str().unwrap(),
            "--progress",
            "--stats", "250ms",
            "--checksum",
        ];

        // Create a progress bar for the upload
        let pb = ProgressBar::new(100);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("[{bar:40.cyan/blue}] {pos:>3}% {msg}")
                .unwrap(),
        );
        pb.set_message("Uploading build to R2...");

        if verbose {
            println!("Running rclone with args: {:?}", args);
        }

        // Use PTY to get real-time output
        let pty_system = native_pty_system();
        let pty_pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(rclone_path);
        for arg in &args {
            cmd.arg(arg);
        }

        let mut child = pty_pair.slave.spawn_command(cmd)?;
        
        // Read from PTY master asynchronously
        let mut reader = pty_pair.master.try_clone_reader()?;
        
        let pb_clone = pb.clone();
        let read_task = tokio::task::spawn_blocking(move || {
            let mut buffer = [0u8; 1024];
            let mut line_buffer = String::new();
            
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buffer[..n]);
                        line_buffer.push_str(&chunk);
                        
                        // Process complete lines
                        while let Some(newline_pos) = line_buffer.find('\n') {
                            let line = line_buffer[..newline_pos].to_string();
                            line_buffer = line_buffer[newline_pos + 1..].to_string();
                            
                            if verbose {
                                println!("pty line: '{}'", line);
                            }
                            
                            if let Some(percent) = Self::parse_rclone_progress(&line, verbose) {
                                pb_clone.set_position(percent.min(100));
                            }
                        }
                        
                        // Also check for carriage return (for real-time updates)
                        while let Some(cr_pos) = line_buffer.find('\r') {
                            let line = line_buffer[..cr_pos].to_string();
                            line_buffer = line_buffer[cr_pos + 1..].to_string();
                            
                            if verbose {
                                println!("pty line (CR): '{}'", line);
                            }
                            
                            if let Some(percent) = Self::parse_rclone_progress(&line, verbose) {
                                pb_clone.set_position(percent.min(100));
                            }
                        }
                    }
                    Err(e) => {
                        if verbose {
                            println!("PTY read error: {}", e);
                        }
                        break;
                    }
                }
            }
        });

        // Wait for the process to finish
        let status = child.wait()?;
        
        // Wait for reading task to complete
        let _ = read_task.await;

        if !status.success() {
            pb.finish_with_message("❌ Upload failed");
            anyhow::bail!("rclone sync failed with exit code: {:?}", status);
        }

        pb.finish_with_message("✓ Build uploaded successfully!");
        Ok(())
    }
}