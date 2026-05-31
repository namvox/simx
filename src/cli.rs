use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::Context;
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
    Status,
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
}

#[derive(Debug, Serialize)]
struct LeaseOutput<'a> {
    slug: &'a str,
    udid: &'a str,
    device_name: &'a str,
    lease_expires_at: Option<u64>,
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
        Command::Status => {
            let state = service.status()?;
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
                serve(ServeConfig {
                    host,
                    port,
                    quality,
                    fps,
                    idle_timeout,
                    slug,
                    udid: device.udid,
                    state_path,
                    stats: Arc::new(Mutex::new(StreamStats::default())),
                })?;
            }
        }
        Command::Release { slug } => {
            if service.release(&slug)? {
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
            let devices = service.clean(&mut simctl)?;
            println!("removed {} simulator(s)", devices.len());
            for device in devices {
                println!("{} {}", device.name, device.udid);
            }
        }
    }

    Ok(())
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
            ttl_seconds: ttl.as_secs(),
            serve: ServeOutput {
                command: format!("simx lease --slug {slug} --serve --host {host} --port {port}"),
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
        println!("serve with: simx lease --slug {slug} --serve --host {host} --port {port}");
    }
    Ok(())
}

fn format_unix_timestamp(timestamp: u64) -> String {
    humantime::format_rfc3339_seconds(UNIX_EPOCH + Duration::from_secs(timestamp)).to_string()
}

fn parse_duration(raw: &str) -> Result<Duration, String> {
    humantime::parse_duration(raw).map_err(|error| error.to_string())
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
