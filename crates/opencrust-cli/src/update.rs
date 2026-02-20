use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

const GITHUB_REPO: &str = "opencrust-org/opencrust";
const RELEASES_API: &str = "https://api.github.com/repos/opencrust-org/opencrust/releases/latest";
const UPDATE_CHECK_FILE: &str = "update-check.json";
const CHECK_TTL_SECS: u64 = 86400; // 24 hours

/// Cached update check result.
#[derive(serde::Serialize, serde::Deserialize)]
struct UpdateCheck {
    latest_version: String,
    release_notes: String,
    checked_at: u64,
}

/// GitHub release API response (only the fields we need).
#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
    body: Option<String>,
    assets: Vec<GitHubAsset>,
}

#[derive(serde::Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn cache_path() -> PathBuf {
    opencrust_config::ConfigLoader::default_config_dir().join(UPDATE_CHECK_FILE)
}

/// Return the asset name for the current platform.
fn platform_asset_name() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("opencrust-macos-aarch64"),
        ("macos", "x86_64") => Ok("opencrust-macos-x86_64"),
        ("linux", "x86_64") => Ok("opencrust-linux-x86_64"),
        ("linux", "aarch64") => Ok("opencrust-linux-aarch64"),
        ("windows", "x86_64") => Ok("opencrust-windows-x86_64.exe"),
        (os, arch) => bail!("unsupported platform: {os}/{arch}"),
    }
}

/// Strip leading 'v' from a version tag.
fn strip_v(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Fetch the latest release info from GitHub.
async fn fetch_latest_release(client: &reqwest::Client) -> Result<GitHubRelease> {
    let resp = client
        .get(RELEASES_API)
        .header("user-agent", format!("opencrust/{}", current_version()))
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .context("failed to reach GitHub releases API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("GitHub API returned {status}: {body}");
    }

    resp.json::<GitHubRelease>()
        .await
        .context("failed to parse GitHub release response")
}

/// Run `opencrust update`. Returns Ok(true) if an update was applied.
pub async fn run_update(yes: bool) -> Result<bool> {
    let client = reqwest::Client::new();
    let release = fetch_latest_release(&client).await?;

    let latest = strip_v(&release.tag_name);
    let current = current_version();

    if latest == current {
        println!("Already up to date (v{current}).");
        save_check_cache(latest, release.body.as_deref().unwrap_or(""));
        return Ok(false);
    }

    println!("Current version:   v{current}");
    println!("Latest version:    v{latest}");
    println!();

    // Show release notes (truncated)
    if let Some(notes) = &release.body {
        let preview: String = notes.lines().take(20).collect::<Vec<_>>().join("\n");
        println!("Release notes:");
        println!("{preview}");
        if notes.lines().count() > 20 {
            println!(
                "  ... (truncated, see https://github.com/{GITHUB_REPO}/releases/tag/{})",
                release.tag_name
            );
        }
        println!();
    }

    // Confirm unless --yes
    if !yes {
        let confirm = dialoguer::Confirm::new()
            .with_prompt(format!("Update to v{latest}?"))
            .default(true)
            .interact()
            .context("failed to read confirmation")?;

        if !confirm {
            println!("Update cancelled.");
            return Ok(false);
        }
    }

    // Find the right asset
    let asset_name = platform_asset_name()?;
    let checksum_name = format!("{asset_name}.sha256");

    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .context(format!(
            "release has no asset for this platform ({asset_name})"
        ))?;

    let checksum_asset = release.assets.iter().find(|a| a.name == checksum_name);

    // Download binary
    println!("Downloading {asset_name}...");
    let binary_bytes = client
        .get(&asset.browser_download_url)
        .header("user-agent", format!("opencrust/{current}"))
        .send()
        .await
        .context("failed to download binary")?
        .bytes()
        .await
        .context("failed to read binary bytes")?;

    // Verify checksum if available
    if let Some(cs_asset) = checksum_asset {
        print!("Verifying checksum...");
        let cs_text = client
            .get(&cs_asset.browser_download_url)
            .header("user-agent", format!("opencrust/{current}"))
            .send()
            .await
            .context("failed to download checksum")?
            .text()
            .await
            .context("failed to read checksum")?;

        let expected_hash = cs_text
            .split_whitespace()
            .next()
            .context("invalid checksum file format")?
            .to_lowercase();

        use std::fmt::Write;
        let digest = sha256_digest(&binary_bytes);
        let mut actual_hash = String::with_capacity(64);
        for byte in &digest {
            write!(&mut actual_hash, "{byte:02x}").unwrap();
        }

        if actual_hash != expected_hash {
            bail!(
                "checksum mismatch!\n  expected: {expected_hash}\n  got:      {actual_hash}\n\nDownload may be corrupted. Update aborted."
            );
        }
        println!(" ok");
    }

    // Locate current binary
    let current_exe =
        std::env::current_exe().context("cannot determine current executable path")?;
    let backup_path = current_exe.with_extension("old");

    // Write new binary to temp file
    let temp_path = current_exe.with_extension("new");
    fs::write(&temp_path, &binary_bytes).context("failed to write new binary")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o755))
            .context("failed to set executable permissions")?;

        // Backup current binary
        if current_exe.exists() {
            fs::copy(&current_exe, &backup_path).context("failed to backup current binary")?;
            println!("Backed up current binary to {}", backup_path.display());
        }

        // Atomic replace
        fs::rename(&temp_path, &current_exe).context("failed to replace binary")?;
    }

    #[cfg(windows)]
    {
        // Windows replacement strategy:
        // 1. Remove old backup if exists (rename target must not exist)
        if backup_path.exists() {
            let _ = fs::remove_file(&backup_path);
        }

        // 2. Rename current to backup (this works even if running)
        if current_exe.exists() {
            fs::rename(&current_exe, &backup_path)
                .context("failed to move current binary to backup")?;
            println!("Backed up current binary to {}", backup_path.display());
        }

        // 3. Rename new to current
        if let Err(e) = fs::rename(&temp_path, &current_exe) {
            // Rollback attempt if rename fails
            let _ = fs::rename(&backup_path, &current_exe);
            return Err(e).context("failed to replace binary");
        }
    }

    // Update cache
    save_check_cache(latest, release.body.as_deref().unwrap_or(""));

    println!();
    println!("Updated to v{latest}.");
    println!("Restart the daemon to apply: opencrust restart");

    Ok(true)
}

/// Run `opencrust rollback`.
pub fn run_rollback() -> Result<()> {
    let current_exe =
        std::env::current_exe().context("cannot determine current executable path")?;
    let backup_path = current_exe.with_extension("old");

    if !backup_path.exists() {
        bail!(
            "no backup found at {}. Nothing to roll back to.",
            backup_path.display()
        );
    }

    fs::rename(&backup_path, &current_exe).context("failed to restore backup")?;

    println!("Rolled back to previous version.");
    println!("Restart the daemon to apply: opencrust restart");

    Ok(())
}

/// Check for updates (non-blocking, cached). Returns a message if an update
/// is available, or None.
pub fn check_for_update_notice() -> Option<String> {
    if std::env::var("OPENCRUST_NO_UPDATE_CHECK").is_ok() {
        return None;
    }

    let cache = read_check_cache()?;
    let current = current_version();
    let latest = strip_v(&cache.latest_version);

    if latest != current {
        Some(format!(
            "Update available: v{current} -> v{latest}  —  run `opencrust update`"
        ))
    } else {
        None
    }
}

/// Spawn a background version check that updates the cache file.
/// Does not block. Failures are silently ignored.
pub fn spawn_background_check() {
    if std::env::var("OPENCRUST_NO_UPDATE_CHECK").is_ok() {
        return;
    }

    // Skip if cache is fresh
    if let Some(cache) = read_check_cache() {
        let now = epoch_secs();
        if now.saturating_sub(cache.checked_at) < CHECK_TTL_SECS {
            return;
        }
    }

    tokio::spawn(async {
        let client = reqwest::Client::new();
        if let Ok(release) = fetch_latest_release(&client).await {
            let version = strip_v(&release.tag_name);
            save_check_cache(version, release.body.as_deref().unwrap_or(""));
        }
    });
}

fn read_check_cache() -> Option<UpdateCheck> {
    let path = cache_path();
    let contents = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn save_check_cache(version: &str, notes: &str) {
    let check = UpdateCheck {
        latest_version: version.to_string(),
        release_notes: notes.to_string(),
        checked_at: epoch_secs(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&check) {
        let _ = fs::write(cache_path(), json);
    }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Simple SHA-256 using ring.
fn sha256_digest(data: &[u8]) -> [u8; 32] {
    use ring::digest;
    let d = digest::digest(&digest::SHA256, data);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}
