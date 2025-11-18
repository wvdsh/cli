use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use axum_server::tls_rustls::RustlsConfig;
use directories::ProjectDirs;
use rcgen::{Certificate, CertificateParams, DnType, SanType, PKCS_ECDSA_P256_SHA256};

const DEV_CERT_SUBDIR: &str = "dev-server";
const DEV_CERT_NAME: &str = "localhost-cert.pem";
const DEV_KEY_NAME: &str = "localhost-key.pem";
const DEV_CERT_COMMON_NAME: &str = "wvdsh dev server";

#[cfg(target_os = "linux")]
const LINUX_CERT_INSTALL_PATH: &str = "/usr/local/share/ca-certificates/wvdsh-dev.crt";

pub async fn load_or_create_certificates() -> Result<(RustlsConfig, PathBuf, PathBuf)> {
    let project_dirs = ProjectDirs::from("gg", "Wavedash", "wvdsh")
        .ok_or_else(|| anyhow::anyhow!("Unable to determine config directory for certificates"))?;
    let cert_dir = project_dirs.config_dir().join(DEV_CERT_SUBDIR);
    fs::create_dir_all(&cert_dir)?;

    let cert_path = cert_dir.join(DEV_CERT_NAME);
    let key_path = cert_dir.join(DEV_KEY_NAME);

    if !cert_path.exists() || !key_path.exists() {
        create_self_signed_cert(&cert_path, &key_path)?;
    }

    let cert_pem = fs::read(&cert_path)
        .with_context(|| format!("Failed to read certificate at {}", cert_path.display()))?;
    let key_pem = fs::read(&key_path)
        .with_context(|| format!("Failed to read private key at {}", key_path.display()))?;

    let rustls_config = RustlsConfig::from_pem(cert_pem, key_pem).await?;

    Ok((rustls_config, cert_path, key_path))
}

pub fn ensure_cert_trusted(cert_path: &Path) -> Result<()> {
    platform::PlatformCertManager::ensure_trusted(cert_path)
}

fn create_self_signed_cert(cert_path: &Path, key_path: &Path) -> Result<()> {
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, DEV_CERT_COMMON_NAME);
    params.subject_alt_names = vec![
        SanType::DnsName("localhost".into()),
        SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)),
    ];
    params.alg = &PKCS_ECDSA_P256_SHA256;

    let cert = Certificate::from_params(params)?;
    let cert_pem = cert.serialize_pem()?;
    let key_pem = cert.serialize_private_key_pem();

    fs::write(cert_path, cert_pem)?;
    fs::write(key_path, key_pem)?;

    Ok(())
}

fn trust_marker_path(cert_path: &Path) -> PathBuf {
    let mut marker = cert_path.to_path_buf();
    marker.set_extension(format!("trusted.{}", std::env::consts::OS));
    marker
}

trait CertTrustManager {
    fn ensure_trusted(cert_path: &Path) -> Result<()>;
}

mod platform {

    #[cfg(target_os = "linux")]
    pub(crate) use linux::LinuxCertManager as PlatformCertManager;
    #[cfg(target_os = "macos")]
    pub(crate) use macos::MacosCertManager as PlatformCertManager;
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub(crate) use unsupported::UnsupportedCertManager as PlatformCertManager;
    #[cfg(target_os = "windows")]
    pub(crate) use windows::WindowsCertManager as PlatformCertManager;

    #[cfg(target_os = "macos")]
    mod macos {
        use super::super::{trust_marker_path, CertTrustManager, DEV_CERT_COMMON_NAME};
        use anyhow::{Context, Result};
        use dialoguer::{Confirm, Password};
        use std::fs;
        use std::io::Write;
        use std::path::Path;
        use std::process::{Command, Stdio};

        pub(crate) struct MacosCertManager;

        impl CertTrustManager for MacosCertManager {
            fn ensure_trusted(cert_path: &Path) -> Result<()> {
                let marker = trust_marker_path(cert_path);
                if marker.exists() {
                    match cert_present(DEV_CERT_COMMON_NAME) {
                        Ok(true) => return Ok(()),
                        Ok(false) => {
                            println!(
                                "⚠️  Detected missing macOS trust entry for {}. Re-installing certificate...",
                                DEV_CERT_COMMON_NAME
                            );
                        }
                        Err(err) => {
                            println!(
                                "⚠️  Could not verify macOS trust state ({}). Re-installing certificate...",
                                err
                            );
                        }
                    }
                    let _ = fs::remove_file(&marker);
                }

                println!(
                    "⚠️  macOS needs to trust the generated localhost certificate for HTTPS previews."
                );
                if !Confirm::new()
                    .with_prompt("Add the certificate to the System keychain now?")
                    .default(true)
                    .interact()?
                {
                    println!(
                        "Skipping trust step. You can trust {} manually later (security add-trusted-cert ...).",
                        cert_path.display()
                    );
                    return Ok(());
                }

                let password = Password::new()
                    .with_prompt("Enter your macOS password (used for sudo)")
                    .allow_empty_password(false)
                    .interact()?;

                let mut child = Command::new("sudo")
                    .arg("-S")
                    .arg("security")
                    .arg("add-trusted-cert")
                    .arg("-d")
                    .arg("-r")
                    .arg("trustRoot")
                    .arg("-k")
                    .arg("/Library/Keychains/System.keychain")
                    .arg(cert_path)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .context("Failed to spawn sudo security command")?;

                {
                    let stdin = child
                        .stdin
                        .as_mut()
                        .context("Failed to open sudo stdin for password")?;
                    stdin.write_all(password.as_bytes())?;
                    stdin.write_all(b"\n")?;
                }

                let output = child.wait_with_output()?;
                if !output.status.success() {
                    anyhow::bail!(
                        "Failed to trust certificate: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }

                fs::write(&marker, b"trusted")?;
                println!("✓ Trusted dev certificate in the macOS System keychain.");
                Ok(())
            }
        }

        fn cert_present(common_name: &str) -> Result<bool> {
            let output = Command::new("security")
                .arg("find-certificate")
                .arg("-c")
                .arg(common_name)
                .arg("/Library/Keychains/System.keychain")
                .output()
                .context("failed to run security find-certificate")?;

            if output.status.success() {
                return Ok(true);
            }

            if matches!(output.status.code(), Some(44)) {
                return Ok(false);
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("could not be found in the keychain") {
                return Ok(false);
            }

            anyhow::bail!(stderr.trim().to_string())
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::super::{
            trust_marker_path, CertTrustManager, DEV_CERT_COMMON_NAME, LINUX_CERT_INSTALL_PATH,
        };
        use anyhow::{Context, Result};
        use dialoguer::{Confirm, Password};
        use std::fs;
        use std::io::Write;
        use std::path::Path;
        use std::process::{Command, Stdio};

        pub(crate) struct LinuxCertManager;

        impl CertTrustManager for LinuxCertManager {
            fn ensure_trusted(cert_path: &Path) -> Result<()> {
                let marker = trust_marker_path(cert_path);
                if marker.exists() {
                    match cert_matches(cert_path) {
                        Ok(true) => return Ok(()),
                        Ok(false) => {
                            println!(
                                "⚠️  Detected missing Linux trust entry for {}. Re-installing certificate...",
                                DEV_CERT_COMMON_NAME
                            );
                        }
                        Err(err) => {
                            println!(
                                "⚠️  Could not verify Linux trust state ({}). Re-installing certificate...",
                                err
                            );
                        }
                    }
                    let _ = fs::remove_file(&marker);
                }

                println!(
                    "⚠️  Linux needs to trust the generated localhost certificate for HTTPS previews."
                );
                if !Confirm::new()
                    .with_prompt("Install the certificate into the system trust store now?")
                    .default(true)
                    .interact()?
                {
                    println!(
                        "Skipping trust step. You can place {} into /usr/local/share/ca-certificates \
                         and run your distro's CA update command later.",
                        cert_path.display()
                    );
                    return Ok(());
                }

                let password = Password::new()
                    .with_prompt("Enter your sudo password")
                    .allow_empty_password(false)
                    .interact()?;

                install_certificate(cert_path, &password)?;

                fs::write(&marker, b"trusted")?;
                println!("✓ Trusted dev certificate in the Linux system trust store.");
                Ok(())
            }
        }

        fn install_certificate(cert_path: &Path, password: &str) -> Result<()> {
            let cert_str = cert_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Certificate path is not valid UTF-8"))?;
            let dest = Path::new(LINUX_CERT_INSTALL_PATH);
            let dest_dir = dest
                .parent()
                .unwrap_or_else(|| Path::new("/usr/local/share/ca-certificates"));

            run_sudo(
                password,
                &["mkdir", "-p", dest_dir.to_str().unwrap_or_default()],
            )?;
            run_sudo(
                password,
                &["cp", cert_str, dest.to_str().unwrap_or_default()],
            )?;
            run_sudo(
                password,
                &["chmod", "644", dest.to_str().unwrap_or_default()],
            )?;

            let attempts: &[&[&str]] = &[
                &["update-ca-certificates"],
                &["update-ca-trust"],
                &["update-ca-trust", "extract"],
                &["trust", "extract-compat"],
            ];

            for args in attempts {
                if run_sudo(password, args).is_ok() {
                    return Ok(());
                }
            }

            println!(
                "⚠️  Could not update the Linux trust store automatically. \
                 Please run your distribution's CA update command manually so browsers trust {}.",
                dest.display()
            );

            Ok(())
        }

        fn cert_matches(cert_path: &Path) -> Result<bool> {
            let dest = Path::new(LINUX_CERT_INSTALL_PATH);
            if !dest.exists() {
                return Ok(false);
            }

            let installed =
                fs::read(dest).with_context(|| format!("failed to read {}", dest.display()))?;
            let expected = fs::read(cert_path)
                .with_context(|| format!("failed to read {}", cert_path.display()))?;

            Ok(installed == expected)
        }

        fn run_sudo(password: &str, args: &[&str]) -> Result<()> {
            let mut child = Command::new("sudo");
            child.arg("-S");
            for arg in args {
                child.arg(arg);
            }
            child
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            let mut child = child.spawn().context("Failed to run sudo command")?;
            {
                let stdin = child
                    .stdin
                    .as_mut()
                    .context("Failed to open sudo stdin for password")?;
                stdin.write_all(password.as_bytes())?;
                stdin.write_all(b"\n")?;
            }

            let output = child.wait_with_output()?;
            if !output.status.success() {
                anyhow::bail!(
                    "sudo {:?} failed: {}",
                    args,
                    String::from_utf8_lossy(&output.stderr)
                );
            }

            Ok(())
        }
    }

    #[cfg(target_os = "windows")]
    mod windows {
        use super::super::{trust_marker_path, CertTrustManager, DEV_CERT_COMMON_NAME};
        use anyhow::{Context, Result};
        use dialoguer::Confirm;
        use std::fs;
        use std::path::Path;
        use std::process::Command;

        pub(crate) struct WindowsCertManager;

        impl CertTrustManager for WindowsCertManager {
            fn ensure_trusted(cert_path: &Path) -> Result<()> {
                let marker = trust_marker_path(cert_path);
                if marker.exists() {
                    match cert_present(DEV_CERT_COMMON_NAME) {
                        Ok(true) => return Ok(()),
                        Ok(false) => {
                            println!(
                                "⚠️  Detected missing Windows trust entry for {}. Re-installing certificate...",
                                DEV_CERT_COMMON_NAME
                            );
                        }
                        Err(err) => {
                            println!(
                                "⚠️  Could not verify Windows trust state ({}). Re-installing certificate...",
                                err
                            );
                        }
                    }
                    let _ = fs::remove_file(&marker);
                }

                println!(
                    "⚠️  Windows needs to trust the generated localhost certificate for HTTPS previews."
                );
                if !Confirm::new()
                    .with_prompt("Add the certificate to your Windows Root store now?")
                    .default(true)
                    .interact()?
                {
                    println!(
                        "Skipping trust step. You can trust {} later via certutil.",
                        cert_path.display()
                    );
                    return Ok(());
                }

                let cert_str = cert_path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Certificate path is not valid UTF-8"))?;

                let output = Command::new("certutil")
                    .args(["-user", "-addstore", "Root", "-f", cert_str])
                    .output()
                    .context("Failed to run certutil")?;

                if !output.status.success() {
                    anyhow::bail!(
                        "certutil failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }

                fs::write(&marker, b"trusted")?;
                println!("✓ Trusted dev certificate in the Windows Root store (current user).");
                Ok(())
            }
        }

        fn cert_present(common_name: &str) -> Result<bool> {
            let output = Command::new("certutil")
                .args(["-user", "-store", "Root"])
                .output()
                .context("Failed to run certutil -store Root")?;

            if !output.status.success() {
                anyhow::bail!(
                    "certutil -store Root failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let needle = format!("CN={}", common_name);
            Ok(stdout.contains(&needle))
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    mod unsupported {
        use super::super::{CertTrustManager, DEV_CERT_COMMON_NAME};
        use anyhow::Result;
        use std::path::Path;

        pub(crate) struct UnsupportedCertManager;

        impl CertTrustManager for UnsupportedCertManager {
            fn ensure_trusted(cert_path: &Path) -> Result<()> {
                println!(
                    "⚠️  Automatic certificate trust is not yet supported on {}. \
                     Please trust {} manually if browsers warn about HTTPS.",
                    std::env::consts::OS,
                    cert_path.display()
                );
                println!(
                    "   The dev certificate is issued for common name '{}'.",
                    DEV_CERT_COMMON_NAME
                );
                Ok(())
            }
        }
    }
}
