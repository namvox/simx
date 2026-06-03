use std::collections::HashMap;
use std::process::Command;

use anyhow::{bail, Context};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSpec {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSpec {
    pub id: String,
    pub name: String,
}

pub trait Simctl {
    fn latest_iphone_device_type(&self) -> anyhow::Result<DeviceSpec>;
    fn latest_ios_runtime(&self) -> anyhow::Result<RuntimeSpec>;
    fn find_device_by_name(&self, name: &str) -> anyhow::Result<Option<String>>;
    fn device_state(&self, udid: &str) -> anyhow::Result<Option<String>>;
    fn create_device(
        &mut self,
        name: &str,
        device_type_id: &str,
        runtime_id: &str,
    ) -> anyhow::Result<String>;
    fn boot_if_needed(&mut self, udid: &str) -> anyhow::Result<()>;
    fn shutdown_if_needed(&mut self, udid: &str) -> anyhow::Result<()>;
    fn delete_device(&mut self, udid: &str) -> anyhow::Result<()>;
}

#[derive(Default)]
pub struct XcrunSimctl;

impl Simctl for XcrunSimctl {
    fn latest_iphone_device_type(&self) -> anyhow::Result<DeviceSpec> {
        let list = run_json(["simctl", "list", "devicetypes", "-j"])?;
        let parsed: DeviceTypesResponse =
            serde_json::from_str(&list).context("failed to parse simctl devicetypes")?;
        parsed
            .devicetypes
            .into_iter()
            .filter(|device| device.name.starts_with("iPhone"))
            .max_by_key(|device| ranking_key(&device.name))
            .map(|device| DeviceSpec {
                id: device.identifier,
                name: device.name,
            })
            .context("no available iPhone simulator device type found")
    }

    fn latest_ios_runtime(&self) -> anyhow::Result<RuntimeSpec> {
        let list = run_json(["simctl", "list", "runtimes", "-j"])?;
        let parsed: RuntimesResponse =
            serde_json::from_str(&list).context("failed to parse simctl runtimes")?;
        parsed
            .runtimes
            .into_iter()
            .filter(|runtime| runtime.platform == "iOS" && runtime.is_available())
            .max_by_key(|runtime| version_key(runtime.version.as_deref().unwrap_or(&runtime.name)))
            .map(|runtime| RuntimeSpec {
                id: runtime.identifier,
                name: runtime.name,
            })
            .context("no available iOS simulator runtime found")
    }

    fn find_device_by_name(&self, name: &str) -> anyhow::Result<Option<String>> {
        let list = run_json(["simctl", "list", "devices", "-j"])?;
        let parsed: DevicesResponse =
            serde_json::from_str(&list).context("failed to parse simctl devices")?;
        for devices in parsed.devices.into_values() {
            for device in devices {
                if device.name == name {
                    return Ok(Some(device.udid));
                }
            }
        }
        Ok(None)
    }

    fn device_state(&self, udid: &str) -> anyhow::Result<Option<String>> {
        let list = run_json(["simctl", "list", "devices", "-j"])?;
        let parsed: DevicesResponse =
            serde_json::from_str(&list).context("failed to parse simctl devices")?;
        for devices in parsed.devices.into_values() {
            for device in devices {
                if device.udid == udid {
                    return Ok(device.state);
                }
            }
        }
        Ok(None)
    }

    fn create_device(
        &mut self,
        name: &str,
        device_type_id: &str,
        runtime_id: &str,
    ) -> anyhow::Result<String> {
        let output = Command::new("/usr/bin/xcrun")
            .args(["simctl", "create", name, device_type_id, runtime_id])
            .output()
            .context("failed to run xcrun simctl create")?;
        if !output.status.success() {
            bail!(
                "simctl create failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn boot_if_needed(&mut self, udid: &str) -> anyhow::Result<()> {
        let output = Command::new("/usr/bin/xcrun")
            .args(["simctl", "boot", udid])
            .output()
            .context("failed to run xcrun simctl boot")?;
        if output.status.success() || stderr_contains_already_booted(&output.stderr) {
            return Ok(());
        }
        bail!(
            "simctl boot failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }

    fn shutdown_if_needed(&mut self, udid: &str) -> anyhow::Result<()> {
        let output = Command::new("/usr/bin/xcrun")
            .args(["simctl", "shutdown", udid])
            .output()
            .context("failed to run xcrun simctl shutdown")?;
        if output.status.success() || stderr_contains_already_shutdown(&output.stderr) {
            return Ok(());
        }
        bail!(
            "simctl shutdown failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }

    fn delete_device(&mut self, udid: &str) -> anyhow::Result<()> {
        let output = Command::new("/usr/bin/xcrun")
            .args(["simctl", "delete", udid])
            .output()
            .context("failed to run xcrun simctl delete")?;
        if output.status.success() {
            return Ok(());
        }
        bail!(
            "simctl delete failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn run_json<const N: usize>(args: [&str; N]) -> anyhow::Result<String> {
    let output = Command::new("/usr/bin/xcrun")
        .args(args)
        .output()
        .context("failed to run xcrun")?;
    if !output.status.success() {
        bail!(
            "xcrun failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout).context("xcrun output was not UTF-8")?)
}

fn stderr_contains_already_booted(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr);
    stderr.contains("Unable to boot device in current state: Booted")
        || stderr.contains("current state: Booted")
}

fn stderr_contains_already_shutdown(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr);
    stderr.contains("Unable to shutdown device in current state: Shutdown")
        || stderr.contains("current state: Shutdown")
        || stderr.contains("is not booted")
}

fn ranking_key(name: &str) -> (Vec<u32>, String) {
    (version_key(name), name.to_string())
}

fn version_key(value: &str) -> Vec<u32> {
    let mut current = String::new();
    let mut parts = Vec::new();
    for ch in value.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(part) = current.parse() {
                parts.push(part);
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        if let Ok(part) = current.parse() {
            parts.push(part);
        }
    }
    parts
}

#[derive(Debug, Deserialize)]
struct DeviceTypesResponse {
    devicetypes: Vec<DeviceTypeEntry>,
}

#[derive(Debug, Deserialize)]
struct DeviceTypeEntry {
    name: String,
    identifier: String,
}

#[derive(Debug, Deserialize)]
struct RuntimesResponse {
    runtimes: Vec<RuntimeEntry>,
}

#[derive(Debug, Deserialize)]
struct RuntimeEntry {
    name: String,
    identifier: String,
    platform: String,
    version: Option<String>,
    #[serde(default)]
    #[serde(rename = "isAvailable")]
    is_available: Option<bool>,
    #[serde(default)]
    availability: Option<String>,
}

impl RuntimeEntry {
    fn is_available(&self) -> bool {
        self.is_available.unwrap_or(true)
            && self
                .availability
                .as_deref()
                .map(|availability| availability == "(available)")
                .unwrap_or(true)
    }
}

#[derive(Debug, Deserialize)]
struct DevicesResponse {
    devices: HashMap<String, Vec<DeviceEntry>>,
}

#[derive(Debug, Deserialize)]
struct DeviceEntry {
    name: String,
    udid: String,
    state: Option<String>,
}
