use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::pool::PoolService;

const TARGET_NAME: &str = "PreviewHost";
const PLUGIN_TARGET_NAME: &str = "PreviewReloadPlugin";
const BUNDLE_ID: &str = "dev.simx.preview.host";
const RELOAD_NOTIFICATION: &str = "dev.swiftui-preview-browser.reload";
const RELOAD_DIR_NAME: &str = "swiftui-preview-browser";

#[derive(Debug)]
pub struct PreviewOptions {
    pub slug: String,
    pub udid: String,
    pub state_path: PathBuf,
    pub package_swift: PathBuf,
    pub package_target: String,
    pub preview_filters: Vec<String>,
    pub watch: bool,
    pub json: bool,
}

#[derive(Debug, Serialize)]
pub struct PreviewReadyOutput {
    pub slug: String,
    pub udid: String,
    pub package: String,
    pub package_target: String,
    pub package_product: String,
    pub package_module: String,
    pub preview_filters: Option<Vec<String>>,
    pub project: String,
    pub build_root: String,
    pub host_app: String,
    pub host_bundle_id: &'static str,
    pub pid: Option<i32>,
    pub watch: bool,
}

#[derive(Debug, Clone)]
struct PreviewConfig {
    package_root: PathBuf,
    package_target: String,
    package_product: String,
    package_module: String,
    deployment_target: String,
    preview_filters: Vec<String>,
}

#[derive(Debug)]
struct PreviewState {
    options: PreviewOptions,
    config: PreviewConfig,
    project_root: PathBuf,
    build_root: PathBuf,
    data_container: Option<PathBuf>,
    app_pid: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct PackageDump {
    products: Vec<PackageProduct>,
    targets: Vec<PackageTarget>,
    #[serde(default)]
    platforms: Vec<PackagePlatform>,
}

#[derive(Debug, Deserialize)]
struct PackageProduct {
    name: String,
    #[serde(default)]
    targets: Vec<String>,
    #[serde(rename = "type")]
    product_type: Value,
}

#[derive(Debug, Deserialize)]
struct PackageTarget {
    name: String,
    #[serde(rename = "type")]
    target_type: String,
}

#[derive(Debug, Deserialize)]
struct PackagePlatform {
    #[serde(rename = "platformName")]
    platform_name: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct PackageDescription {
    targets: Vec<PackageDescriptionTarget>,
}

#[derive(Debug, Deserialize)]
struct PackageDescriptionTarget {
    name: String,
    c99name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HostStatus {
    pid: i32,
    phase: String,
    #[serde(rename = "lastToken")]
    last_token: Option<String>,
    #[serde(rename = "lastError")]
    last_error: Option<String>,
}

pub fn run_preview(options: PreviewOptions) -> anyhow::Result<()> {
    let package_swift = resolve_package_swift(&options.package_swift)?;
    let config = resolve_preview_config(
        &package_swift,
        &options.package_target,
        &options.preview_filters,
    )?;
    let scratch_root = default_scratch_root(&package_swift, &config);
    let project_root = scratch_root.join("GeneratedPreviewHost");
    let build_root = scratch_root.join("build");
    let mut state = PreviewState {
        options,
        config,
        project_root,
        build_root,
        data_container: None,
        app_pid: None,
    };

    log(&format!("Package.swift: {}", package_swift.display()));
    log(&format!("package: {}", state.config.package_root.display()));
    log(&format!("product: {}", state.config.package_product));
    log(&format!("module: {}", state.config.package_module));
    if !state.config.preview_filters.is_empty() {
        log(&format!(
            "preview filters: {}",
            state.config.preview_filters.join(", ")
        ));
    }
    log(&format!("simulator: {}", state.options.udid));

    bootstatus(&state.options.udid)?;
    build_and_launch(&mut state)?;

    let ready = PreviewReadyOutput {
        slug: state.options.slug.clone(),
        udid: state.options.udid.clone(),
        package: state.config.package_root.display().to_string(),
        package_target: state.config.package_target.clone(),
        package_product: state.config.package_product.clone(),
        package_module: state.config.package_module.clone(),
        preview_filters: if state.config.preview_filters.is_empty() {
            None
        } else {
            Some(state.config.preview_filters.clone())
        },
        project: state.project_root.display().to_string(),
        build_root: state.build_root.display().to_string(),
        host_app: host_app_path(&state).display().to_string(),
        host_bundle_id: BUNDLE_ID,
        pid: state.app_pid,
        watch: state.options.watch,
    };

    if state.options.json {
        println!("{}", serde_json::to_string_pretty(&ready)?);
    } else {
        println!("swiftui preview ready on simulator {}", state.options.udid);
        println!("host: {}", ready.host_app);
        if state.options.watch {
            println!(
                "watching {} for SwiftUI preview changes",
                state.config.package_root.display()
            );
        }
    }

    if state.options.watch {
        watch_package_tree(&mut state)?;
    }
    Ok(())
}

fn resolve_package_swift(path: &Path) -> anyhow::Result<PathBuf> {
    let resolved = fs::canonicalize(path)
        .with_context(|| format!("Package.swift not found: {}", path.display()))?;
    if resolved.file_name() != Some(OsStr::new("Package.swift")) {
        bail!(
            "package path must point to Package.swift: {}",
            resolved.display()
        );
    }
    Ok(resolved)
}

fn resolve_preview_config(
    package_swift: &Path,
    requested_target: &str,
    preview_filters: &[String],
) -> anyhow::Result<PreviewConfig> {
    let package_root = package_swift
        .parent()
        .context("Package.swift has no parent directory")?
        .to_path_buf();
    let dump: PackageDump = run_json(
        "swift",
        ["package", "dump-package", "--package-path"]
            .into_iter()
            .map(String::from)
            .chain([package_root.display().to_string()]),
    )?;
    let package_target = resolve_package_target(requested_target, &dump)?;
    let package_product = infer_package_product(&package_target, &dump)?;
    let description: PackageDescription = run_json(
        "swift",
        ["package", "--package-path"]
            .into_iter()
            .map(String::from)
            .chain([
                package_root.display().to_string(),
                "describe".to_string(),
                "--type".to_string(),
                "json".to_string(),
            ]),
    )?;
    let package_module = resolve_package_module(&package_target, &description)?;
    let deployment_target = infer_deployment_target(&dump);

    Ok(PreviewConfig {
        package_root,
        package_target,
        package_product,
        package_module,
        deployment_target,
        preview_filters: preview_filters.to_vec(),
    })
}

fn resolve_package_target(requested_target: &str, dump: &PackageDump) -> anyhow::Result<String> {
    if dump
        .targets
        .iter()
        .any(|target| target.name == requested_target && target.target_type == "regular")
    {
        return Ok(requested_target.to_string());
    }
    bail!("Swift package does not contain a regular target named \"{requested_target}\"")
}

fn resolve_package_module(
    package_target: &str,
    description: &PackageDescription,
) -> anyhow::Result<String> {
    description
        .targets
        .iter()
        .find(|target| target.name == package_target)
        .and_then(|target| target.c99name.clone())
        .with_context(|| {
            format!("Swift package target \"{package_target}\" does not expose an importable module name")
        })
}

fn infer_package_product(package_target: &str, dump: &PackageDump) -> anyhow::Result<String> {
    let mut products = dump
        .products
        .iter()
        .filter(|product| {
            is_library_product(product)
                && product
                    .targets
                    .iter()
                    .any(|target| target == package_target)
        })
        .collect::<Vec<_>>();
    let has_dynamic_only = products
        .iter()
        .any(|product| is_dynamic_library_product(product));
    products.retain(|product| !is_dynamic_library_product(product));

    if let Some(product) = products
        .iter()
        .find(|product| product.name == package_target)
    {
        return Ok(product.name.clone());
    }
    products.sort_by(|left, right| {
        left.targets
            .len()
            .cmp(&right.targets.len())
            .then_with(|| left.name.cmp(&right.name))
    });
    if let Some(product) = products.first() {
        return Ok(product.name.clone());
    }
    if has_dynamic_only {
        bail!(
            "Swift package target \"{package_target}\" is only exported by dynamic library products. Dynamic library products are not supported because hot reload replaces the generated preview plugin dylib."
        );
    }
    bail!("Swift package target \"{package_target}\" is not exported by a library product")
}

fn is_library_product(product: &PackageProduct) -> bool {
    product.product_type.get("library").is_some()
}

fn is_dynamic_library_product(product: &PackageProduct) -> bool {
    product
        .product_type
        .get("library")
        .and_then(Value::as_array)
        .map(|values| values.iter().any(|value| value.as_str() == Some("dynamic")))
        .unwrap_or(false)
}

fn infer_deployment_target(dump: &PackageDump) -> String {
    let version = dump
        .platforms
        .iter()
        .find(|platform| platform.platform_name == "ios")
        .map(|platform| platform.version.as_str())
        .unwrap_or("17.0");
    if version.parse::<f32>().unwrap_or(17.0) < 17.0 {
        "17.0".to_string()
    } else {
        version.to_string()
    }
}

fn default_scratch_root(package_swift: &Path, config: &PreviewConfig) -> PathBuf {
    let identity = format!(
        "{}\n{}\n{}",
        package_swift.display(),
        config.package_module,
        config.preview_filters.join(",")
    );
    let hash = Sha1::digest(identity.as_bytes());
    let scratch_id = format!("{hash:x}").chars().take(12).collect::<String>();
    std::env::temp_dir()
        .join("simx-swiftui-preview")
        .join(scratch_id)
}

fn build_and_launch(state: &mut PreviewState) -> anyhow::Result<()> {
    fs::remove_dir_all(&state.project_root).ok();
    fs::create_dir_all(state.project_root.join(format!("{TARGET_NAME}.xcodeproj")))?;
    fs::create_dir_all(state.project_root.join(TARGET_NAME))?;

    write_templates(state)?;
    run_xcodebuild(
        &state.project_root,
        &[
            "-project",
            &format!("{TARGET_NAME}.xcodeproj"),
            "-scheme",
            TARGET_NAME,
            "-configuration",
            "Debug",
            "-destination",
            &format!("id={}", state.options.udid),
            "-sdk",
            "iphonesimulator",
            "-derivedDataPath",
            state
                .build_root
                .to_str()
                .context("build root is not UTF-8")?,
            "CODE_SIGNING_ALLOWED=NO",
            "build",
        ],
        &state.build_root.join("logs").join("preview-host.log"),
    )?;
    log(&format!(
        "built preview host; build log: {}",
        state
            .build_root
            .join("logs")
            .join("preview-host.log")
            .display()
    ));

    let app = host_app_path(state);
    if !app.exists() {
        bail!(
            "Build succeeded but app bundle was not found: {}",
            app.display()
        );
    }

    run_capture(
        "/usr/bin/xcrun",
        &["simctl", "terminate", &state.options.udid, BUNDLE_ID],
    )
    .ok();
    run_checked(
        "/usr/bin/xcrun",
        &["simctl", "install", &state.options.udid, path_str(&app)?],
    )?;
    let data_container = app_data_container(&state.options.udid)?;
    fs::remove_dir_all(data_container.join("Documents").join(RELOAD_DIR_NAME)).ok();
    let launch = run_capture(
        "/usr/bin/xcrun",
        &["simctl", "launch", &state.options.udid, BUNDLE_ID],
    )?;
    state.app_pid = launch_pid(&launch);
    state.data_container = Some(data_container);
    wait_for_host_ready(state)?;
    log(&format!(
        "launched package preview host in pid {:?}",
        state.app_pid
    ));
    Ok(())
}

fn host_app_path(state: &PreviewState) -> PathBuf {
    state
        .build_root
        .join("Build/Products/Debug-iphonesimulator")
        .join(format!("{TARGET_NAME}.app"))
}

fn write_templates(state: &PreviewState) -> anyhow::Result<()> {
    let source_dir = state.project_root.join(TARGET_NAME);
    fs::write(
        source_dir.join("FocusedPreviewApp.swift"),
        FOCUSED_PREVIEW_APP,
    )?;
    fs::write(
        source_dir.join("FocusedPreviewHotReloadRuntime.swift"),
        HOT_RELOAD_RUNTIME,
    )?;
    fs::write(
        source_dir.join("PreviewBrowserEntries.swift"),
        PREVIEW_BROWSER_ENTRIES,
    )?;
    fs::write(
        source_dir.join("PreviewBrowserConfiguration.swift"),
        generated_configuration_source(&state.config),
    )?;
    let package_relative_path = pathdiff(&state.config.package_root, &state.project_root)
        .unwrap_or_else(|| state.config.package_root.clone());
    fs::write(
        state
            .project_root
            .join(format!("{TARGET_NAME}.xcodeproj/project.pbxproj")),
        create_project_file(
            &package_relative_path.display().to_string(),
            &state.config.package_product,
            &state.config.deployment_target,
        ),
    )?;
    Ok(())
}

fn hot_reload(state: &mut PreviewState) -> anyhow::Result<()> {
    let expected_pid = read_host_status(state)?
        .map(|status| status.pid)
        .or(state.app_pid);
    let token = reload_token()?;
    let log_path = state.build_root.join("logs").join("hot-reload.log");
    run_xcodebuild(
        &state.project_root,
        &[
            "-project",
            &format!("{TARGET_NAME}.xcodeproj"),
            "-scheme",
            PLUGIN_TARGET_NAME,
            "-configuration",
            "Debug",
            "-destination",
            &format!("id={}", state.options.udid),
            "-sdk",
            "iphonesimulator",
            "-derivedDataPath",
            state
                .build_root
                .to_str()
                .context("build root is not UTF-8")?,
            "CODE_SIGNING_ALLOWED=NO",
            "build",
        ],
        &log_path,
    )?;
    log(&format!(
        "built hot reload plugin; build log: {}",
        log_path.display()
    ));

    let dylib = state
        .build_root
        .join("Build/Products/Debug-iphonesimulator")
        .join(format!("lib{PLUGIN_TARGET_NAME}.dylib"));
    if !dylib.exists() {
        bail!(
            "Package hot reload build succeeded but dylib was not found: {}",
            dylib.display()
        );
    }
    let reload_dir = state
        .data_container
        .as_ref()
        .context("host data container was not resolved")?
        .join("Documents")
        .join(RELOAD_DIR_NAME);
    fs::create_dir_all(&reload_dir)?;
    let container_dylib = reload_dir.join(format!("lib{PLUGIN_TARGET_NAME}-{token}.dylib"));
    fs::copy(&dylib, &container_dylib)
        .with_context(|| format!("failed to copy {}", dylib.display()))?;
    fs::write(
        reload_dir.join("reload.json"),
        serde_json::json!({ "token": token, "dylibPath": container_dylib }).to_string(),
    )?;
    run_checked(
        "/usr/bin/xcrun",
        &[
            "simctl",
            "spawn",
            &state.options.udid,
            "notifyutil",
            "-p",
            RELOAD_NOTIFICATION,
        ],
    )?;
    let status = wait_for_host_reload(state, &token)?;
    if let Some(expected_pid) = expected_pid {
        if status.pid != expected_pid {
            bail!(
                "Hot reload changed PID from {expected_pid} to {}",
                status.pid
            );
        }
    }
    state.app_pid = Some(status.pid);
    log(&format!(
        "hot reloaded package preview {} in pid {}",
        state.config.package_module, status.pid
    ));
    Ok(())
}

fn watch_package_tree(state: &mut PreviewState) -> anyhow::Result<()> {
    let mut snapshot = source_snapshot(&state.config.package_root)?;
    loop {
        thread::sleep(Duration::from_millis(500));
        if !lease_still_active(state)? {
            log(&format!(
                "lease {} no longer owns simulator {}; stopping preview watcher",
                state.options.slug, state.options.udid
            ));
            return Ok(());
        }
        let next = source_snapshot(&state.config.package_root)?;
        if next != snapshot {
            snapshot = next;
            log("change detected; hot reloading");
            if let Err(error) = hot_reload(state) {
                eprintln!("{error:#}");
            }
        }
    }
}

fn lease_still_active(state: &PreviewState) -> anyhow::Result<bool> {
    let mut service = PoolService::new(state.options.state_path.clone());
    service.active_lease_matches_udid(&state.options.slug, &state.options.udid)
}

fn source_snapshot(root: &Path) -> anyhow::Result<HashSet<(PathBuf, u64, u64)>> {
    let mut entries = HashSet::new();
    collect_source_snapshot(root, root, &mut entries)?;
    Ok(entries)
}

fn collect_source_snapshot(
    root: &Path,
    dir: &Path,
    entries: &mut HashSet<(PathBuf, u64, u64)>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(name.as_ref(), ".build" | ".git" | ".swiftpm") {
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_source_snapshot(root, &path, entries)?;
        } else if path.extension().and_then(OsStr::to_str) == Some("swift")
            || path.file_name() == Some(OsStr::new("Package.swift"))
        {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0);
            entries.insert((
                path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
                metadata.len(),
                modified,
            ));
        }
    }
    Ok(())
}

fn bootstatus(udid: &str) -> anyhow::Result<()> {
    run_checked("/usr/bin/xcrun", &["simctl", "bootstatus", udid, "-b"])
}

fn app_data_container(udid: &str) -> anyhow::Result<PathBuf> {
    let output = run_capture(
        "/usr/bin/xcrun",
        &["simctl", "get_app_container", udid, BUNDLE_ID, "data"],
    )?;
    Ok(PathBuf::from(output.trim()))
}

fn wait_for_host_ready(state: &PreviewState) -> anyhow::Result<HostStatus> {
    wait_for_host_status(state, 50, |status| {
        state.app_pid.is_none() || (status.pid == state.app_pid.unwrap() && status.phase == "running")
    })
    .context("Preview host launched but did not render. The selected preview may not be self-contained; inspect Simulator logs.")
}

fn wait_for_host_reload(state: &PreviewState, token: &str) -> anyhow::Result<HostStatus> {
    for _ in 0..80 {
        thread::sleep(Duration::from_millis(100));
        if let Some(status) = read_host_status(state)? {
            if status.last_token.as_deref() == Some(token) && status.phase == "error" {
                bail!(
                    "Hot reload failed inside host: {}",
                    status
                        .last_error
                        .unwrap_or_else(|| "unknown error".to_string())
                );
            }
            if status.last_token.as_deref() == Some(token) && status.phase == "reloaded" {
                return Ok(status);
            }
        }
    }
    bail!("Timed out waiting for the running host app to apply the hot reload")
}

fn wait_for_host_status<F>(
    state: &PreviewState,
    attempts: usize,
    is_ready: F,
) -> anyhow::Result<HostStatus>
where
    F: Fn(&HostStatus) -> bool,
{
    for _ in 0..attempts {
        thread::sleep(Duration::from_millis(100));
        if let Some(status) = read_host_status(state)? {
            if is_ready(&status) {
                return Ok(status);
            }
        }
    }
    bail!("Timed out waiting for preview host status")
}

fn read_host_status(state: &PreviewState) -> anyhow::Result<Option<HostStatus>> {
    let Some(container) = &state.data_container else {
        return Ok(None);
    };
    let path = container
        .join("Documents")
        .join(RELOAD_DIR_NAME)
        .join("status.json");
    match fs::read_to_string(&path) {
        Ok(contents) => Ok(Some(serde_json::from_str(&contents)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn run_xcodebuild(project_root: &Path, args: &[&str], log_path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let log = File::create(log_path)
        .with_context(|| format!("failed to create {}", log_path.display()))?;
    let log_err = log.try_clone()?;
    let status = Command::new("/usr/bin/xcodebuild")
        .args(args)
        .current_dir(project_root)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .status()
        .context("failed to run xcodebuild")?;
    if status.success() {
        return Ok(());
    }
    bail!(
        "xcodebuild failed with {status}; log: {}\n{}",
        log_path.display(),
        log_tail(log_path, 30).unwrap_or_default()
    )
}

fn run_json<T, I>(command: &str, args: I) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned,
    I: IntoIterator<Item = String>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let output = Command::new(command)
        .args(&args)
        .output()
        .with_context(|| format!("failed to run {command}"))?;
    if !output.status.success() {
        bail!(
            "{} {} failed: {}",
            command,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("failed to parse {command} JSON output"))
}

fn run_checked(command: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new(command)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {command}"))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "{} {} failed: {}",
        command,
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn run_capture(command: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {command}"))?;
    if !output.status.success() {
        bail!(
            "{} {} failed: {}",
            command,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("command output was not UTF-8")
}

fn log_tail(path: &Path, max_lines: usize) -> anyhow::Result<String> {
    let file = File::open(path)?;
    let lines = BufReader::new(file)
        .lines()
        .collect::<Result<Vec<_>, _>>()?;
    Ok(lines
        .iter()
        .skip(lines.len().saturating_sub(max_lines))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n"))
}

fn launch_pid(output: &str) -> Option<i32> {
    output
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{BUNDLE_ID}: ")))
        .and_then(|pid| pid.trim().parse().ok())
}

fn reload_token() -> anyhow::Result<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
    Ok(format!(
        "{}-{}-{}",
        std::process::id(),
        now.as_secs(),
        now.subsec_nanos()
    ))
}

fn path_str(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not UTF-8: {}", path.display()))
}

fn log(message: &str) {
    eprintln!("[simx preview] {message}");
}

fn pathdiff(path: &Path, base: &Path) -> Option<PathBuf> {
    pathdiff::diff_paths(path, base)
}

fn generated_configuration_source(config: &PreviewConfig) -> String {
    format!(
        "import {}\n\nlet previewBrowserFilters: [String]? = {}\nlet previewBrowserIncludedModules: [String]? = [{}]\n",
        config.package_module,
        swift_optional_string_array(&config.preview_filters),
        swift_string_literal(&config.package_module)
    )
}

fn swift_optional_string_array(values: &[String]) -> String {
    if values.is_empty() {
        "nil".to_string()
    } else {
        format!(
            "[{}]",
            values
                .iter()
                .map(|value| swift_string_literal(value))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn swift_string_literal(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
}

fn create_project_file(
    package_relative_path: &str,
    package_product: &str,
    deployment_target: &str,
) -> String {
    format!(
        r#"// !$*UTF8*$!
{{
  archiveVersion = 1;
  classes = {{}};
  objectVersion = 77;
  objects = {{
    100000000000000000000007 /* FocusedPreviewApp.swift in Sources */ = {{isa = PBXBuildFile; fileRef = 100000000000000000000006 /* FocusedPreviewApp.swift */; }};
    100000000000000000000024 /* PreviewBrowserEntries.swift in Sources */ = {{isa = PBXBuildFile; fileRef = 100000000000000000000023 /* PreviewBrowserEntries.swift */; }};
    100000000000000000000027 /* PreviewBrowserConfiguration.swift in Sources */ = {{isa = PBXBuildFile; fileRef = 100000000000000000000026 /* PreviewBrowserConfiguration.swift */; }};
    100000000000000000000008 /* SnapshotPreviewsCore in Frameworks */ = {{isa = PBXBuildFile; productRef = 100000000000000000000017 /* SnapshotPreviewsCore */; }};
    100000000000000000000009 /* {package_product} in Frameworks */ = {{isa = PBXBuildFile; productRef = 100000000000000000000016 /* {package_product} */; }};
    10000000000000000000001A /* FocusedPreviewHotReloadRuntime.swift in Sources */ = {{isa = PBXBuildFile; fileRef = 100000000000000000000019 /* FocusedPreviewHotReloadRuntime.swift */; }};
    100000000000000000000029 /* PreviewBrowserEntries.swift in Sources */ = {{isa = PBXBuildFile; fileRef = 100000000000000000000023 /* PreviewBrowserEntries.swift */; }};
    100000000000000000000028 /* PreviewBrowserConfiguration.swift in Sources */ = {{isa = PBXBuildFile; fileRef = 100000000000000000000026 /* PreviewBrowserConfiguration.swift */; }};
    10000000000000000000001B /* SnapshotPreviewsCore in Frameworks */ = {{isa = PBXBuildFile; productRef = 100000000000000000000017 /* SnapshotPreviewsCore */; }};
    10000000000000000000001C /* {package_product} in Frameworks */ = {{isa = PBXBuildFile; productRef = 100000000000000000000016 /* {package_product} */; }};
    100000000000000000000005 /* PreviewHost.app */ = {{isa = PBXFileReference; explicitFileType = wrapper.application; includeInIndex = 0; path = PreviewHost.app; sourceTree = BUILT_PRODUCTS_DIR; }};
    100000000000000000000006 /* FocusedPreviewApp.swift */ = {{isa = PBXFileReference; lastKnownFileType = sourcecode.swift; path = FocusedPreviewApp.swift; sourceTree = "<group>"; }};
    100000000000000000000018 /* libPreviewReloadPlugin.dylib */ = {{isa = PBXFileReference; explicitFileType = "compiled.mach-o.dylib"; includeInIndex = 0; path = libPreviewReloadPlugin.dylib; sourceTree = BUILT_PRODUCTS_DIR; }};
    100000000000000000000019 /* FocusedPreviewHotReloadRuntime.swift */ = {{isa = PBXFileReference; lastKnownFileType = sourcecode.swift; path = FocusedPreviewHotReloadRuntime.swift; sourceTree = "<group>"; }};
    100000000000000000000023 /* PreviewBrowserEntries.swift */ = {{isa = PBXFileReference; lastKnownFileType = sourcecode.swift; path = PreviewBrowserEntries.swift; sourceTree = "<group>"; }};
    100000000000000000000026 /* PreviewBrowserConfiguration.swift */ = {{isa = PBXFileReference; lastKnownFileType = sourcecode.swift; path = PreviewBrowserConfiguration.swift; sourceTree = "<group>"; }};
    10000000000000000000000C /* Frameworks */ = {{isa = PBXFrameworksBuildPhase; buildActionMask = 2147483647; files = (100000000000000000000008, 100000000000000000000009,); runOnlyForDeploymentPostprocessing = 0; }};
    10000000000000000000001F /* Frameworks */ = {{isa = PBXFrameworksBuildPhase; buildActionMask = 2147483647; files = (10000000000000000000001B, 10000000000000000000001C,); runOnlyForDeploymentPostprocessing = 0; }};
    100000000000000000000002 = {{isa = PBXGroup; children = (100000000000000000000004, 100000000000000000000003,); sourceTree = "<group>"; }};
    100000000000000000000003 /* Products */ = {{isa = PBXGroup; children = (100000000000000000000005, 100000000000000000000018,); name = Products; sourceTree = "<group>"; }};
    100000000000000000000004 /* PreviewHost */ = {{isa = PBXGroup; children = (100000000000000000000006, 100000000000000000000019, 100000000000000000000023, 100000000000000000000026,); path = PreviewHost; sourceTree = "<group>"; }};
    10000000000000000000000A /* PreviewHost */ = {{isa = PBXNativeTarget; buildConfigurationList = 10000000000000000000000F; buildPhases = (10000000000000000000000B, 10000000000000000000000C, 10000000000000000000000D,); buildRules = (); dependencies = (); name = PreviewHost; packageProductDependencies = (100000000000000000000016, 100000000000000000000017,); productName = PreviewHost; productReference = 100000000000000000000005; productType = "com.apple.product-type.application"; }};
    10000000000000000000001D /* PreviewReloadPlugin */ = {{isa = PBXNativeTarget; buildConfigurationList = 100000000000000000000020; buildPhases = (10000000000000000000001E, 10000000000000000000001F,); buildRules = (); dependencies = (); name = PreviewReloadPlugin; packageProductDependencies = (100000000000000000000016, 100000000000000000000017,); productName = PreviewReloadPlugin; productReference = 100000000000000000000018; productType = "com.apple.product-type.library.dynamic"; }};
    100000000000000000000001 /* Project object */ = {{isa = PBXProject; attributes = {{BuildIndependentTargetsInParallel = 1; LastSwiftUpdateCheck = 2641; LastUpgradeCheck = 2641; TargetAttributes = {{10000000000000000000000A = {{CreatedOnToolsVersion = 26.4.1; }}; 10000000000000000000001D = {{CreatedOnToolsVersion = 26.4.1; }}; }}; }}; buildConfigurationList = 10000000000000000000000E; developmentRegion = en; hasScannedForEncodings = 0; knownRegions = (en, Base,); mainGroup = 100000000000000000000002; minimizedProjectReferenceProxies = 1; packageReferences = (100000000000000000000014, 100000000000000000000015,); preferredProjectObjectVersion = 77; productRefGroup = 100000000000000000000003; projectDirPath = ""; projectRoot = ""; targets = (10000000000000000000000A, 10000000000000000000001D,); }};
    10000000000000000000000D /* Resources */ = {{isa = PBXResourcesBuildPhase; buildActionMask = 2147483647; files = (); runOnlyForDeploymentPostprocessing = 0; }};
    10000000000000000000000B /* Sources */ = {{isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (100000000000000000000007, 100000000000000000000024, 100000000000000000000027,); runOnlyForDeploymentPostprocessing = 0; }};
    10000000000000000000001E /* Sources */ = {{isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (10000000000000000000001A, 100000000000000000000029, 100000000000000000000028,); runOnlyForDeploymentPostprocessing = 0; }};
    100000000000000000000010 /* Debug */ = {{isa = XCBuildConfiguration; buildSettings = {{ALWAYS_SEARCH_USER_PATHS = NO; CLANG_ENABLE_MODULES = YES; CLANG_ENABLE_OBJC_ARC = YES; COPY_PHASE_STRIP = NO; DEBUG_INFORMATION_FORMAT = dwarf; ENABLE_STRICT_OBJC_MSGSEND = YES; ENABLE_TESTABILITY = YES; GCC_C_LANGUAGE_STANDARD = gnu17; GCC_DYNAMIC_NO_PIC = NO; GCC_NO_COMMON_BLOCKS = YES; GCC_OPTIMIZATION_LEVEL = 0; GCC_PREPROCESSOR_DEFINITIONS = ("DEBUG=1", "$(inherited)",); GCC_WARN_64_TO_32_BIT_CONVERSION = YES; GCC_WARN_ABOUT_RETURN_TYPE = YES_ERROR; GCC_WARN_UNDECLARED_SELECTOR = YES; GCC_WARN_UNINITIALIZED_AUTOS = YES_AGGRESSIVE; GCC_WARN_UNUSED_FUNCTION = YES; GCC_WARN_UNUSED_VARIABLE = YES; IPHONEOS_DEPLOYMENT_TARGET = {deployment_target}; MTL_ENABLE_DEBUG_INFO = INCLUDE_SOURCE; MTL_FAST_MATH = YES; ONLY_ACTIVE_ARCH = YES; SDKROOT = iphoneos; SWIFT_ACTIVE_COMPILATION_CONDITIONS = DEBUG; }}; name = Debug; }};
    100000000000000000000011 /* Release */ = {{isa = XCBuildConfiguration; buildSettings = {{ALWAYS_SEARCH_USER_PATHS = NO; CLANG_ENABLE_MODULES = YES; CLANG_ENABLE_OBJC_ARC = YES; COPY_PHASE_STRIP = NO; ENABLE_NS_ASSERTIONS = NO; ENABLE_STRICT_OBJC_MSGSEND = YES; GCC_C_LANGUAGE_STANDARD = gnu17; GCC_NO_COMMON_BLOCKS = YES; GCC_WARN_64_TO_32_BIT_CONVERSION = YES; GCC_WARN_ABOUT_RETURN_TYPE = YES_ERROR; IPHONEOS_DEPLOYMENT_TARGET = {deployment_target}; MTL_FAST_MATH = YES; SDKROOT = iphoneos; SWIFT_COMPILATION_MODE = wholemodule; VALIDATE_PRODUCT = YES; }}; name = Release; }};
    100000000000000000000012 /* Debug */ = {{isa = XCBuildConfiguration; buildSettings = {{CODE_SIGN_STYLE = Automatic; CURRENT_PROJECT_VERSION = 1; DEVELOPMENT_TEAM = ""; ENABLE_PREVIEWS = NO; GENERATE_INFOPLIST_FILE = YES; INFOPLIST_KEY_CFBundleDisplayName = "SwiftUI Preview Browser"; INFOPLIST_KEY_LSRequiresIPhoneOS = YES; INFOPLIST_KEY_UIApplicationSceneManifest_Generation = YES; INFOPLIST_KEY_UIApplicationSupportsIndirectInputEvents = YES; INFOPLIST_KEY_UILaunchScreen_Generation = YES; LD_RUNPATH_SEARCH_PATHS = ("$(inherited)", "@executable_path/Frameworks",); MARKETING_VERSION = 1.0; PRODUCT_BUNDLE_IDENTIFIER = dev.simx.preview.host; PRODUCT_NAME = "$(TARGET_NAME)"; SUPPORTED_PLATFORMS = "iphoneos iphonesimulator"; SUPPORTS_MACCATALYST = NO; SWIFT_EMIT_LOC_STRINGS = YES; SWIFT_VERSION = 6.0; TARGETED_DEVICE_FAMILY = "1,2"; }}; name = Debug; }};
    100000000000000000000013 /* Release */ = {{isa = XCBuildConfiguration; buildSettings = {{CODE_SIGN_STYLE = Automatic; CURRENT_PROJECT_VERSION = 1; DEVELOPMENT_TEAM = ""; ENABLE_PREVIEWS = NO; GENERATE_INFOPLIST_FILE = YES; INFOPLIST_KEY_CFBundleDisplayName = "SwiftUI Preview Browser"; INFOPLIST_KEY_LSRequiresIPhoneOS = YES; INFOPLIST_KEY_UIApplicationSceneManifest_Generation = YES; INFOPLIST_KEY_UIApplicationSupportsIndirectInputEvents = YES; INFOPLIST_KEY_UILaunchScreen_Generation = YES; LD_RUNPATH_SEARCH_PATHS = ("$(inherited)", "@executable_path/Frameworks",); MARKETING_VERSION = 1.0; PRODUCT_BUNDLE_IDENTIFIER = dev.simx.preview.host; PRODUCT_NAME = "$(TARGET_NAME)"; SUPPORTED_PLATFORMS = "iphoneos iphonesimulator"; SUPPORTS_MACCATALYST = NO; SWIFT_EMIT_LOC_STRINGS = YES; SWIFT_VERSION = 6.0; TARGETED_DEVICE_FAMILY = "1,2"; }}; name = Release; }};
    100000000000000000000021 /* Debug */ = {{isa = XCBuildConfiguration; buildSettings = {{CODE_SIGN_STYLE = Automatic; CURRENT_PROJECT_VERSION = 1; DEVELOPMENT_TEAM = ""; EXECUTABLE_PREFIX = lib; MACH_O_TYPE = mh_dylib; MARKETING_VERSION = 1.0; PRODUCT_NAME = "$(TARGET_NAME)"; SKIP_INSTALL = YES; SUPPORTED_PLATFORMS = "iphoneos iphonesimulator"; SUPPORTS_MACCATALYST = NO; SWIFT_EMIT_LOC_STRINGS = YES; SWIFT_VERSION = 6.0; TARGETED_DEVICE_FAMILY = "1,2"; }}; name = Debug; }};
    100000000000000000000022 /* Release */ = {{isa = XCBuildConfiguration; buildSettings = {{CODE_SIGN_STYLE = Automatic; CURRENT_PROJECT_VERSION = 1; DEVELOPMENT_TEAM = ""; EXECUTABLE_PREFIX = lib; MACH_O_TYPE = mh_dylib; MARKETING_VERSION = 1.0; PRODUCT_NAME = "$(TARGET_NAME)"; SKIP_INSTALL = YES; SUPPORTED_PLATFORMS = "iphoneos iphonesimulator"; SUPPORTS_MACCATALYST = NO; SWIFT_EMIT_LOC_STRINGS = YES; SWIFT_VERSION = 6.0; TARGETED_DEVICE_FAMILY = "1,2"; }}; name = Release; }};
    10000000000000000000000E = {{isa = XCConfigurationList; buildConfigurations = (100000000000000000000010, 100000000000000000000011,); defaultConfigurationIsVisible = 0; defaultConfigurationName = Release; }};
    10000000000000000000000F = {{isa = XCConfigurationList; buildConfigurations = (100000000000000000000012, 100000000000000000000013,); defaultConfigurationIsVisible = 0; defaultConfigurationName = Release; }};
    100000000000000000000020 = {{isa = XCConfigurationList; buildConfigurations = (100000000000000000000021, 100000000000000000000022,); defaultConfigurationIsVisible = 0; defaultConfigurationName = Release; }};
    100000000000000000000014 /* XCLocalSwiftPackageReference */ = {{isa = XCLocalSwiftPackageReference; relativePath = {package_relative_path_literal}; }};
    100000000000000000000015 /* XCRemoteSwiftPackageReference "SnapshotPreviews-iOS" */ = {{isa = XCRemoteSwiftPackageReference; repositoryURL = "https://github.com/EmergeTools/SnapshotPreviews-iOS.git"; requirement = {{kind = revision; revision = d42446f0439217941a4e3a2ca58a643c1ac328c4; }}; }};
    100000000000000000000016 /* {package_product} */ = {{isa = XCSwiftPackageProductDependency; package = 100000000000000000000014; productName = {package_product_literal}; }};
    100000000000000000000017 /* SnapshotPreviewsCore */ = {{isa = XCSwiftPackageProductDependency; package = 100000000000000000000015; productName = SnapshotPreviewsCore; }};
  }};
  rootObject = 100000000000000000000001;
}}
"#,
        package_product = package_product,
        deployment_target = deployment_target,
        package_relative_path_literal = swift_string_literal(package_relative_path),
        package_product_literal = swift_string_literal(package_product),
    )
}

const FOCUSED_PREVIEW_APP: &str = r#"import Darwin
import Foundation
import Observation
import SwiftUI

@main
@MainActor
struct FocusedPreviewApp: App {
  var body: some Scene {
    WindowGroup {
      FocusedPreviewRootView()
    }
  }
}

@MainActor
@Observable
private final class FocusedPreviewStore {
  nonisolated private static let hotReloadRequestedNotificationName = "dev.swiftui-preview-browser.reload"

  init() {
    previewVariants = previewBrowserPreviewVariants()
    observeReloadRequests()
  }

  deinit {
    CFNotificationCenterRemoveObserver(
      CFNotificationCenterGetDarwinNotifyCenter(),
      UnsafeRawPointer(Unmanaged.passUnretained(self).toOpaque()),
      CFNotificationName(Self.hotReloadRequestedNotificationName as CFString),
      nil
    )
  }

  private(set) var previewVariants: [PreviewVariant]
  var selectedPageIndex = 0 {
    didSet {
      let clampedPageIndex = Self.clampedPageIndex(selectedPageIndex, for: previewVariants)
      guard selectedPageIndex != clampedPageIndex else { return }
      selectedPageIndex = clampedPageIndex
    }
  }

  func loadHotReloadIfNeeded() {
    guard
      let manifest = HotReloadManifest(url: PreviewBrowserFiles.manifestURL),
      manifest.token != lastHotReloadToken
    else {
      return
    }

    lastHotReloadToken = manifest.token
    let handle = dlopen(manifest.dylibPath, RTLD_NOW | RTLD_LOCAL)
    guard let handle else {
      let error =
        if let errorPointer = dlerror() {
          String(cString: errorPointer)
        } else {
          "unknown dlopen error"
        }
      writeStatus(phase: "error", error: error)
      return
    }

    hotReloadHandlesToKeepAlive.append(handle)
    let hotReloadPreviewVariants = Self.hotReloadPreviewVariants(
      handle: handle,
      generation: manifest.token
    )
    guard !hotReloadPreviewVariants.isEmpty else {
      writeStatus(phase: "error", error: "No previews found in generation \(manifest.token)")
      return
    }

    replacePreviewVariants(with: hotReloadPreviewVariants)
    writeStatus(phase: "reloaded", error: nil)
  }

  @ObservationIgnored private var hotReloadHandlesToKeepAlive = [UnsafeMutableRawPointer]()
  @ObservationIgnored private var lastHotReloadToken: String?
  @ObservationIgnored private let statusWriter = HostStatusWriter()
  @ObservationIgnored private var pendingStatusWrite: Task<Void, Never>?

  private static func hotReloadPreviewVariants(
    handle: UnsafeMutableRawPointer,
    generation: String
  ) -> [PreviewVariant] {
    guard
      let previewCountSymbol = dlsym(handle, "focused_preview_hot_reload_preview_count"),
      let idSymbol = dlsym(handle, "focused_preview_hot_reload_preview_id"),
      let groupDisplayNameSymbol = dlsym(handle, "focused_preview_hot_reload_group_display_name"),
      let displayNameSymbol = dlsym(handle, "focused_preview_hot_reload_preview_display_name"),
      let freeStringSymbol = dlsym(handle, "focused_preview_hot_reload_free_string"),
      let makeViewSymbol = dlsym(handle, "focused_preview_hot_reload_make_view")
    else {
      return []
    }

    let previewCount = unsafeBitCast(previewCountSymbol, to: HotReloadPreviewCountFunction.self)
    let id = unsafeBitCast(idSymbol, to: HotReloadPreviewStringFunction.self)
    let groupDisplayName = unsafeBitCast(groupDisplayNameSymbol, to: HotReloadPreviewStringFunction.self)
    let displayName = unsafeBitCast(displayNameSymbol, to: HotReloadPreviewStringFunction.self)
    let freeString = unsafeBitCast(freeStringSymbol, to: HotReloadPreviewFreeStringFunction.self)
    let makeView = unsafeBitCast(makeViewSymbol, to: HotReloadPreviewMakeViewFunction.self)

    return (0..<previewCount()).compactMap { previewIndex -> PreviewVariant? in
      guard
        let id = hotReloadPreviewString(id(previewIndex), freeString: freeString),
        let groupDisplayName = hotReloadPreviewString(groupDisplayName(previewIndex), freeString: freeString),
        let displayName = hotReloadPreviewString(displayName(previewIndex), freeString: freeString)
      else {
        return nil
      }

      return PreviewVariant(
        id: "\(generation):\(id)",
        groupDisplayName: groupDisplayName,
        displayName: displayName,
        makeView: {
          guard let viewPointer = makeView(previewIndex) else {
            return AnyView(EmptyView())
          }
          let view = viewPointer.assumingMemoryBound(to: AnyView.self)
          let anyView = view.move()
          view.deallocate()
          return anyView
        }
      )
    }
  }

  private static func clampedPageIndex(_ index: Int, for variants: [PreviewVariant]) -> Int {
    min(max(0, index), max(0, variants.count - 1))
  }

  private func replacePreviewVariants(with variants: [PreviewVariant]) {
    let clampedPageIndex = Self.clampedPageIndex(selectedPageIndex, for: variants)
    previewVariants = variants
    selectedPageIndex = clampedPageIndex
  }

  private func observeReloadRequests() {
    do {
      try FileManager.default.createDirectory(
        at: PreviewBrowserFiles.directoryURL,
        withIntermediateDirectories: true
      )
    } catch {
      print("Unable to create status directory: \(error.localizedDescription)")
    }
    writeStatus(phase: "running", error: nil)

    let hotReloadRequestedCallback: CFNotificationCallback = { _, observer, _, _, _ in
      guard let observer else { return }
      let store = Unmanaged<FocusedPreviewStore>.fromOpaque(observer).takeUnretainedValue()
      Task<Void, Never> {
        store.loadHotReloadIfNeeded()
      }
    }

    CFNotificationCenterAddObserver(
      CFNotificationCenterGetDarwinNotifyCenter(),
      UnsafeRawPointer(Unmanaged.passUnretained(self).toOpaque()),
      hotReloadRequestedCallback,
      Self.hotReloadRequestedNotificationName as CFString,
      nil,
      .deliverImmediately
    )
  }

  private func writeStatus(phase: String, error: String?) {
    pendingStatusWrite = Task { [statusWriter, lastHotReloadToken, previousStatusWrite = pendingStatusWrite] in
      await previousStatusWrite?.value
      await statusWriter.write(phase: phase, lastToken: lastHotReloadToken, lastError: error)
    }
  }
}

private typealias HotReloadPreviewCountFunction = @convention(c) () -> Int
private typealias HotReloadPreviewStringFunction = @convention(c) (Int) -> UnsafeMutablePointer<CChar>?
private typealias HotReloadPreviewFreeStringFunction = @convention(c) (UnsafeMutablePointer<CChar>?) -> Void
private typealias HotReloadPreviewMakeViewFunction = @convention(c) (Int) -> UnsafeMutableRawPointer?

private func hotReloadPreviewString(
  _ pointer: UnsafeMutablePointer<CChar>?,
  freeString: HotReloadPreviewFreeStringFunction
) -> String? {
  guard let pointer else { return nil }
  defer { freeString(pointer) }
  return String(cString: pointer)
}

private struct HostStatus: Encodable {
  let pid: Int
  let phase: String
  let lastToken: String?
  let lastError: String?
}

private enum PreviewBrowserFiles {
  static let directoryURL = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    .appendingPathComponent("swiftui-preview-browser", isDirectory: true)
  static let manifestURL = directoryURL.appendingPathComponent("reload.json", isDirectory: false)
  static let statusURL = directoryURL.appendingPathComponent("status.json", isDirectory: false)
}

private actor HostStatusWriter {
  private let encoder = JSONEncoder()

  func write(phase: String, lastToken: String?, lastError: String?) {
    let status = HostStatus(
      pid: Int(getpid()),
      phase: phase,
      lastToken: lastToken,
      lastError: lastError
    )
    do {
      let data = try encoder.encode(status)
      try data.write(to: PreviewBrowserFiles.statusURL, options: .atomic)
    } catch {
      print("Unable to write host status: \(error.localizedDescription)")
    }
  }
}

private struct HotReloadManifest: Decodable {
  let token: String
  let dylibPath: String

  init?(url: URL) {
    do {
      let data = try Data(contentsOf: url)
      self = try JSONDecoder().decode(HotReloadManifest.self, from: data)
    } catch {
      return nil
    }
  }
}

@MainActor
private struct FocusedPreviewRootView: View {
  @State private var store = FocusedPreviewStore()

  var body: some View {
    VStack(spacing: 0) {
      if store.previewVariants.isEmpty {
        ContentUnavailableView("No Previews", systemImage: "eye.slash")
      } else {
        TabView(selection: $store.selectedPageIndex) {
          ForEach(Array(store.previewVariants.enumerated()), id: \.offset) { index, variant in
            VStack(spacing: 12) {
              Text(variant.groupDisplayName)
                .font(.headline)
              variant.makeView()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .padding()
            .tag(index)
          }
        }
        .tabViewStyle(.page(indexDisplayMode: .automatic))
      }
    }
    .onAppear {
      store.loadHotReloadIfNeeded()
    }
  }
}
"#;

const HOT_RELOAD_RUNTIME: &str = r#"import Darwin
import Foundation
import SwiftUI

@MainActor private let cachedHotReloadPreviewVariants = previewBrowserPreviewVariants()

private func hotReloadPreviewCString(_ value: String) -> UnsafeMutablePointer<CChar>? {
  strdup(value)
}

@_cdecl("focused_preview_hot_reload_preview_count")
public func focusedPreviewHotReloadPreviewCount() -> Int {
  MainActor.assumeIsolated {
    cachedHotReloadPreviewVariants.count
  }
}

@_cdecl("focused_preview_hot_reload_preview_id")
@MainActor
public func focusedPreviewHotReloadPreviewID(_ previewIndex: Int) -> UnsafeMutablePointer<CChar>? {
  hotReloadPreviewCString(cachedHotReloadPreviewVariants[previewIndex].id)
}

@_cdecl("focused_preview_hot_reload_group_display_name")
@MainActor
public func focusedPreviewHotReloadGroupDisplayName(
  _ previewIndex: Int
) -> UnsafeMutablePointer<CChar>? {
  hotReloadPreviewCString(cachedHotReloadPreviewVariants[previewIndex].groupDisplayName)
}

@_cdecl("focused_preview_hot_reload_preview_display_name")
@MainActor
public func focusedPreviewHotReloadPreviewDisplayName(
  _ previewIndex: Int
) -> UnsafeMutablePointer<CChar>? {
  hotReloadPreviewCString(cachedHotReloadPreviewVariants[previewIndex].displayName)
}

@_cdecl("focused_preview_hot_reload_free_string")
public func focusedPreviewHotReloadFreeString(_ pointer: UnsafeMutablePointer<CChar>?) {
  free(pointer)
}

@MainActor
@_cdecl("focused_preview_hot_reload_make_view")
public func focusedPreviewHotReloadMakeView(
  _ previewIndex: Int
) -> UnsafeMutableRawPointer? {
  let previewVariant = cachedHotReloadPreviewVariants[previewIndex]
  let view = UnsafeMutablePointer<AnyView>.allocate(capacity: 1)
  view.initialize(to: previewVariant.makeView())
  return UnsafeMutableRawPointer(view)
}
"#;

const PREVIEW_BROWSER_ENTRIES: &str = r#"import Foundation
import SnapshotPreviewsCore
import SwiftUI

struct PreviewVariant: Identifiable {
  let id: String
  let groupDisplayName: String
  let displayName: String
  let makeView: @MainActor () -> AnyView
}

@MainActor
func previewBrowserPreviewVariants() -> [PreviewVariant] {
  previewBrowserPreviewTypes().flatMap { previewType in
    previewType.previews.enumerated().map { index, preview in
      PreviewVariant(
        id: "\(previewType.typeName):\(index)",
        groupDisplayName: previewType.displayName,
        displayName: preview.displayName ?? "Preview",
        makeView: { AnyView(preview.view()) }
      )
    }
  }
}

@MainActor
private func previewBrowserPreviewTypes() -> [PreviewType] {
  let previewTypes = FindPreviews.findPreviews(
    included: nil,
    excluded: nil,
    includedModules: previewBrowserIncludedModules,
    excludedModules: nil
  )

  var retainedTypeNames = Set<String>()
  let newestPreviewTypes = previewTypes.reversed().filter { previewType in
    retainedTypeNames.insert(previewType.typeName).inserted
  }.reversed()

  guard let filters = previewBrowserFilters else {
    return Array(newestPreviewTypes)
  }

  return newestPreviewTypes.compactMap { previewType in
    var filteredType = previewType
    filteredType.previews = previewType.previews.filter { preview in
      matchesFilter(
        typeName: previewType.typeName,
        groupDisplayName: previewType.displayName,
        displayName: preview.displayName ?? "Preview",
        filters: filters
      )
    }
    return filteredType.previews.isEmpty ? nil : filteredType
  }
}

private func matchesFilter(
  typeName: String,
  groupDisplayName: String,
  displayName: String,
  filters: [String]
) -> Bool {
  let candidates = [typeName, groupDisplayName, displayName]
  return filters.contains { filter in
    candidates.contains {
      $0.range(of: filter, options: [.regularExpression, .caseInsensitive]) != nil
    }
  }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_smallest_static_library_product_for_target() {
        let dump = PackageDump {
            targets: vec![PackageTarget {
                name: "Feature".to_string(),
                target_type: "regular".to_string(),
            }],
            products: vec![
                PackageProduct {
                    name: "Everything".to_string(),
                    targets: vec!["Feature".to_string(), "Other".to_string()],
                    product_type: serde_json::json!({"library":["automatic"]}),
                },
                PackageProduct {
                    name: "FeatureKit".to_string(),
                    targets: vec!["Feature".to_string()],
                    product_type: serde_json::json!({"library":["automatic"]}),
                },
            ],
            platforms: vec![],
        };

        assert_eq!(
            infer_package_product("Feature", &dump).unwrap(),
            "FeatureKit"
        );
    }

    #[test]
    fn rejects_dynamic_only_product() {
        let dump = PackageDump {
            targets: vec![],
            products: vec![PackageProduct {
                name: "Feature".to_string(),
                targets: vec!["Feature".to_string()],
                product_type: serde_json::json!({"library":["dynamic"]}),
            }],
            platforms: vec![],
        };

        assert!(infer_package_product("Feature", &dump).is_err());
    }

    #[test]
    fn generated_configuration_escapes_filters() {
        let config = PreviewConfig {
            package_root: PathBuf::from("/tmp/App"),
            package_target: "App".to_string(),
            package_product: "App".to_string(),
            package_module: "App".to_string(),
            deployment_target: "17.0".to_string(),
            preview_filters: vec!["A \"quoted\" preview".to_string()],
        };

        let source = generated_configuration_source(&config);
        assert!(source.contains("import App"));
        assert!(source.contains("\"A \\\"quoted\\\" preview\""));
    }
}
