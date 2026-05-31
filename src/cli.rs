use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use directories::BaseDirs;

use crate::pool::{PoolConfig, PoolService};
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
    /// Shut down and delete all devices in the simx pool.
    Clean,
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
                println!("{} {} {}", device.name, device.udid, owner);
            }
        }
        Command::Lease {
            slug,
            wait_timeout,
            serve: should_serve,
            host,
            port,
            quality,
            fps,
            idle_timeout,
            new: _new,
        } => {
            let device = service.lease(&mut simctl, &slug, wait_timeout)?;
            println!("{}", device.udid);
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
