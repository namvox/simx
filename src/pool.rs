use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::simctl::Simctl;

const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub size: usize,
    pub device_type: Option<String>,
    pub runtime: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PoolState {
    pub version: u32,
    pub size: usize,
    pub device_type_id: String,
    pub runtime_id: String,
    pub devices: Vec<PoolDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PoolDevice {
    pub name: String,
    pub udid: String,
    pub lease_id: Option<String>,
    #[serde(default)]
    pub lease_expires_at: Option<u64>,
    #[serde(default)]
    pub serve_pid: Option<u32>,
    #[serde(default)]
    pub serve_host: Option<String>,
    #[serde(default)]
    pub serve_port: Option<u16>,
}

#[derive(Debug, Clone, Copy)]
pub struct LeaseOptions {
    pub wait_timeout: Duration,
    pub ttl: Duration,
}

pub struct PoolService {
    state_path: PathBuf,
}

impl PoolService {
    pub fn new(state_path: PathBuf) -> Self {
        Self { state_path }
    }

    pub fn init<S: Simctl>(
        &mut self,
        simctl: &mut S,
        config: PoolConfig,
    ) -> anyhow::Result<PoolState> {
        if config.size == 0 {
            bail!("pool size must be greater than zero");
        }

        self.with_locked_state(|file| {
            let device_type_id = match config.device_type {
                Some(id) => id,
                None => simctl.latest_iphone_device_type()?.id,
            };
            let runtime_id = match config.runtime {
                Some(id) => id,
                None => simctl.latest_ios_runtime()?.id,
            };

            let mut devices = Vec::with_capacity(config.size);
            for index in 1..=config.size {
                let name = format!("simx-pool-{index:03}");
                let udid = match simctl.find_device_by_name(&name)? {
                    Some(udid) => udid,
                    None => simctl.create_device(&name, &device_type_id, &runtime_id)?,
                };
                devices.push(PoolDevice {
                    name,
                    udid,
                    lease_id: None,
                    lease_expires_at: None,
                    serve_pid: None,
                    serve_host: None,
                    serve_port: None,
                });
            }

            let state = PoolState {
                version: STATE_VERSION,
                size: config.size,
                device_type_id,
                runtime_id,
                devices,
            };
            write_state(file, &state)?;
            Ok(state.clone())
        })
    }

    pub fn status(&mut self) -> anyhow::Result<PoolState> {
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            if reap_expired_leases(&mut state, now_unix_seconds()?) {
                write_state(file, &state)?;
            }
            Ok(state)
        })
    }

    pub fn status_with_simctl<S: Simctl>(&mut self, simctl: &mut S) -> anyhow::Result<PoolState> {
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            let mut changed = reap_expired_leases(&mut state, now_unix_seconds()?);
            changed |= reap_non_booted_unserved_leases(&mut state, simctl)?;
            if changed {
                write_state(file, &state)?;
            }
            Ok(state)
        })
    }

    pub fn lease<S: Simctl>(
        &mut self,
        simctl: &mut S,
        lease_id: &str,
        options: LeaseOptions,
    ) -> anyhow::Result<PoolDevice> {
        validate_lease_id(lease_id)?;
        validate_ttl(options.ttl)?;
        let deadline = Instant::now() + options.wait_timeout;

        loop {
            match self.try_lease_once(simctl, lease_id, options.ttl)? {
                LeaseAttempt::Leased(device) => return Ok(device),
                LeaseAttempt::Full => {
                    let now = Instant::now();
                    if now >= deadline {
                        bail!("simulator pool is full; timed out waiting for an idle device");
                    }
                    thread::sleep((deadline - now).min(Duration::from_millis(250)));
                }
            }
        }
    }

    pub fn release(&mut self, lease_id: &str) -> anyhow::Result<ReleaseResult> {
        validate_lease_id(lease_id)?;
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            let mut released = false;
            let mut serve_processes = Vec::new();
            for device in &mut state.devices {
                if device.lease_id.as_deref() == Some(lease_id) {
                    if let Some(pid) = device.serve_pid.take() {
                        serve_processes.push(ServeProcess {
                            pid,
                            slug: lease_id.to_string(),
                            udid: device.udid.clone(),
                            host: device.serve_host.take(),
                            port: device.serve_port.take(),
                        });
                    }
                    device.lease_id = None;
                    device.lease_expires_at = None;
                    released = true;
                }
            }
            if released {
                write_state(file, &state)?;
            }
            Ok(ReleaseResult {
                released,
                serve_processes,
            })
        })
    }

    pub fn renew(&mut self, lease_id: &str, ttl: Duration) -> anyhow::Result<PoolDevice> {
        validate_lease_id(lease_id)?;
        validate_ttl(ttl)?;
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            let now = now_unix_seconds()?;
            let reaped = reap_expired_leases_at(&mut state, now);
            let expires_at = expires_at(now, ttl)?;
            if let Some(device) = state
                .devices
                .iter_mut()
                .find(|device| device.lease_id.as_deref() == Some(lease_id))
            {
                device.lease_expires_at = Some(expires_at);
                let renewed = device.clone();
                write_state(file, &state)?;
                return Ok(renewed);
            }
            if reaped {
                write_state(file, &state)?;
            }
            bail!("no active lease found for {lease_id}");
        })
    }

    pub fn clean<S: Simctl>(&mut self, simctl: &mut S) -> anyhow::Result<Vec<PoolDevice>> {
        self.with_locked_state(|file| {
            let state = read_state(file)?;
            let devices = state.devices.clone();
            for device in &devices {
                simctl
                    .shutdown_if_needed(&device.udid)
                    .with_context(|| format!("failed to shut down {}", device.udid))?;
                simctl
                    .delete_device(&device.udid)
                    .with_context(|| format!("failed to delete {}", device.udid))?;
            }
            file.seek(SeekFrom::Start(0))?;
            file.set_len(0)?;
            file.sync_all()?;
            let _ = fs::remove_file(&self.state_path);
            Ok(devices)
        })
    }

    pub fn active_lease(&mut self, lease_id: &str) -> anyhow::Result<PoolDevice> {
        validate_lease_id(lease_id)?;
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            let now = now_unix_seconds()?;
            let reaped = reap_expired_leases_at(&mut state, now);
            let device = state
                .devices
                .iter()
                .find(|device| device.lease_id.as_deref() == Some(lease_id))
                .cloned();
            if reaped {
                write_state(file, &state)?;
            }
            device.with_context(|| format!("no active lease found for {lease_id}"))
        })
    }

    pub fn active_lease_matches_udid(
        &mut self,
        lease_id: &str,
        udid: &str,
    ) -> anyhow::Result<bool> {
        validate_lease_id(lease_id)?;
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            let now = now_unix_seconds()?;
            let reaped = reap_expired_leases_at(&mut state, now);
            let matches = state
                .devices
                .iter()
                .any(|device| device.lease_id.as_deref() == Some(lease_id) && device.udid == udid);
            if reaped {
                write_state(file, &state)?;
            }
            Ok(matches)
        })
    }

    pub fn register_serve(
        &mut self,
        lease_id: &str,
        udid: &str,
        pid: u32,
        host: &str,
        port: u16,
    ) -> anyhow::Result<()> {
        validate_lease_id(lease_id)?;
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            let now = now_unix_seconds()?;
            reap_expired_leases_at(&mut state, now);
            let device = state
                .devices
                .iter_mut()
                .find(|device| device.udid == udid && device.lease_id.as_deref() == Some(lease_id))
                .with_context(|| format!("no active lease found for {lease_id}"))?;
            device.serve_pid = Some(pid);
            device.serve_host = Some(host.to_string());
            device.serve_port = Some(port);
            write_state(file, &state)
        })
    }

    pub fn clear_serve(&mut self, lease_id: &str, pid: u32) -> anyhow::Result<()> {
        validate_lease_id(lease_id)?;
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            let mut changed = false;
            for device in &mut state.devices {
                if device.lease_id.as_deref() == Some(lease_id) && device.serve_pid == Some(pid) {
                    device.serve_pid = None;
                    device.serve_host = None;
                    device.serve_port = None;
                    changed = true;
                }
            }
            if changed {
                write_state(file, &state)?;
            }
            Ok(())
        })
    }

    fn try_lease_once<S: Simctl>(
        &mut self,
        simctl: &mut S,
        lease_id: &str,
        ttl: Duration,
    ) -> anyhow::Result<LeaseAttempt> {
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            let now = now_unix_seconds()?;
            let mut changed = reap_expired_leases_at(&mut state, now);
            changed |= reap_non_booted_unserved_leases(&mut state, simctl)?;
            let lease_expires_at = expires_at(now, ttl)?;
            if let Some(device) = state
                .devices
                .iter_mut()
                .find(|device| device.lease_id.as_deref() == Some(lease_id))
            {
                device.lease_expires_at = Some(lease_expires_at);
                let leased = device.clone();
                if let Err(error) = simctl.boot_if_needed(&leased.udid) {
                    clear_device_claim(device);
                    write_state(file, &state)?;
                    return Err(error).with_context(|| format!("failed to boot {}", leased.udid));
                }
                write_state(file, &state)?;
                return Ok(LeaseAttempt::Leased(leased));
            }

            if let Some(device) = state
                .devices
                .iter_mut()
                .find(|device| device.lease_id.is_none())
            {
                device.lease_id = Some(lease_id.to_string());
                device.lease_expires_at = Some(lease_expires_at);
                let leased = device.clone();
                if let Err(error) = simctl.boot_if_needed(&leased.udid) {
                    clear_device_claim(device);
                    write_state(file, &state)?;
                    return Err(error).with_context(|| format!("failed to boot {}", leased.udid));
                }
                write_state(file, &state)?;
                return Ok(LeaseAttempt::Leased(leased));
            }

            if changed {
                write_state(file, &state)?;
            }
            Ok(LeaseAttempt::Full)
        })
    }

    fn with_locked_state<T>(
        &self,
        operation: impl FnOnce(&mut File) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let lock_path = lock_path_for(&self.state_path);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("failed to open lock {}", lock_path.display()))?;
        lock.lock_exclusive()
            .with_context(|| format!("failed to lock {}", lock_path.display()))?;

        let mut state_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.state_path)
            .with_context(|| format!("failed to open state {}", self.state_path.display()))?;
        let result = operation(&mut state_file);
        let unlock_result = lock.unlock();

        match (result, unlock_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error).context("failed to unlock pool state"),
        }
    }
}

fn clear_device_claim(device: &mut PoolDevice) {
    device.lease_id = None;
    device.lease_expires_at = None;
    device.serve_pid = None;
    device.serve_host = None;
    device.serve_port = None;
}

enum LeaseAttempt {
    Leased(PoolDevice),
    Full,
}

fn validate_lease_id(lease_id: &str) -> anyhow::Result<()> {
    if lease_id.trim().is_empty() {
        bail!("lease id must not be empty");
    }
    Ok(())
}

fn validate_ttl(ttl: Duration) -> anyhow::Result<()> {
    if ttl.is_zero() {
        bail!("ttl must be greater than zero");
    }
    Ok(())
}

fn reap_expired_leases(state: &mut PoolState, now: u64) -> bool {
    reap_expired_leases_at(state, now)
}

fn reap_expired_leases_at(state: &mut PoolState, now: u64) -> bool {
    let mut changed = false;
    for device in &mut state.devices {
        if device.lease_id.is_some()
            && device
                .lease_expires_at
                .is_some_and(|expires| expires <= now)
        {
            clear_device_claim(device);
            changed = true;
        }
    }
    changed
}

fn reap_non_booted_unserved_leases<S: Simctl>(
    state: &mut PoolState,
    simctl: &mut S,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for device in &mut state.devices {
        if device.lease_id.is_some() && device.serve_pid.is_none() {
            let Some(simulator_state) = simctl.device_state(&device.udid)? else {
                continue;
            };
            if !simulator_state.eq_ignore_ascii_case("Booted") {
                clear_device_claim(device);
                changed = true;
            }
        }
    }
    Ok(changed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseResult {
    pub released: bool,
    pub serve_processes: Vec<ServeProcess>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeProcess {
    pub pid: u32,
    pub slug: String,
    pub udid: String,
    pub host: Option<String>,
    pub port: Option<u16>,
}

fn now_unix_seconds() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

fn expires_at(now: u64, ttl: Duration) -> anyhow::Result<u64> {
    now.checked_add(ttl.as_secs())
        .context("lease expiry timestamp overflowed")
}

fn read_state(file: &mut File) -> anyhow::Result<PoolState> {
    file.seek(SeekFrom::Start(0))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)?;
    if raw.trim().is_empty() {
        bail!("simx pool is not initialized; run `simx init --size <N>` first");
    }
    serde_json::from_str(&raw).context("failed to parse pool state")
}

fn write_state(file: &mut File, state: &PoolState) -> anyhow::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    serde_json::to_writer_pretty(&mut *file, state)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn lock_path_for(state_path: &Path) -> PathBuf {
    let file_name = state_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("pool.json");
    state_path.with_file_name(format!("{file_name}.lock"))
}
