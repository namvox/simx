use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use directories::BaseDirs;
use serde::Serialize;

use crate::pool::{LeaseOptions, PoolConfig, PoolDevice, PoolService};
use crate::simctl::XcrunSimctl;
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
    } else if message.contains("ttl must") || message.contains("lease id must") {
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
