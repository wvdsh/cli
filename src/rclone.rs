use anyhow::{Result, Context};
use directories::ProjectDirs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::NamedTempFile;
use std::io::{Write, Cursor};
use zip::ZipArchive;

const RCLONE_VERSION: &str = "1.67.0";

pub struct R2Config {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
    pub endpoint: String,
}

pub struct RcloneManager {
    rclone_path: Option<PathBuf>,
}

impl RcloneManager {
    pub fn new() -> Result<Self> {
        // First, check if rclone is available in PATH
        if Command::new("rclone").arg("version").output().is_ok() {
            println!("✓ Using system rclone");
            return Ok(Self {
                rclone_path: Some(PathBuf::from("rclone")),
            });
        }

        // Fallback to downloading rclone
        println!("rclone not found in PATH, will download on first use");
        
        Ok(Self {
            rclone_path: None,
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

    pub async fn run_rclone(&mut self, args: &[&str]) -> Result<std::process::Output> {
        let rclone_path = self.ensure_rclone().await?;
        
        let output = Command::new(rclone_path)
            .args(args)
            .output()
            .context("Failed to execute rclone command")?;

        Ok(output)
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

    pub async fn sync_to_r2(
        &mut self,
        source: &str,
        bucket: &str,
        prefix: &str,
        config: &R2Config,
    ) -> Result<()> {
        let config_file = self.create_rclone_config(config)?;
        let destination = format!("r2:{}/{}", bucket, prefix);
        
        let args = vec![
            "sync",
            source,
            &destination,
            "--config", config_file.path().to_str().unwrap(),
            "--progress",
            "--stats", "10s",
            "--checksum",
        ];

        println!("Uploading build to R2...");
        let output = self.run_rclone(&args).await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("rclone sync failed: {}", stderr);
        }

        println!("✓ Successfully uploaded to R2: {}", destination);
        Ok(())
    }
}