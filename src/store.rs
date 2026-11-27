use crate::adb::DeviceInfo;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const SESSION_EXPIRE_SECS: u64 = 7 * 24 * 3600;
static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortRule {
    /// "reverse" or "forward"
    pub direction: String,
    /// e.g. "tcp:8081"
    pub local: String,
    /// e.g. "tcp:8081"
    pub remote: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DevicesConfig {
    #[serde(default)]
    aliases: BTreeMap<String, String>,
    #[serde(default)]
    groups: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    wifi_hosts: BTreeMap<String, String>,
    #[serde(default)]
    ports: Vec<PortRule>,
    /// 自定义快捷宏: macro_name -> adb args (e.g. ["shell", "svc", "wifi", "disable"])
    #[serde(default)]
    macros: BTreeMap<String, Vec<String>>,
    /// 自定义日志高亮规则: regex_pattern -> color_name (red/green/yellow/blue/magenta/cyan/red_bg/yellow_bg/green_bg)
    #[serde(default)]
    log_highlights: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionEntry {
    serial: String,
    updated_at: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DeviceRuntimeFlags {
    #[serde(default)]
    pub wants_root: bool,
    #[serde(default)]
    pub wants_remount: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionState {
    #[serde(default)]
    locked: BTreeMap<String, SessionEntry>,
    /// serial -> 记住该设备是否执行过 adbx root / adbx remount，供重启后自动恢复
    #[serde(default)]
    device_flags: BTreeMap<String, DeviceRuntimeFlags>,
}

pub struct Store {
    config_dir: PathBuf,
    devices_cfg: DevicesConfig,
    session_state: SessionState,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{seq}", std::process::id()));
    fs::write(&tmp, content)?;
    if fs::rename(&tmp, path).is_err() {
        let _ = fs::write(path, content);
        let _ = fs::remove_file(&tmp);
    }
    Ok(())
}

impl Store {
    pub fn load() -> Result<Self> {
        let config_dir = match std::env::var("ADBX_CONFIG_DIR") {
            Ok(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
            _ => dirs::config_dir()
                .context("无法获取系统配置目录")?
                .join("adbx"),
        };
        fs::create_dir_all(&config_dir)?;

        let devices_path = config_dir.join("devices.toml");
        let devices_cfg = if devices_path.exists() {
            let raw = fs::read_to_string(&devices_path)?;
            toml::from_str(&raw).unwrap_or_default()
        } else {
            DevicesConfig::default()
        };

        let state_path = config_dir.join("sessions.json");
        let mut session_state = if state_path.exists() {
            let raw = fs::read_to_string(&state_path)?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            SessionState::default()
        };

        let cutoff = now_secs().saturating_sub(SESSION_EXPIRE_SECS);
        session_state.locked.retain(|_, v| v.updated_at >= cutoff);

        Ok(Self {
            config_dir,
            devices_cfg,
            session_state,
        })
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn devices_config_path(&self) -> PathBuf {
        self.config_dir.join("devices.toml")
    }

    pub fn sessions_path(&self) -> PathBuf {
        self.config_dir.join("sessions.json")
    }

    pub fn history_path(&self) -> PathBuf {
        self.config_dir.join("history.jsonl")
    }

    pub fn repl_history_path(&self) -> PathBuf {
        self.config_dir.join("repl_history.txt")
    }

    pub fn validate_devices_toml(raw: &str) -> Result<()> {
        let _: DevicesConfig = toml::from_str(raw).context("devices.toml 语法校验失败")?;
        Ok(())
    }

    fn save_devices(&self) -> Result<()> {
        let raw = toml::to_string_pretty(&self.devices_cfg)?;
        atomic_write(&self.devices_config_path(), &raw)
    }

    pub fn alias_for(&self, serial: &str) -> Option<&str> {
        self.devices_cfg.aliases.get(serial).map(|s| s.as_str())
    }

    pub fn display_label(&self, serial: &str) -> String {
        self.alias_for(serial).unwrap_or(serial).to_string()
    }

    pub fn set_alias(&mut self, serial: &str, alias: &str) -> Result<()> {
        self.devices_cfg.aliases.retain(|_, v| v != alias);
        self.devices_cfg
            .aliases
            .insert(serial.to_string(), alias.to_string());
        self.save_devices()
    }

    pub fn group_members(&self, group_name: &str) -> Option<&[String]> {
        self.devices_cfg.groups.get(group_name).map(|v| v.as_slice())
    }

    pub fn list_groups(&self) -> &BTreeMap<String, Vec<String>> {
        &self.devices_cfg.groups
    }

    pub fn set_group(&mut self, name: &str, members: Vec<String>) -> Result<()> {
        self.devices_cfg.groups.insert(name.to_string(), members);
        self.save_devices()
    }

    pub fn remove_group(&mut self, name: &str) -> Result<bool> {
        let removed = self.devices_cfg.groups.remove(name).is_some();
        if removed {
            self.save_devices()?;
        }
        Ok(removed)
    }

    pub fn save_wifi_host(&mut self, label: &str, endpoint: &str) -> Result<()> {
        self.devices_cfg
            .wifi_hosts
            .insert(label.to_string(), endpoint.to_string());
        self.save_devices()
    }

    pub fn list_wifi_hosts(&self) -> &BTreeMap<String, String> {
        &self.devices_cfg.wifi_hosts
    }

    pub fn list_ports(&self) -> &[PortRule] {
        &self.devices_cfg.ports
    }

    pub fn add_port_rule(&mut self, rule: PortRule) -> Result<()> {
        if !self.devices_cfg.ports.contains(&rule) {
            self.devices_cfg.ports.push(rule);
            self.save_devices()?;
        }
        Ok(())
    }

    pub fn remove_port_rule(&mut self, rule: &PortRule) -> Result<bool> {
        let before = self.devices_cfg.ports.len();
        self.devices_cfg.ports.retain(|r| r != rule);
        let removed = self.devices_cfg.ports.len() != before;
        if removed {
            self.save_devices()?;
        }
        Ok(removed)
    }

    pub fn list_macros(&self) -> &BTreeMap<String, Vec<String>> {
        &self.devices_cfg.macros
    }

    pub fn get_macro(&self, name: &str) -> Option<&[String]> {
        self.devices_cfg.macros.get(name).map(|v| v.as_slice())
    }

    pub fn set_macro(&mut self, name: &str, args: Vec<String>) -> Result<()> {
        self.devices_cfg.macros.insert(name.to_string(), args);
        self.save_devices()
    }

    pub fn remove_macro(&mut self, name: &str) -> Result<bool> {
        let removed = self.devices_cfg.macros.remove(name).is_some();
        if removed {
            self.save_devices()?;
        }
        Ok(removed)
    }

    pub fn list_log_highlights(&self) -> &BTreeMap<String, String> {
        &self.devices_cfg.log_highlights
    }

    pub fn get_session_device(&self, session_id: &str) -> Option<String> {
        self.session_state.locked.get(session_id).map(|e| e.serial.clone())
    }

    pub fn set_session_device(&mut self, session_id: &str, serial: &str) -> Result<()> {
        self.session_state.locked.insert(
            session_id.to_string(),
            SessionEntry {
                serial: serial.to_string(),
                updated_at: now_secs(),
            },
        );
        self.save_sessions()
    }

    pub fn clear_session_device(&mut self, session_id: &str) -> Result<()> {
        self.session_state.locked.remove(session_id);
        self.save_sessions()
    }

    pub fn get_device_flags(&self, serial: &str) -> DeviceRuntimeFlags {
        self.session_state
            .device_flags
            .get(serial)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_device_flags(&mut self, serial: &str, flags: DeviceRuntimeFlags) -> Result<()> {
        self.session_state
            .device_flags
            .insert(serial.to_string(), flags);
        self.save_sessions()
    }

    fn save_sessions(&self) -> Result<()> {
        let raw = serde_json::to_string_pretty(&self.session_state)?;
        atomic_write(&self.config_dir.join("sessions.json"), &raw)?;
        Ok(())
    }

    pub fn resolve_input_to_serial(&self, input: &str, devices: &[DeviceInfo]) -> Result<String> {
        for (serial, alias) in &self.devices_cfg.aliases {
            if alias == input {
                return Ok(serial.clone());
            }
        }
        if devices.iter().any(|d| d.serial == input) {
            return Ok(input.to_string());
        }
        let matches: Vec<_> = devices
            .iter()
            .filter(|d| d.serial.starts_with(input))
            .collect();
        match matches.len() {
            1 => Ok(matches[0].serial.clone()),
            0 => bail!("未找到匹配 '{input}' 的设备或别名"),
            _ => bail!("前缀 '{input}' 匹配到多台设备，请提供更精确的别名或序列号"),
        }
    }

    pub fn expand_group_online(&self, group_name: &str, online: &[DeviceInfo]) -> Result<Vec<DeviceInfo>> {
        let Some(members) = self.group_members(group_name) else {
            bail!("未定义设备分组 '{group_name}'");
        };
        let mut out = Vec::new();
        for m in members {
            if let Ok(serial) = self.resolve_input_to_serial(m, online) {
                if !out.iter().any(|d: &DeviceInfo| d.serial == serial) {
                    if let Some(found) = online.iter().find(|d| d.serial == serial) {
                        out.push(found.clone());
                    }
                }
            }
        }
        if out.is_empty() {
            bail!("分组 '{group_name}' 中目前没有在线设备");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_store() -> Store {
        let mut cfg = DevicesConfig::default();
        cfg.aliases.insert("R5CT10ABCDE".into(), "pixel".into());
        cfg.aliases.insert("9A8B7C6D".into(), "mi".into());
        cfg.groups.insert("QA".into(), vec!["pixel".into(), "mi".into()]);
        Store {
            config_dir: PathBuf::new(),
            devices_cfg: cfg,
            session_state: SessionState::default(),
        }
    }

    #[test]
    fn resolve_alias_prefix_and_uppercase_group() {
        let st = sample_store();
        let devs = vec![
            DeviceInfo { serial: "R5CT10ABCDE".into(), state: "device".into(), model: "Pixel_8".into() },
            DeviceInfo { serial: "9A8B7C6D".into(), state: "device".into(), model: "Xiaomi_14".into() },
        ];
        assert_eq!(st.resolve_input_to_serial("pixel", &devs).unwrap(), "R5CT10ABCDE");
        let grp = st.expand_group_online("QA", &devs).unwrap();
        assert_eq!(grp.len(), 2);
        assert!(st.resolve_input_to_serial("unknown", &devs).is_err());

        let ambig = vec![
            DeviceInfo { serial: "ABC111".into(), state: "device".into(), model: "A".into() },
            DeviceInfo { serial: "ABC222".into(), state: "device".into(), model: "B".into() },
        ];
        assert!(st.resolve_input_to_serial("ABC", &ambig).is_err());
    }

    #[test]
    fn validates_devices_toml_and_persists_all_store_sections() {
        assert!(Store::validate_devices_toml("[aliases]\nDEV1 = \"pixel\"\n").is_ok());
        assert!(Store::validate_devices_toml("broken = [unclosed").is_err());

        let dir = std::env::temp_dir().join(format!(
            "adbx-store-test-{}-{}",
            std::process::id(),
            now_secs()
        ));
        fs::create_dir_all(&dir).unwrap();
        let mut st = Store {
            config_dir: dir.clone(),
            devices_cfg: DevicesConfig::default(),
            session_state: SessionState::default(),
        };

        st.set_alias("DEV001", "pixel").unwrap();
        assert_eq!(st.alias_for("DEV001"), Some("pixel"));
        assert_eq!(st.display_label("DEV001"), "pixel");

        st.set_group("qa", vec!["pixel".into()]).unwrap();
        assert_eq!(st.group_members("qa"), Some(["pixel".to_string()].as_slice()));
        assert!(st.remove_group("qa").unwrap());
        assert!(st.group_members("qa").is_none());

        st.set_macro("wifi-off", vec!["shell".into(), "svc".into(), "wifi".into(), "disable".into()])
            .unwrap();
        assert_eq!(st.list_macros().len(), 1);
        assert!(st.remove_macro("wifi-off").unwrap());

        let rule = PortRule {
            direction: "reverse".into(),
            local: "tcp:8081".into(),
            remote: "tcp:8081".into(),
        };
        st.add_port_rule(rule.clone()).unwrap();
        assert_eq!(st.list_ports(), std::slice::from_ref(&rule));
        assert!(st.remove_port_rule(&rule).unwrap());

        st.save_wifi_host("pixel", "192.168.1.50:5555").unwrap();
        assert_eq!(
            st.list_wifi_hosts().get("pixel").map(|s| s.as_str()),
            Some("192.168.1.50:5555")
        );

        st.set_session_device("sess-1", "DEV001").unwrap();
        assert_eq!(st.get_session_device("sess-1").as_deref(), Some("DEV001"));
        st.clear_session_device("sess-1").unwrap();
        assert!(st.get_session_device("sess-1").is_none());

        st.set_device_flags(
            "DEV001",
            DeviceRuntimeFlags {
                wants_root: true,
                wants_remount: true,
            },
        )
        .unwrap();
        let flags = st.get_device_flags("DEV001");
        assert!(flags.wants_root && flags.wants_remount);

        let _ = fs::remove_dir_all(&dir);
    }
}
