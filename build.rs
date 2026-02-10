fn main() {
    println!("cargo:rerun-if-changed=src/version.rs");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(feature = "bundled-cli")]
    bundled_cli::download();
}

#[cfg(feature = "bundled-cli")]
mod bundled_cli {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Download the Claude Code CLI binary to the bundled directory.
    ///
    /// This runs during `cargo build` when the `bundled-cli` feature is enabled.
    /// The CLI is downloaded to `~/.claude/sdk/bundled/{version}/claude` so it
    /// persists across `cargo clean` and can be shared by multiple projects.
    pub fn download() {
        let version = parse_cli_version();
        let bundled_dir = get_bundled_dir(&version);
        let cli_path = bundled_dir.join(cli_name());

        // Incremental build: skip if already downloaded
        if cli_path.exists() {
            return;
        }

        fs::create_dir_all(&bundled_dir).unwrap_or_else(|e| {
            panic!(
                "Failed to create bundled CLI directory {}: {}",
                bundled_dir.display(),
                e
            )
        });

        eprintln!("cargo:warning=Downloading Claude Code CLI v{version}...");

        #[cfg(not(target_os = "windows"))]
        download_unix(&version, &bundled_dir);

        #[cfg(target_os = "windows")]
        download_windows(&version, &bundled_dir);

        assert!(
            cli_path.exists(),
            "CLI binary not found after download at: {}. \
             Please check your network connection and that curl is installed.",
            cli_path.display()
        );

        eprintln!(
            "cargo:warning=Claude CLI v{version} downloaded to: {}",
            cli_path.display()
        );
    }

    /// Download CLI on Unix-like systems using the official install script.
    ///
    /// install.sh accepts a version argument via `$1`:
    ///   `curl -fsSL https://claude.ai/install.sh | bash -s -- <version>`
    /// It installs the CLI to `~/.local/bin/claude` by default.
    /// We then copy the installed binary to the bundled directory atomically.
    ///
    /// **Note**: install.sh will overwrite `~/.local/bin/claude` as a side effect.
    /// If you have a different CLI version installed there, it will be replaced
    /// with the version specified by `CLI_VERSION`. After copying to the bundled
    /// directory, we restore the original binary if one existed.
    #[cfg(not(target_os = "windows"))]
    fn download_unix(version: &str, bundled_dir: &Path) {
        let home = dirs_home();
        let default_install_path = home.join(".local/bin/claude");

        // Backup existing CLI binary to avoid overwriting user's installation
        let had_existing = default_install_path.exists();
        let backup_path = home.join(".local/bin/.claude.sdk-backup");
        if had_existing {
            let _ = fs::copy(&default_install_path, &backup_path);
        }

        // Run install.sh with the pinned version
        let install_cmd = format!(
            "curl -fsSL https://claude.ai/install.sh | bash -s -- '{}'",
            version
        );

        let status = Command::new("bash")
            .args(["-c", &install_cmd])
            .status()
            .unwrap_or_else(|e| {
                // Restore backup before panicking
                if had_existing {
                    let _ = fs::rename(&backup_path, &default_install_path);
                }
                panic!(
                    "Failed to execute install script. Is curl installed? Error: {}",
                    e
                )
            });

        if !status.success() {
            // Restore backup before panicking
            if had_existing {
                let _ = fs::rename(&backup_path, &default_install_path);
            }
            panic!(
                "Claude CLI install script failed with exit code: {:?}. \
                 Check your network connection.",
                status.code()
            );
        }

        // Find the installed binary from known locations
        let search_paths = [
            default_install_path.clone(),
            PathBuf::from("/usr/local/bin/claude"),
        ];

        let installed_cli = search_paths.iter().find(|p| p.exists()).unwrap_or_else(|| {
            if had_existing {
                let _ = fs::rename(&backup_path, &default_install_path);
            }
            panic!(
                "Could not find installed CLI binary after install.sh. Checked: {:?}",
                search_paths
            )
        });

        // Atomic write: copy to a temp file in the same directory, then rename.
        // This prevents concurrent builds from reading a half-written binary.
        let tmp_path = bundled_dir.join(".claude.tmp");
        fs::copy(installed_cli, &tmp_path).unwrap_or_else(|e| {
            panic!(
                "Failed to copy CLI from {} to {}: {}",
                installed_cli.display(),
                tmp_path.display(),
                e
            )
        });

        set_executable(&tmp_path);

        fs::rename(&tmp_path, bundled_dir.join("claude")).unwrap_or_else(|e| {
            // rename failed (e.g., cross-device), fall back to copy
            let _ = fs::copy(&tmp_path, bundled_dir.join("claude"));
            let _ = fs::remove_file(&tmp_path);
            if !bundled_dir.join("claude").exists() {
                panic!(
                    "Failed to move CLI to final path {}: {}",
                    bundled_dir.join("claude").display(),
                    e
                );
            }
        });

        // Clean up temp file
        let _ = fs::remove_file(&tmp_path);

        // Restore the user's original CLI binary if we backed it up
        if had_existing {
            let _ = fs::rename(&backup_path, &default_install_path);
        }
        // Clean up backup file if it still exists
        let _ = fs::remove_file(&backup_path);
    }

    /// Download CLI on Windows using PowerShell.
    #[cfg(target_os = "windows")]
    fn download_windows(version: &str, bundled_dir: &Path) {
        // $ErrorActionPreference='Stop' ensures PowerShell aborts on any error,
        // preventing 'claude install' from running if irm/iex fails.
        let install_cmd = format!(
            "$ErrorActionPreference='Stop'; irm https://claude.ai/install.ps1 | iex; claude install '{}'",
            version
        );

        let status = Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &install_cmd])
            .status()
            .unwrap_or_else(|e| {
                panic!("Failed to execute PowerShell install script: {}", e)
            });

        if !status.success() {
            panic!(
                "Claude CLI install script failed with exit code: {:?}",
                status.code()
            );
        }

        // Find the installed binary
        let home = dirs_home();
        let possible_paths = [
            home.join("AppData\\Local\\Programs\\Claude\\claude.exe"),
            home.join("AppData\\Roaming\\npm\\claude.cmd"),
            home.join(".local\\bin\\claude.exe"),
        ];

        let installed_cli = possible_paths.iter().find(|p| p.exists()).unwrap_or_else(|| {
            panic!(
                "Could not find installed CLI binary. Checked: {:?}",
                possible_paths
            )
        });

        // Atomic write: copy to temp, then rename
        let tmp_path = bundled_dir.join(".claude.exe.tmp");
        fs::copy(installed_cli, &tmp_path).unwrap_or_else(|e| {
            panic!(
                "Failed to copy CLI from {} to {}: {}",
                installed_cli.display(),
                tmp_path.display(),
                e
            )
        });

        fs::rename(&tmp_path, bundled_dir.join("claude.exe")).unwrap_or_else(|e| {
            let _ = fs::copy(&tmp_path, bundled_dir.join("claude.exe"));
            let _ = fs::remove_file(&tmp_path);
            if !bundled_dir.join("claude.exe").exists() {
                panic!(
                    "Failed to move CLI to final path {}: {}",
                    bundled_dir.join("claude.exe").display(),
                    e
                );
            }
        });

        let _ = fs::remove_file(&tmp_path);
    }

    /// Set executable permission on Unix.
    #[cfg(unix)]
    fn set_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .unwrap_or_else(|e| panic!("Failed to read metadata for {}: {}", path.display(), e))
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)
            .unwrap_or_else(|e| panic!("Failed to set permissions on {}: {}", path.display(), e));
    }

    /// Get the bundled CLI directory: `~/.claude/sdk/bundled/{version}/`
    fn get_bundled_dir(version: &str) -> PathBuf {
        dirs_home().join(".claude/sdk/bundled").join(version)
    }

    /// Get the home directory, panicking if not available.
    fn dirs_home() -> PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .expect(
                "HOME or USERPROFILE environment variable must be set to download bundled CLI",
            );
        PathBuf::from(home)
    }

    /// Get the platform-appropriate CLI binary name.
    fn cli_name() -> &'static str {
        if cfg!(target_os = "windows") {
            "claude.exe"
        } else {
            "claude"
        }
    }

    /// Parse CLI_VERSION from `src/version.rs`.
    ///
    /// We parse the source file directly instead of depending on the crate,
    /// because build.rs runs before the crate is compiled.
    fn parse_cli_version() -> String {
        let version_rs = std::fs::read_to_string("src/version.rs")
            .expect("Failed to read src/version.rs");

        for line in version_rs.lines() {
            let trimmed = line.trim();
            // Match exactly: pub const CLI_VERSION: &str = "x.y.z";
            if trimmed.starts_with("pub const CLI_VERSION:") {
                if let Some(start) = trimmed.find('"') {
                    if let Some(end) = trimmed[start + 1..].find('"') {
                        let version = trimmed[start + 1..start + 1 + end].to_string();
                        // Validate: must be digits and dots only (semver x.y.z)
                        assert!(
                            !version.is_empty()
                                && version
                                    .chars()
                                    .all(|c| c.is_ascii_digit() || c == '.'),
                            "CLI_VERSION contains invalid characters: '{}'",
                            version
                        );
                        return version;
                    }
                }
            }
        }

        panic!("Could not parse CLI_VERSION from src/version.rs");
    }
}
