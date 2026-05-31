use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use simx::pool::{LeaseOptions, PoolConfig, PoolService};
use simx::simctl::{DeviceSpec, RuntimeSpec, Simctl};
use tempfile::TempDir;

#[derive(Default)]
struct FakeSimctl {
    created: Vec<(String, String, String)>,
    booted: HashSet<String>,
    shutdown: Vec<String>,
    deleted: Vec<String>,
    devices: HashMap<String, String>,
}

impl FakeSimctl {
    fn with_pool_devices(count: usize) -> Self {
        let mut fake = Self::default();
        for index in 1..=count {
            fake.devices
                .insert(format!("simx-pool-{index:03}"), format!("UDID-{index}"));
        }
        fake
    }
}

impl Simctl for FakeSimctl {
    fn latest_iphone_device_type(&self) -> anyhow::Result<DeviceSpec> {
        Ok(DeviceSpec {
            id: "com.apple.CoreSimulator.SimDeviceType.iPhone-16-Pro".to_string(),
            name: "iPhone 16 Pro".to_string(),
        })
    }

    fn latest_ios_runtime(&self) -> anyhow::Result<RuntimeSpec> {
        Ok(RuntimeSpec {
            id: "com.apple.CoreSimulator.SimRuntime.iOS-18-5".to_string(),
            name: "iOS 18.5".to_string(),
        })
    }

    fn find_device_by_name(&self, name: &str) -> anyhow::Result<Option<String>> {
        Ok(self.devices.get(name).cloned())
    }

    fn create_device(
        &mut self,
        name: &str,
        device_type_id: &str,
        runtime_id: &str,
    ) -> anyhow::Result<String> {
        let udid = format!("UDID-{}", self.devices.len() + 1);
        self.created.push((
            name.to_string(),
            device_type_id.to_string(),
            runtime_id.to_string(),
        ));
        self.devices.insert(name.to_string(), udid.clone());
        Ok(udid)
    }

    fn boot_if_needed(&mut self, udid: &str) -> anyhow::Result<()> {
        self.booted.insert(udid.to_string());
        Ok(())
    }

    fn shutdown_if_needed(&mut self, udid: &str) -> anyhow::Result<()> {
        self.shutdown.push(udid.to_string());
        Ok(())
    }

    fn delete_device(&mut self, udid: &str) -> anyhow::Result<()> {
        self.deleted.push(udid.to_string());
        Ok(())
    }
}

fn service_path(temp: &TempDir) -> PathBuf {
    temp.path().join("pool.json")
}

fn lease_options(ttl: Duration) -> LeaseOptions {
    LeaseOptions {
        wait_timeout: Duration::from_millis(1),
        ttl,
    }
}

fn short_lease_options() -> LeaseOptions {
    lease_options(Duration::from_secs(30))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn init_creates_fixed_pool_devices() {
    let temp = TempDir::new().unwrap();
    let mut simctl = FakeSimctl::default();
    let mut service = PoolService::new(service_path(&temp));

    let state = service
        .init(
            &mut simctl,
            PoolConfig {
                size: 2,
                device_type: None,
                runtime: None,
            },
        )
        .unwrap();

    assert_eq!(state.devices.len(), 2);
    assert_eq!(state.devices[0].name, "simx-pool-001");
    assert_eq!(state.devices[1].name, "simx-pool-002");
    assert_eq!(simctl.created.len(), 2);
    assert_eq!(
        simctl.created[0].1,
        "com.apple.CoreSimulator.SimDeviceType.iPhone-16-Pro"
    );
    assert_eq!(
        simctl.created[0].2,
        "com.apple.CoreSimulator.SimRuntime.iOS-18-5"
    );
}

#[test]
fn lease_reuses_existing_lease_and_boots_allocated_device() {
    let temp = TempDir::new().unwrap();
    let mut simctl = FakeSimctl::with_pool_devices(2);
    let mut service = PoolService::new(service_path(&temp));
    service
        .init(
            &mut simctl,
            PoolConfig {
                size: 2,
                device_type: None,
                runtime: None,
            },
        )
        .unwrap();

    let first = service
        .lease(&mut simctl, "agent-a", short_lease_options())
        .unwrap();
    let second = service
        .lease(&mut simctl, "agent-a", short_lease_options())
        .unwrap();

    assert_eq!(first.udid, second.udid);
    assert_eq!(first.lease_id.as_deref(), Some("agent-a"));
    assert!(simctl.booted.contains(&first.udid));
}

#[test]
fn same_slug_lease_extends_ttl_and_returns_same_device() {
    let temp = TempDir::new().unwrap();
    let mut simctl = FakeSimctl::with_pool_devices(1);
    let mut service = PoolService::new(service_path(&temp));
    service
        .init(
            &mut simctl,
            PoolConfig {
                size: 1,
                device_type: None,
                runtime: None,
            },
        )
        .unwrap();

    let first = service
        .lease(
            &mut simctl,
            "agent-a",
            lease_options(Duration::from_secs(1)),
        )
        .unwrap();
    thread::sleep(Duration::from_secs(1));
    let second = service
        .lease(
            &mut simctl,
            "agent-a",
            lease_options(Duration::from_secs(30)),
        )
        .unwrap();

    assert_eq!(first.udid, second.udid);
    assert_eq!(second.lease_id.as_deref(), Some("agent-a"));
    assert!(second.lease_expires_at.unwrap() >= unix_now() + 20);
}

#[test]
fn different_leases_get_different_devices_and_full_pool_times_out() {
    let temp = TempDir::new().unwrap();
    let mut simctl = FakeSimctl::with_pool_devices(2);
    let mut service = PoolService::new(service_path(&temp));
    service
        .init(
            &mut simctl,
            PoolConfig {
                size: 2,
                device_type: None,
                runtime: None,
            },
        )
        .unwrap();

    let first = service
        .lease(&mut simctl, "agent-a", short_lease_options())
        .unwrap();
    let second = service
        .lease(&mut simctl, "agent-b", short_lease_options())
        .unwrap();
    let third = service.lease(&mut simctl, "agent-c", short_lease_options());

    assert_ne!(first.udid, second.udid);
    assert!(third.is_err());
}

#[test]
fn expired_lease_is_reclaimed_for_different_slug() {
    let temp = TempDir::new().unwrap();
    let mut simctl = FakeSimctl::with_pool_devices(1);
    let mut service = PoolService::new(service_path(&temp));
    service
        .init(
            &mut simctl,
            PoolConfig {
                size: 1,
                device_type: None,
                runtime: None,
            },
        )
        .unwrap();

    let first = service
        .lease(
            &mut simctl,
            "agent-a",
            lease_options(Duration::from_secs(1)),
        )
        .unwrap();
    thread::sleep(Duration::from_secs(2));
    let second = service
        .lease(&mut simctl, "agent-b", short_lease_options())
        .unwrap();

    assert_eq!(first.udid, second.udid);
    assert_eq!(second.lease_id.as_deref(), Some("agent-b"));
}

#[test]
fn status_reaps_expired_leases() {
    let temp = TempDir::new().unwrap();
    let mut simctl = FakeSimctl::with_pool_devices(1);
    let mut service = PoolService::new(service_path(&temp));
    service
        .init(
            &mut simctl,
            PoolConfig {
                size: 1,
                device_type: None,
                runtime: None,
            },
        )
        .unwrap();

    service
        .lease(
            &mut simctl,
            "agent-a",
            lease_options(Duration::from_secs(1)),
        )
        .unwrap();
    thread::sleep(Duration::from_secs(2));

    let state = service.status().unwrap();
    assert_eq!(state.devices[0].lease_id, None);
    assert_eq!(state.devices[0].lease_expires_at, None);
}

#[test]
fn renew_extends_active_lease_and_fails_after_expiry() {
    let temp = TempDir::new().unwrap();
    let mut simctl = FakeSimctl::with_pool_devices(1);
    let mut service = PoolService::new(service_path(&temp));
    service
        .init(
            &mut simctl,
            PoolConfig {
                size: 1,
                device_type: None,
                runtime: None,
            },
        )
        .unwrap();

    let leased = service
        .lease(
            &mut simctl,
            "agent-a",
            lease_options(Duration::from_secs(30)),
        )
        .unwrap();
    let renewed = service.renew("agent-a", Duration::from_secs(60)).unwrap();
    assert_eq!(leased.udid, renewed.udid);
    assert!(renewed.lease_expires_at.unwrap() >= unix_now() + 50);

    assert!(service.renew("missing", Duration::from_secs(60)).is_err());

    service.release("agent-a").unwrap();
    service
        .lease(
            &mut simctl,
            "agent-a",
            lease_options(Duration::from_secs(1)),
        )
        .unwrap();
    thread::sleep(Duration::from_secs(2));
    assert!(service.renew("agent-a", Duration::from_secs(60)).is_err());
}

#[test]
fn release_keeps_device_available_without_shutdown() {
    let temp = TempDir::new().unwrap();
    let mut simctl = FakeSimctl::with_pool_devices(1);
    let mut service = PoolService::new(service_path(&temp));
    service
        .init(
            &mut simctl,
            PoolConfig {
                size: 1,
                device_type: None,
                runtime: None,
            },
        )
        .unwrap();

    let first = service
        .lease(&mut simctl, "agent-a", short_lease_options())
        .unwrap();
    service.release("agent-a").unwrap();
    let second = service
        .lease(&mut simctl, "agent-b", short_lease_options())
        .unwrap();

    assert_eq!(first.udid, second.udid);
    assert!(simctl.booted.contains(&first.udid));
}

#[test]
fn concurrent_leases_cannot_claim_the_same_device() {
    let temp = TempDir::new().unwrap();
    let state_path = service_path(&temp);
    let mut simctl = FakeSimctl::with_pool_devices(1);
    let mut service = PoolService::new(state_path.clone());
    service
        .init(
            &mut simctl,
            PoolConfig {
                size: 1,
                device_type: None,
                runtime: None,
            },
        )
        .unwrap();

    let first_path = state_path.clone();
    let second_path = state_path;
    let first = thread::spawn(move || {
        let mut simctl = FakeSimctl::with_pool_devices(1);
        PoolService::new(first_path).lease(
            &mut simctl,
            "agent-a",
            LeaseOptions {
                wait_timeout: Duration::from_millis(10),
                ttl: Duration::from_secs(30),
            },
        )
    });
    let second = thread::spawn(move || {
        let mut simctl = FakeSimctl::with_pool_devices(1);
        PoolService::new(second_path).lease(
            &mut simctl,
            "agent-b",
            LeaseOptions {
                wait_timeout: Duration::from_millis(10),
                ttl: Duration::from_secs(30),
            },
        )
    });

    let results = vec![first.join().unwrap(), second.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
}

#[test]
fn clean_shuts_down_deletes_devices_and_removes_state() {
    let temp = TempDir::new().unwrap();
    let mut simctl = FakeSimctl::with_pool_devices(2);
    let mut service = PoolService::new(service_path(&temp));
    service
        .init(
            &mut simctl,
            PoolConfig {
                size: 2,
                device_type: None,
                runtime: None,
            },
        )
        .unwrap();

    let cleaned = service.clean(&mut simctl).unwrap();

    assert_eq!(cleaned.len(), 2);
    assert_eq!(simctl.shutdown, vec!["UDID-1", "UDID-2"]);
    assert_eq!(simctl.deleted, vec!["UDID-1", "UDID-2"]);
    assert!(service.status().is_err());
}
