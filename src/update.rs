use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};

const REPO: &str = "namvox/simx";
const ASSET: &str = "simx-aarch64-apple-darwin.tar.gz";
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug, Serialize)]
pub struct UpdateHint {
    pub available: bool,
    pub current_version: String,
    pub latest_version: String,
    pub command: &'static str,
}

#[derive(Debug, Serialize)]
pub struct UpdateOutput {
    pub ok: bool,
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub installed: bool,
    pub installed_version: Option<String>,
    pub install_path: Option<String>,
    pub asset: Option<&'static str>,
    pub checksum_verified: Option<bool>,
}

#[derive(Debug)]
pub struct UpdateOptions {
    pub check: bool,
    pub version: Option<String>,
    pub install_dir: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
struct UpdateCache {
    checked_at: u64,
    latest_version: String,
}

pub fn maybe_update_hint() -> Option<UpdateHint> {
    let latest_version = latest_version_for_hint()?;
    if is_newer_version(&latest_version, current_version()) {
        Some(UpdateHint {
            available: true,
            current_version: current_version().to_string(),
            latest_version,
            command: "simx update",
        })
    } else {
        None
    }
}

pub fn run_update(options: UpdateOptions) -> Result<UpdateOutput> {
    validate_release_platform()?;
    let requested_version = options.version.is_some();
    let latest_version = match options.version {
        Some(version) => validate_version_tag(&version)?,
        None => fetch_latest_version(Duration::from_secs(15))?,
    };
    if !requested_version {
        write_cache(&latest_version).ok();
    }

    let update_available = is_newer_version(&latest_version, current_version());
    if options.check || (!update_available && !requested_version) {
        return Ok(UpdateOutput {
            ok: true,
            current_version: current_version().to_string(),
            latest_version,
            update_available,
            installed: false,
            installed_version: None,
            install_path: None,
            asset: None,
            checksum_verified: None,
        });
    }

    let install_path = resolve_install_path(options.install_dir.as_deref())?;
    let checksum_verified = install_release(&latest_version, &install_path)?;
    Ok(UpdateOutput {
        ok: true,
        current_version: current_version().to_string(),
        latest_version: latest_version.clone(),
        update_available,
        installed: true,
        installed_version: Some(latest_version),
        install_path: Some(install_path.display().to_string()),
        asset: Some(ASSET),
        checksum_verified: Some(checksum_verified),
    })
}

pub fn print_update_hint(hint: &UpdateHint) {
    eprintln!(
        "simx {} is available; current version is {}. Run `{}` to upgrade.",
        hint.latest_version, hint.current_version, hint.command
    );
}

fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn latest_version_for_hint() -> Option<String> {
    if let Some(cache) = read_cache().filter(cache_is_fresh) {
        return Some(cache.latest_version);
    }

    match fetch_latest_version(Duration::from_secs(3)) {
        Ok(version) => {
            write_cache(&version).ok();
            Some(version)
        }
        Err(_) => read_cache().map(|cache| cache.latest_version),
    }
}

fn cache_is_fresh(cache: &UpdateCache) -> bool {
    now_unix_seconds()
        .map(|now| now.saturating_sub(cache.checked_at) <= CACHE_TTL.as_secs())
        .unwrap_or(false)
}

fn read_cache() -> Option<UpdateCache> {
    let path = cache_path().ok()?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_cache(latest_version: &str) -> Result<()> {
    let path = cache_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let cache = UpdateCache {
        checked_at: now_unix_seconds()?,
        latest_version: latest_version.to_string(),
    };
    let json = serde_json::to_string_pretty(&cache)?;
    fs::write(&path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", path.display()))
}

fn cache_path() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("could not resolve home directory")?;
    Ok(base
        .home_dir()
        .join("Library/Application Support/simx/update.json"))
}

fn fetch_latest_version(timeout: Duration) -> Result<String> {
    let timeout = timeout.as_secs().to_string();
    let url = format!("https://github.com/{REPO}/releases/latest");
    let output = Command::new("curl")
        .args([
            "-fsSLI",
            "-o",
            "/dev/null",
            "-w",
            "%{url_effective}",
            "--max-time",
            &timeout,
            &url,
        ])
        .output()
        .context("failed to run curl")?;
    if !output.status.success() {
        anyhow::bail!(
            "failed to check latest simx release: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let url = String::from_utf8(output.stdout).context("curl output was not UTF-8")?;
    let tag = url
        .trim()
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .context("could not determine latest simx release tag")?;
    Ok(normalize_version_tag(tag))
}

fn normalize_version_tag(value: &str) -> String {
    value.trim().trim_start_matches('v').to_string()
}

fn validate_version_tag(value: &str) -> Result<String> {
    let normalized = normalize_version_tag(value);
    Version::parse(&normalized)
        .with_context(|| format!("release version must be a semver tag like v0.1.2: {value}"))?;
    Ok(normalized)
}

fn is_newer_version(latest: &str, current: &str) -> bool {
    let Ok(latest) = Version::parse(&normalize_version_tag(latest)) else {
        return false;
    };
    let Ok(current) = Version::parse(&normalize_version_tag(current)) else {
        return false;
    };
    latest > current
}

fn validate_release_platform() -> Result<()> {
    let os = command_output("uname", &["-s"])?;
    if os.trim() != "Darwin" {
        anyhow::bail!(
            "unsupported OS {}; simx release binaries currently support macOS Apple Silicon only",
            os.trim()
        );
    }

    let arch = command_output("uname", &["-m"])?;
    if arch.trim() != "arm64" {
        anyhow::bail!(
            "unsupported architecture {}; simx release binaries currently support aarch64-apple-darwin only",
            arch.trim()
        );
    }
    Ok(())
}

fn resolve_install_path(install_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(install_dir) = install_dir {
        fs::create_dir_all(install_dir)
            .with_context(|| format!("failed to create {}", install_dir.display()))?;
        ensure_writable_dir(install_dir)?;
        return Ok(install_dir.join("simx"));
    }

    let current_exe = std::env::current_exe().context("failed to locate current simx binary")?;
    let install_dir = current_exe
        .parent()
        .context("current simx binary path has no parent")?;
    ensure_writable_dir(install_dir)?;
    Ok(current_exe)
}

fn ensure_writable_dir(path: &Path) -> Result<()> {
    if !path.exists() || !path.is_dir() {
        anyhow::bail!("install directory does not exist: {}", path.display());
    }
    let probe = path.join(format!(".simx-write-test-{}", std::process::id()));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            fs::remove_file(&probe).ok();
            Ok(())
        }
        Err(error) => anyhow::bail!(
            "install directory is not writable: {}. Pass --install-dir with a writable bin directory. ({error})",
            path.display()
        ),
    }
}

fn install_release(version: &str, install_path: &Path) -> Result<bool> {
    let temp_dir = TempDirGuard::new()?;
    let asset_path = temp_dir.path().join(ASSET);
    let checksum_path = temp_dir.path().join("checksums.txt");
    let tag = format!("v{}", normalize_version_tag(version));
    let base_url = format!("https://github.com/{REPO}/releases/download/{tag}");

    download(
        &format!("{base_url}/{ASSET}"),
        &asset_path,
        Duration::from_secs(120),
    )?;
    let checksum_verified = match download(
        &format!("{base_url}/checksums.txt"),
        &checksum_path,
        Duration::from_secs(30),
    ) {
        Ok(()) => {
            verify_checksum(&checksum_path, &asset_path)?;
            true
        }
        Err(_) => false,
    };

    extract_archive(&asset_path, temp_dir.path())?;
    let extracted = temp_dir.path().join("simx");
    if !extracted.exists() {
        anyhow::bail!("release archive did not contain a simx binary");
    }
    let metadata = fs::metadata(&extracted).context("failed to inspect extracted simx binary")?;
    if !metadata.is_file() {
        anyhow::bail!("release archive simx entry was not a file");
    }
    let extracted_version = binary_version(&extracted)?;
    if extracted_version != normalize_version_tag(version) {
        anyhow::bail!(
            "release archive version mismatch: requested {}, but bundled simx reports {}",
            normalize_version_tag(version),
            extracted_version
        );
    }
    install_binary_atomically(&extracted, install_path)?;
    Ok(checksum_verified)
}

fn install_binary_atomically(extracted: &Path, install_path: &Path) -> Result<()> {
    let install_dir = install_path
        .parent()
        .context("install path has no parent directory")?;
    let staged = install_dir.join(format!(".simx-update-{}", std::process::id()));
    fs::copy(extracted, &staged)
        .with_context(|| format!("failed to stage simx at {}", staged.display()))?;
    let mut permissions = fs::metadata(&staged)
        .with_context(|| format!("failed to inspect {}", staged.display()))?
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
        fs::set_permissions(&staged, permissions)
            .with_context(|| format!("failed to chmod {}", staged.display()))?;
    }
    if let Err(error) = fs::rename(&staged, install_path) {
        fs::remove_file(&staged).ok();
        return Err(error)
            .with_context(|| format!("failed to install simx to {}", install_path.display()));
    }
    Ok(())
}

fn binary_version(path: &Path) -> Result<String> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .with_context(|| format!("failed to run {}", path.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "{} --version failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let raw = String::from_utf8(output.stdout).context("simx --version output was not UTF-8")?;
    raw.split_whitespace()
        .last()
        .map(normalize_version_tag)
        .context("could not parse simx --version output")
}

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "simx-update-{}-{}",
            std::process::id(),
            now_unix_seconds()?
        ));
        fs::create_dir(&path).with_context(|| format!("failed to create {}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).ok();
    }
}

fn download(url: &str, output_path: &Path, timeout: Duration) -> Result<()> {
    let output = Command::new("curl")
        .arg("-fsSL")
        .arg("--max-time")
        .arg(timeout.as_secs().to_string())
        .arg(url)
        .arg("-o")
        .arg(output_path)
        .output()
        .context("failed to run curl")?;
    if output.status.success() {
        return Ok(());
    }
    anyhow::bail!(
        "failed to download {url}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn verify_checksum(checksum_path: &Path, asset_path: &Path) -> Result<()> {
    let raw = fs::read_to_string(checksum_path)
        .with_context(|| format!("failed to read {}", checksum_path.display()))?;
    let line = raw
        .lines()
        .find(|line| line.ends_with(ASSET))
        .context("checksums.txt did not contain the simx release asset")?;
    let asset_name = asset_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("asset path had no UTF-8 file name")?;
    let rewritten = line.replace(ASSET, asset_name);
    let mut child = Command::new("shasum")
        .arg("-a")
        .arg("256")
        .arg("-c")
        .arg("-")
        .current_dir(
            asset_path
                .parent()
                .context("asset path had no parent directory")?,
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run shasum")?;
    let mut stdin = child.stdin.take().context("failed to open shasum stdin")?;
    stdin
        .write_all(rewritten.as_bytes())
        .context("failed to write checksum input")?;
    stdin
        .write_all(b"\n")
        .context("failed to finish checksum input")?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .context("failed to wait for shasum")?;
    if output.status.success() {
        return Ok(());
    }
    anyhow::bail!(
        "release checksum verification failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn extract_archive(asset_path: &Path, temp_dir: &Path) -> Result<()> {
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(asset_path)
        .arg("-C")
        .arg(temp_dir)
        .output()
        .context("failed to run tar")?;
    if output.status.success() {
        return Ok(());
    }
    anyhow::bail!(
        "failed to extract release archive: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn command_output(command: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {command}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).with_context(|| format!("{command} output was not UTF-8"))
}

fn now_unix_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_version_compares_semver_tags() {
        assert!(is_newer_version("v0.1.1", "0.1.0"));
        assert!(!is_newer_version("0.1.0", "0.1.0"));
        assert!(!is_newer_version("0.1.0", "0.1.1"));
    }

    #[test]
    fn version_tag_normalization_removes_prefix() {
        assert_eq!(normalize_version_tag("v0.2.0"), "0.2.0");
        assert_eq!(normalize_version_tag("0.2.0"), "0.2.0");
    }

    #[test]
    fn version_tag_validation_rejects_non_semver_values() {
        assert_eq!(validate_version_tag("v0.2.0").unwrap(), "0.2.0");
        assert!(validate_version_tag("latest").is_err());
    }
}
