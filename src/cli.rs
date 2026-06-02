use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use directories::BaseDirs;
use serde::Serialize;

use crate::pool::{LeaseOptions, PoolConfig, PoolDevice, PoolService};
use crate::simctl::{Simctl, XcrunSimctl};
use crate::stream::{serve, ServeConfig, StreamStats};

#[derive(Debug, Parser)]
#[command(
    name = "simx",
    version,
    about = "Agent-friendly iOS Simulator device pool"
)]
struct Cli {
    /// Print CLI errors as stable JSON.
    #[arg(long, global = true)]
    json_errors: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize or reconcile the fixed simulator pool.
    Init {
        #[arg(long)]
        size: usize,
        #[arg(long)]
        device_type: Option<String>,
        #[arg(long)]
        runtime: Option<String>,
    },
    /// Show the current pool state.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Lease a simulator for an agent.
    Lease {
        #[arg(long)]
        slug: String,
        #[arg(long, default_value = "60s", value_parser = parse_duration)]
        wait_timeout: Duration,
        #[arg(long, default_value = "30m", value_parser = parse_duration)]
        ttl: Duration,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        serve: bool,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8080)]
        port: u16,
        #[arg(long, default_value_t = 0.7)]
        quality: f32,
        #[arg(long, default_value_t = 120)]
        fps: u32,
        #[arg(long, default_value = "5m", value_parser = parse_duration)]
        idle_timeout: Duration,
        #[arg(long)]
        new: bool,
    },
    /// Release a simulator lease back to the pool.
    Release {
        #[arg(long)]
        slug: String,
    },
    /// Serve an existing active lease.
    Serve {
        #[arg(long)]
        slug: String,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8080)]
        port: u16,
        #[arg(long, default_value_t = 0.7)]
        quality: f32,
        #[arg(long, default_value_t = 120)]
        fps: u32,
        #[arg(long, default_value = "5m", value_parser = parse_duration)]
        idle_timeout: Duration,
    },
    /// Build, install, and launch the app in the current Xcode project.
    Run {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        project: Option<PathBuf>,
        #[arg(long)]
        scheme: Option<String>,
        #[arg(long, default_value = "Debug")]
        configuration: String,
        #[arg(long)]
        derived_data_path: Option<PathBuf>,
        #[arg(long)]
        bundle_id: Option<String>,
        #[arg(long)]
        no_launch: bool,
        #[arg(long)]
        json: bool,
    },
    /// Install and launch an app bundle on an active lease.
    Install {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        app: PathBuf,
        #[arg(long)]
        bundle_id: Option<String>,
        #[arg(long)]
        no_launch: bool,
        #[arg(long)]
        json: bool,
    },
    /// Extend an active simulator lease.
    Renew {
        #[arg(long)]
        slug: String,
        #[arg(long, default_value = "30m", value_parser = parse_duration)]
        ttl: Duration,
        #[arg(long)]
        json: bool,
    },
    /// Shut down and delete all devices in the simx pool.
    Clean,
    /// Check host tooling required by simx.
    Doctor {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Serialize)]
struct LeaseOutput<'a> {
    slug: &'a str,
    udid: &'a str,
    device_name: &'a str,
    lease_expires_at: Option<u64>,
    lease_expires_at_rfc3339: Option<String>,
    ttl_seconds: u64,
    serve: ServeOutput,
}

#[derive(Debug, Serialize)]
struct ServeOutput {
    command: String,
    url: String,
    stream: String,
    stats: String,
}

#[derive(Debug, Serialize)]
struct StatusOutput {
    size: usize,
    device_type: String,
    runtime: String,
    devices: Vec<StatusDeviceOutput>,
}

#[derive(Debug, Serialize)]
struct StatusDeviceOutput {
    name: String,
    udid: String,
    slug: Option<String>,
    lease_expires_at: Option<u64>,
    lease_expires_at_rfc3339: Option<String>,
    serve_pid: Option<u32>,
    serve_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct RunOutput<'a> {
    slug: &'a str,
    udid: &'a str,
    run_state: String,
    project: String,
    scheme: String,
    configuration: String,
    derived_data_path: String,
    app: String,
    bundle_id: String,
    launched: bool,
}

#[derive(Debug, Serialize)]
struct InstallOutput<'a> {
    slug: &'a str,
    udid: &'a str,
    app: String,
    bundle_id: String,
    launched: bool,
}

#[derive(Debug, Serialize)]
struct ErrorOutput<'a> {
    ok: bool,
    code: &'a str,
    message: String,
}

#[derive(Debug, Serialize)]
struct DoctorOutput {
    ok: bool,
    checks: Vec<DoctorCheck>,
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: &'static str,
    ok: bool,
    message: String,
}

pub fn main_entry() -> i32 {
    let wants_json_errors = std::env::args().any(|arg| arg == "--json-errors");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if wants_json_errors {
                let output = ErrorOutput {
                    ok: false,
                    code: "invalid_argument",
                    message: error.to_string(),
                };
                eprintln!(
                    "{}",
                    serde_json::to_string(&output).unwrap_or_else(|_| {
                        r#"{"ok":false,"code":"invalid_argument","message":"invalid arguments"}"#
                            .to_string()
                    })
                );
                return 2;
            }
            let _ = error.print();
            return error.exit_code();
        }
    };
    let json_errors = cli.json_errors;
    match default_state_path().and_then(|state_path| run_with(cli, state_path)) {
        Ok(()) => 0,
        Err(error) => {
            if json_errors {
                let output = ErrorOutput {
                    ok: false,
                    code: error_code(&error),
                    message: format!("{error:#}"),
                };
                eprintln!(
                    "{}",
                    serde_json::to_string(&output).unwrap_or_else(|_| {
                        r#"{"ok":false,"code":"internal","message":"failed to serialize error"}"#
                            .to_string()
                    })
                );
            } else {
                eprintln!("{error:#}");
            }
            exit_code(&error)
        }
    }
}

pub fn run() -> anyhow::Result<()> {
    run_with(Cli::parse(), default_state_path()?)
}

fn run_with(cli: Cli, state_path: PathBuf) -> anyhow::Result<()> {
    let mut service = PoolService::new(state_path.clone());
    let mut simctl = XcrunSimctl::default();

    match cli.command {
        Command::Init {
            size,
            device_type,
            runtime,
        } => {
            let state = service.init(
                &mut simctl,
                PoolConfig {
                    size,
                    device_type,
                    runtime,
                },
            )?;
            println!("initialized {} simulator(s)", state.devices.len());
            for device in state.devices {
                println!("{} {}", device.name, device.udid);
            }
        }
        Command::Status { json } => {
            let state = service.status()?;
            if json {
                let output = StatusOutput {
                    size: state.size,
                    device_type: state.device_type_id,
                    runtime: state.runtime_id,
                    devices: state
                        .devices
                        .into_iter()
                        .map(|device| {
                            let serve_url = match (
                                device.serve_host.as_deref(),
                                device.serve_port,
                                device.lease_id.as_deref(),
                            ) {
                                (Some(host), Some(port), Some(slug)) => {
                                    Some(format!("http://{host}:{port}/{slug}"))
                                }
                                _ => None,
                            };
                            StatusDeviceOutput {
                                name: device.name,
                                udid: device.udid,
                                slug: device.lease_id,
                                lease_expires_at: device.lease_expires_at,
                                lease_expires_at_rfc3339: device
                                    .lease_expires_at
                                    .map(format_unix_timestamp),
                                serve_pid: device.serve_pid,
                                serve_url,
                            }
                        })
                        .collect(),
                };
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("size: {}", state.size);
                println!("device_type: {}", state.device_type_id);
                println!("runtime: {}", state.runtime_id);
                for device in state.devices {
                    let owner = device.lease_id.as_deref().unwrap_or("idle");
                    let expires = device
                        .lease_expires_at
                        .map(format_unix_timestamp)
                        .unwrap_or_else(|| "-".to_string());
                    println!("{} {} {} {}", device.name, device.udid, owner, expires);
                }
            }
        }
        Command::Lease {
            slug,
            wait_timeout,
            ttl,
            json,
            serve: should_serve,
            host,
            port,
            quality,
            fps,
            idle_timeout,
            new: _new,
        } => {
            let device = service.lease(&mut simctl, &slug, LeaseOptions { wait_timeout, ttl })?;
            print_lease(&slug, &device, ttl, &host, port, json)?;
            if should_serve {
                run_serve(
                    &mut service,
                    ServeCommand {
                        slug,
                        host,
                        port,
                        quality,
                        fps,
                        idle_timeout,
                        udid: device.udid,
                    },
                    state_path,
                )?;
            }
        }
        Command::Serve {
            slug,
            host,
            port,
            quality,
            fps,
            idle_timeout,
        } => {
            let device = service.active_lease(&slug)?;
            run_serve(
                &mut service,
                ServeCommand {
                    slug,
                    host,
                    port,
                    quality,
                    fps,
                    idle_timeout,
                    udid: device.udid,
                },
                state_path,
            )?;
        }
        Command::Run {
            slug,
            project,
            scheme,
            configuration,
            derived_data_path,
            bundle_id,
            no_launch,
            json,
        } => {
            let device = service.active_lease(&slug)?;
            simctl
                .boot_if_needed(&device.udid)
                .with_context(|| format!("failed to boot {}", device.udid))?;
            run_xcode_app(RunAppCommand {
                slug,
                udid: device.udid,
                project,
                scheme,
                configuration,
                derived_data_path,
                bundle_id,
                launch: !no_launch,
                json,
            })?;
        }
        Command::Install {
            slug,
            app,
            bundle_id,
            no_launch,
            json,
        } => {
            let device = service.active_lease(&slug)?;
            simctl
                .boot_if_needed(&device.udid)
                .with_context(|| format!("failed to boot {}", device.udid))?;
            install_app_command(&slug, &device.udid, &app, bundle_id, !no_launch, json)?;
        }
        Command::Release { slug } => {
            let released = service.release(&slug)?;
            for process in &released.serve_processes {
                stop_process(process.pid);
            }
            if released.released {
                println!("released {slug}");
            } else {
                println!("no lease found for {slug}");
            }
        }
        Command::Renew { slug, ttl, json } => {
            let device = service.renew(&slug, ttl)?;
            print_lease(&slug, &device, ttl, "127.0.0.1", 8080, json)?;
        }
        Command::Clean => {
            let state = service.status().ok();
            if let Some(state) = state {
                for device in state.devices {
                    if let Some(pid) = device.serve_pid {
                        stop_process(pid);
                    }
                }
            }
            let devices = service.clean(&mut simctl)?;
            println!("removed {} simulator(s)", devices.len());
            for device in devices {
                println!("{} {}", device.name, device.udid);
            }
        }
        Command::Doctor { json } => {
            let output = doctor(default_state_path()?.as_path());
            if json {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                for check in &output.checks {
                    println!(
                        "{} {} - {}",
                        if check.ok { "ok" } else { "fail" },
                        check.name,
                        check.message
                    );
                }
            }
            if !output.ok {
                anyhow::bail!("doctor found failing checks");
            }
        }
    }

    Ok(())
}

struct ServeCommand {
    slug: String,
    host: String,
    port: u16,
    quality: f32,
    fps: u32,
    idle_timeout: Duration,
    udid: String,
}

fn run_serve(
    service: &mut PoolService,
    command: ServeCommand,
    state_path: PathBuf,
) -> anyhow::Result<()> {
    let pid = std::process::id();
    service.register_serve(
        &command.slug,
        &command.udid,
        pid,
        &command.host,
        command.port,
    )?;
    let result = serve(ServeConfig {
        host: command.host,
        port: command.port,
        quality: command.quality,
        fps: command.fps,
        idle_timeout: command.idle_timeout,
        slug: command.slug.clone(),
        udid: command.udid,
        state_path,
        stats: Arc::new(Mutex::new(StreamStats::default())),
        controllers: Arc::new(Mutex::new(None)),
    });
    let clear_result = service.clear_serve(&command.slug, pid);
    result.and(clear_result)
}

struct RunAppCommand {
    slug: String,
    udid: String,
    project: Option<PathBuf>,
    scheme: Option<String>,
    configuration: String,
    derived_data_path: Option<PathBuf>,
    bundle_id: Option<String>,
    launch: bool,
    json: bool,
}

fn run_xcode_app(command: RunAppCommand) -> anyhow::Result<()> {
    let project = resolve_xcode_project(command.project.as_deref())?;
    let scheme = command
        .scheme
        .unwrap_or_else(|| default_scheme_for_project(&project));
    let derived_data_path = command
        .derived_data_path
        .unwrap_or_else(|| default_derived_data_path(&command.slug));
    build_xcode_app(
        &project,
        &scheme,
        &command.configuration,
        &command.udid,
        &derived_data_path,
    )?;
    let app = find_built_app(&derived_data_path, &command.configuration, &scheme)?;
    let bundle_id = install_app(&command.udid, &app, command.bundle_id, command.launch)?;
    let run_state_path = write_run_state(RunStateInput {
        slug: &command.slug,
        udid: &command.udid,
        project: &project,
        scheme: &scheme,
        configuration: &command.configuration,
        derived_data_path: &derived_data_path,
        app: &app,
        bundle_id: &bundle_id,
        launched: command.launch,
    })?;

    if command.json {
        let output = RunOutput {
            slug: &command.slug,
            udid: &command.udid,
            run_state: run_state_path.display().to_string(),
            project: project.display().to_string(),
            scheme,
            configuration: command.configuration,
            derived_data_path: derived_data_path.display().to_string(),
            app: app.display().to_string(),
            bundle_id,
            launched: command.launch,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("built {}", project.display());
        println!("installed {}", app.display());
        println!("wrote {}", run_state_path.display());
        if command.launch {
            println!("launched {bundle_id}");
        }
    }
    Ok(())
}

fn install_app_command(
    slug: &str,
    udid: &str,
    app: &Path,
    bundle_id: Option<String>,
    launch: bool,
    json: bool,
) -> anyhow::Result<()> {
    let bundle_id = install_app(udid, app, bundle_id, launch)?;
    if json {
        let output = InstallOutput {
            slug,
            udid,
            app: app.display().to_string(),
            bundle_id,
            launched: launch,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("installed {}", app.display());
        if launch {
            println!("launched {bundle_id}");
        }
    }
    Ok(())
}

struct RunStateInput<'a> {
    slug: &'a str,
    udid: &'a str,
    project: &'a Path,
    scheme: &'a str,
    configuration: &'a str,
    derived_data_path: &'a Path,
    app: &'a Path,
    bundle_id: &'a str,
    launched: bool,
}

#[derive(Debug, Serialize)]
struct RunState<'a> {
    version: u32,
    slug: &'a str,
    udid: &'a str,
    project: String,
    scheme: &'a str,
    configuration: &'a str,
    derived_data_path: String,
    app: String,
    bundle_id: &'a str,
    launched: bool,
    updated_at: String,
}

fn write_run_state(input: RunStateInput<'_>) -> anyhow::Result<PathBuf> {
    let path = PathBuf::from(".simx").join("run.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let state = RunState {
        version: 1,
        slug: input.slug,
        udid: input.udid,
        project: input.project.display().to_string(),
        scheme: input.scheme,
        configuration: input.configuration,
        derived_data_path: input.derived_data_path.display().to_string(),
        app: input.app.display().to_string(),
        bundle_id: input.bundle_id,
        launched: input.launched,
        updated_at: format_unix_timestamp(now_unix_seconds()?),
    };
    let json = serde_json::to_string_pretty(&state)?;
    fs::write(&path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn resolve_xcode_project(project: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(project) = project {
        validate_xcode_project(project)?;
        return Ok(project.to_path_buf());
    }

    let mut projects = Vec::new();
    for entry in fs::read_dir(".").context("failed to read current directory")? {
        let path = entry?.path();
        if is_xcode_project(&path) {
            projects.push(path);
        }
    }
    projects.sort();
    match projects.len() {
        0 => anyhow::bail!("no .xcodeproj found in the current directory"),
        1 => Ok(projects.remove(0)),
        _ => anyhow::bail!(
            "multiple .xcodeproj files found; pass --project explicitly: {}",
            projects
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn validate_xcode_project(project: &Path) -> anyhow::Result<()> {
    if !is_xcode_project(project) {
        anyhow::bail!(
            "project path must point to a .xcodeproj: {}",
            project.display()
        );
    }
    Ok(())
}

fn is_xcode_project(path: &Path) -> bool {
    path.exists()
        && path.is_dir()
        && path.extension().and_then(|value| value.to_str()) == Some("xcodeproj")
}

fn default_scheme_for_project(project: &Path) -> String {
    project
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("App")
        .to_string()
}

fn default_derived_data_path(slug: &str) -> PathBuf {
    PathBuf::from(".simx")
        .join("DerivedData")
        .join(safe_path_component(slug))
}

fn safe_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn build_xcode_app(
    project: &Path,
    scheme: &str,
    configuration: &str,
    udid: &str,
    derived_data_path: &Path,
) -> anyhow::Result<()> {
    let output = ProcessCommand::new("/usr/bin/xcodebuild")
        .arg("-project")
        .arg(project)
        .arg("-scheme")
        .arg(scheme)
        .arg("-configuration")
        .arg(configuration)
        .arg("-destination")
        .arg(format!("platform=iOS Simulator,id={udid}"))
        .arg("-derivedDataPath")
        .arg(derived_data_path)
        .arg("build")
        .output()
        .context("failed to run xcodebuild")?;
    if output.status.success() {
        return Ok(());
    }
    anyhow::bail!(
        "xcodebuild failed: {}",
        command_failure_summary(&output.stdout, &output.stderr)
    );
}

fn command_failure_summary(stdout: &[u8], stderr: &[u8]) -> String {
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    let lines = combined
        .lines()
        .filter(|line| {
            line.contains("error:")
                || line.contains("fatal error:")
                || line.contains("BUILD FAILED")
                || line.contains("Testing failed")
        })
        .take(20)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        combined
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        lines.join("\n")
    }
}

fn find_built_app(
    derived_data_path: &Path,
    configuration: &str,
    scheme: &str,
) -> anyhow::Result<PathBuf> {
    let products_dir = derived_data_path
        .join("Build")
        .join("Products")
        .join(format!("{configuration}-iphonesimulator"));
    let expected = products_dir.join(format!("{scheme}.app"));
    if expected.exists() {
        return Ok(expected);
    }

    let mut apps = Vec::new();
    for entry in fs::read_dir(&products_dir).with_context(|| {
        format!(
            "failed to read build products at {}",
            products_dir.display()
        )
    })? {
        let path = entry?.path();
        if validate_app_path(&path).is_ok() {
            apps.push(path);
        }
    }
    apps.sort();
    match apps.len() {
        0 => anyhow::bail!("no built .app found under {}", products_dir.display()),
        1 => Ok(apps.remove(0)),
        _ => anyhow::bail!(
            "multiple .app bundles found; pass --scheme to disambiguate: {}",
            apps.iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn install_app(
    udid: &str,
    app: &Path,
    bundle_id: Option<String>,
    launch: bool,
) -> anyhow::Result<String> {
    validate_app_path(app)?;
    let bundle_id = match bundle_id {
        Some(bundle_id) => bundle_id,
        None => infer_bundle_id(app)?,
    };
    run_simctl(["install", udid, path_as_str(app)?])?;
    if launch {
        run_simctl(["launch", udid, &bundle_id])?;
    }
    Ok(bundle_id)
}

fn validate_app_path(app: &Path) -> anyhow::Result<()> {
    if !app.exists() {
        anyhow::bail!("app path does not exist: {}", app.display());
    }
    if !app.is_dir() || app.extension().and_then(|value| value.to_str()) != Some("app") {
        anyhow::bail!("app path must point to a .app bundle: {}", app.display());
    }
    Ok(())
}

fn infer_bundle_id(app: &Path) -> anyhow::Result<String> {
    let info_plist = app.join("Info.plist");
    if !info_plist.exists() {
        anyhow::bail!(
            "could not infer bundle id because Info.plist is missing: {}",
            info_plist.display()
        );
    }
    let output = ProcessCommand::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
        .arg(&info_plist)
        .output()
        .context("failed to run plutil")?;
    if !output.status.success() {
        anyhow::bail!(
            "could not infer bundle id: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let bundle_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if bundle_id.is_empty() {
        anyhow::bail!("could not infer bundle id because CFBundleIdentifier is empty");
    }
    Ok(bundle_id)
}

fn run_simctl<const N: usize>(args: [&str; N]) -> anyhow::Result<()> {
    let output = ProcessCommand::new("/usr/bin/xcrun")
        .arg("simctl")
        .args(args)
        .output()
        .context("failed to run xcrun simctl")?;
    if output.status.success() {
        return Ok(());
    }
    anyhow::bail!(
        "simctl {} failed: {}",
        args.first().copied().unwrap_or("command"),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn path_as_str(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn print_lease(
    slug: &str,
    device: &PoolDevice,
    ttl: Duration,
    host: &str,
    port: u16,
    json: bool,
) -> anyhow::Result<()> {
    if json {
        let output = LeaseOutput {
            slug,
            udid: &device.udid,
            device_name: &device.name,
            lease_expires_at: device.lease_expires_at,
            lease_expires_at_rfc3339: device.lease_expires_at.map(format_unix_timestamp),
            ttl_seconds: ttl.as_secs(),
            serve: ServeOutput {
                command: format!("simx serve --slug {slug} --host {host} --port {port}"),
                url: format!("http://{host}:{port}/{slug}"),
                stream: format!("ws://{host}:{port}/{slug}/stream"),
                stats: format!("http://{host}:{port}/{slug}/stats"),
            },
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", device.udid);
        if let Some(expires_at) = device.lease_expires_at {
            println!("lease expires at {}", format_unix_timestamp(expires_at));
        }
        println!("serve with: simx serve --slug {slug} --host {host} --port {port}");
    }
    Ok(())
}

fn format_unix_timestamp(timestamp: u64) -> String {
    humantime::format_rfc3339_seconds(UNIX_EPOCH + Duration::from_secs(timestamp)).to_string()
}

fn now_unix_seconds() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

fn parse_duration(raw: &str) -> Result<Duration, String> {
    humantime::parse_duration(raw).map_err(|error| error.to_string())
}

fn stop_process(pid: u32) {
    let _ = ProcessCommand::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status();
}

fn error_code(error: &anyhow::Error) -> &'static str {
    let message = format!("{error:#}");
    if message.contains("pool is not initialized") {
        "pool_not_initialized"
    } else if message.contains("timed out waiting") || message.contains("pool is full") {
        "pool_full"
    } else if message.contains("no active lease") || message.contains("no lease found") {
        "lease_not_found"
    } else if message.contains("doctor found failing") {
        "doctor_failed"
    } else if message.contains("ttl must")
        || message.contains("lease id must")
        || message.contains("app path")
        || message.contains(".xcodeproj")
        || message.contains("xcodebuild failed")
        || message.contains("could not infer bundle id")
        || message.contains("path is not valid UTF-8")
    {
        "invalid_argument"
    } else {
        "internal"
    }
}

fn exit_code(error: &anyhow::Error) -> i32 {
    match error_code(error) {
        "invalid_argument" => 2,
        "pool_not_initialized" | "pool_full" | "lease_not_found" => 3,
        "doctor_failed" => 4,
        _ => 1,
    }
}

fn doctor(state_path: &Path) -> DoctorOutput {
    let mut checks = Vec::new();
    checks.push(command_check(
        "xcode-select",
        "/usr/bin/xcode-select",
        &["-p"],
    ));
    checks.push(command_check(
        "xcrun",
        "/usr/bin/xcrun",
        &["simctl", "help"],
    ));
    checks.push(path_check(
        "CoreSimulator",
        Path::new("/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator"),
    ));
    let simulator_kit = developer_dir_for_doctor()
        .map(|developer_dir| {
            PathBuf::from(developer_dir)
                .join("Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit")
        })
        .unwrap_or_else(|| {
            PathBuf::from(
                "/Applications/Xcode.app/Contents/Developer/Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit",
            )
        });
    checks.push(path_check("SimulatorKit", &simulator_kit));
    checks.push(command_check(
        "iOS runtime",
        "/usr/bin/xcrun",
        &["simctl", "list", "runtimes", "-j"],
    ));
    checks.push(DoctorCheck {
        name: "state directory",
        ok: state_path.parent().is_some(),
        message: state_path
            .parent()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "state path has no parent".to_string()),
    });
    DoctorOutput {
        ok: checks.iter().all(|check| check.ok),
        checks,
    }
}

fn command_check(name: &'static str, command: &str, args: &[&str]) -> DoctorCheck {
    match ProcessCommand::new(command).args(args).output() {
        Ok(output) if output.status.success() => DoctorCheck {
            name,
            ok: true,
            message: String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("available")
                .to_string(),
        },
        Ok(output) => DoctorCheck {
            name,
            ok: false,
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        },
        Err(error) => DoctorCheck {
            name,
            ok: false,
            message: error.to_string(),
        },
    }
}

fn path_check(name: &'static str, path: &Path) -> DoctorCheck {
    DoctorCheck {
        name,
        ok: path.exists(),
        message: path.display().to_string(),
    }
}

fn developer_dir_for_doctor() -> Option<String> {
    let output = ProcessCommand::new("/usr/bin/xcode-select")
        .arg("-p")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn default_state_path() -> anyhow::Result<PathBuf> {
    let base = BaseDirs::new().context("could not resolve home directory")?;
    #[cfg(target_os = "macos")]
    {
        return Ok(base
            .home_dir()
            .join("Library/Application Support/simx/pool.json"));
    }

    #[cfg(not(target_os = "macos"))]
    {
        bail!("simx currently supports macOS only");
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn infer_bundle_id_reads_app_info_plist() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Example.app");
        fs::create_dir(&app).unwrap();
        fs::write(
            app.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>ai.boncasa.example</string>
</dict>
</plist>
"#,
        )
        .unwrap();

        assert_eq!(infer_bundle_id(&app).unwrap(), "ai.boncasa.example");
    }

    #[test]
    fn validate_app_path_requires_app_bundle_directory() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("Example.txt");
        fs::write(&file, "not an app").unwrap();

        assert!(validate_app_path(&file).is_err());
    }

    #[test]
    fn validate_xcode_project_requires_xcodeproj_directory() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("Example.xcodeproj");
        fs::create_dir(&project).unwrap();
        let file = temp.path().join("Example.txt");
        fs::write(&file, "not a project").unwrap();

        assert!(validate_xcode_project(&project).is_ok());
        assert!(validate_xcode_project(&file).is_err());
    }

    #[test]
    fn default_scheme_uses_project_name() {
        assert_eq!(
            default_scheme_for_project(Path::new("Lumi.xcodeproj")),
            "Lumi"
        );
    }

    #[test]
    fn safe_path_component_replaces_unsafe_characters() {
        assert_eq!(safe_path_component("agent/one two"), "agent-one-two");
    }
}
