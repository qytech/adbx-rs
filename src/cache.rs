use crate::adb::{self, DeviceInfo};
use crate::store;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL_SECS: u64 = 5;
const TIMEOUT_MS: u64 = 1500;

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    devices_updated_at: u64,
    devices: Vec<CachedDevice>,
    packages: BTreeMap<String, CachedPackages>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDevice {
    pub serial: String,
    pub model: String,
    pub state: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CachedPackages {
    updated_at: u64,
    items: Vec<String>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_path() -> Option<PathBuf> {
    let dir = match std::env::var("ADBX_CACHE_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
        _ => dirs::cache_dir()?.join("adbx"),
    };
    let _ = fs::create_dir_all(&dir);
    Some(dir.join("completion_cache.json"))
}

fn load_cache() -> CacheFile {
    let Some(path) = cache_path() else {
        return CacheFile::default();
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_cache(cache: &CacheFile) {
    if let Some(path) = cache_path() {
        if let Ok(raw) = serde_json::to_string(cache) {
            let _ = store::atomic_write(&path, &raw);
        }
    }
}

pub fn get_devices_fast() -> Vec<CachedDevice> {
    let mut cache = load_cache();
    let now = now_secs();
    if now.saturating_sub(cache.devices_updated_at) <= TTL_SECS && !cache.devices.is_empty() {
        return cache.devices;
    }

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Ok(adb_path) = adb::find_adb_binary() {
            if let Ok(list) = adb::list_devices(&adb_path) {
                let _ = tx.send(list);
            }
        }
    });

    match rx.recv_timeout(Duration::from_millis(TIMEOUT_MS)) {
        Ok(list) => {
            let converted: Vec<CachedDevice> = list
                .into_iter()
                .map(|d: DeviceInfo| CachedDevice {
                    serial: d.serial,
                    model: d.model,
                    state: d.state,
                })
                .collect();
            cache.devices = converted.clone();
            cache.devices_updated_at = now;
            save_cache(&cache);
            converted
        }
        Err(_) => cache.devices,
    }
}

pub fn get_packages_fast(serial: &str) -> Vec<String> {
    let mut cache = load_cache();
    let now = now_secs();
    if let Some(entry) = cache.packages.get(serial) {
        if now.saturating_sub(entry.updated_at) <= TTL_SECS && !entry.items.is_empty() {
            return entry.items.clone();
        }
    }

    let serial_owned = serial.to_string();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let Ok(adb_path) = adb::find_adb_binary() else {
            return;
        };
        let Ok(output) = Command::new(adb_path)
            .args(["-s", &serial_owned, "shell", "pm", "list", "packages", "-3"])
            .output()
        else {
            return;
        };
        if !output.status.success() {
            return;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut pkgs: Vec<String> = stdout
            .lines()
            .filter_map(|line| line.trim().strip_prefix("package:").map(|s| s.to_string()))
            .collect();
        pkgs.sort();
        let _ = tx.send(pkgs);
    });

    match rx.recv_timeout(Duration::from_millis(TIMEOUT_MS)) {
        Ok(pkgs) => {
            cache.packages.insert(
                serial.to_string(),
                CachedPackages {
                    updated_at: now,
                    items: pkgs.clone(),
                },
            );
            save_cache(&cache);
            pkgs
        }
        Err(_) => cache
            .packages
            .get(serial)
            .map(|e| e.items.clone())
            .unwrap_or_default(),
    }
}
