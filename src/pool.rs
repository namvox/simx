use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

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
        self.with_locked_state(|file| read_state(file))
    }

    pub fn lease<S: Simctl>(
        &mut self,
        simctl: &mut S,
        lease_id: &str,
        wait_timeout: Duration,
    ) -> anyhow::Result<PoolDevice> {
        validate_lease_id(lease_id)?;
        let deadline = Instant::now() + wait_timeout;

        loop {
            match self.try_lease_once(lease_id)? {
                LeaseAttempt::Leased(device) => {
                    if let Err(error) = simctl.boot_if_needed(&device.udid) {
                        self.clear_lease_if_matches(lease_id, &device.udid)?;
                        return Err(error)
                            .with_context(|| format!("failed to boot {}", device.udid));
                    }
                    return Ok(device);
                }
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

    pub fn release(&mut self, lease_id: &str) -> anyhow::Result<bool> {
        validate_lease_id(lease_id)?;
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            let mut released = false;
            for device in &mut state.devices {
                if device.lease_id.as_deref() == Some(lease_id) {
                    device.lease_id = None;
                    released = true;
                }
            }
            if released {
                write_state(file, &state)?;
            }
            Ok(released)
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

    fn try_lease_once(&mut self, lease_id: &str) -> anyhow::Result<LeaseAttempt> {
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            if let Some(device) = state
                .devices
                .iter()
                .find(|device| device.lease_id.as_deref() == Some(lease_id))
            {
                return Ok(LeaseAttempt::Leased(device.clone()));
            }

            if let Some(device) = state
                .devices
                .iter_mut()
                .find(|device| device.lease_id.is_none())
            {
                device.lease_id = Some(lease_id.to_string());
                let leased = device.clone();
                write_state(file, &state)?;
                return Ok(LeaseAttempt::Leased(leased));
            }

            Ok(LeaseAttempt::Full)
        })
    }

    fn clear_lease_if_matches(&mut self, lease_id: &str, udid: &str) -> anyhow::Result<()> {
        self.with_locked_state(|file| {
            let mut state = read_state(file)?;
            let mut changed = false;
            for device in &mut state.devices {
                if device.udid == udid && device.lease_id.as_deref() == Some(lease_id) {
                    device.lease_id = None;
                    changed = true;
                }
            }
            if changed {
                write_state(file, &state)?;
            }
            Ok(())
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
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("failed to open lock {}", lock_path.display()))?;
        lock.lock_exclusive()
            .with_context(|| format!("failed to lock {}", lock_path.display()))?;

        let mut state_file = OpenOptions::new()
            .create(true)
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
