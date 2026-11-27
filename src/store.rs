use crate::adb::DeviceInfo;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
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

/// A fixed selection contains full transport serials resolved once at selection time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceTarget {
    Fixed(Vec<String>),
    Group(String),
    All,
}

impl DeviceTarget {
    pub fn single(serial: &str) -> Self {
        Self::Fixed(vec![serial.to_owned()])
    }

    pub fn single_serial(&self) -> Option<&str> {
        match self {
            Self::Fixed(list) if list.len() == 1 => Some(&list[0]),
            _ => None,
        }
    }

    pub fn spec(&self) -> String {
        match self {
            Self::Fixed(list) => list.join(","),
            Self::Group(name) => name.clone(),
            Self::All => "all".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionEntry {
    // Keep the old display field readable by earlier adbx releases.
    serial: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<DeviceTarget>,
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
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("无法替换 {}", path.display()));
    }
    Ok(())
}

pub fn config_dir() -> Result<PathBuf> {
    let dir = match std::env::var("ADBX_CONFIG_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
        _ => dirs::config_dir()
            .context("无法获取系统配置目录")?
            .join("adbx"),
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn with_store_lock<T>(dir: &Path, change: impl FnOnce() -> Result<T>) -> Result<T> {
    // 锁文件独立于被原子替换的数据文件，所有写入者都在同一个文件上加锁。
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".adbx-state.lock"))?;
    let mut lock = fd_lock::RwLock::new(file);
    let _guard = lock.write()?;
    change()
}

pub(crate) fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("读取 {} 失败", path.display())),
    }
}

fn read_devices(dir: &Path) -> Result<DevicesConfig> {
    let path = dir.join("devices.toml");
    match read_optional(&path)? {
        Some(raw) => toml::from_str(&raw).with_context(|| {
            format!(
                "无法解析 {}，请用 adbx config edit 修复，或用 adbx config import 导入有效配置",
                path.display()
            )
        }),
        None => Ok(DevicesConfig::default()),
    }
}

fn read_sessions(dir: &Path) -> Result<SessionState> {
    let path = dir.join("sessions.json");
    let mut state: SessionState = match read_optional(&path)? {
        Some(raw) => {
            serde_json::from_str(&raw).with_context(|| format!("无法解析 {}", path.display()))?
        }
        None => SessionState::default(),
    };
    let cutoff = now_secs().saturating_sub(SESSION_EXPIRE_SECS);
    state.locked.retain(|_, entry| entry.updated_at >= cutoff);
    Ok(state)
}

impl Store {
    pub fn load() -> Result<Self> {
        let config_dir = config_dir()?;
        let devices_cfg = read_devices(&config_dir)?;
        let session_state = read_sessions(&config_dir)?;
        Ok(Self {
            config_dir,
            devices_cfg,
            session_state,
        })
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

    fn update_devices(&mut self, edit: impl FnOnce(&mut DevicesConfig) -> bool) -> Result<bool> {
        with_store_lock(&self.config_dir, || {
            let mut cfg = read_devices(&self.config_dir)?;
            let changed = edit(&mut cfg);
            if changed {
                atomic_write(
                    &self.config_dir.join("devices.toml"),
                    &toml::to_string_pretty(&cfg)?,
                )?;
            }
            self.devices_cfg = cfg;
            Ok(changed)
        })
    }

    pub fn alias_for(&self, serial: &str) -> Option<&str> {
        self.devices_cfg.aliases.get(serial).map(|s| s.as_str())
    }

    pub fn display_label(&self, serial: &str) -> String {
        self.alias_for(serial).unwrap_or(serial).to_string()
    }

    pub fn set_alias(&mut self, serial: &str, alias: &str) -> Result<()> {
        self.update_devices(|cfg| {
            cfg.aliases.retain(|_, v| v != alias);
            cfg.aliases.insert(serial.to_string(), alias.to_string());
            true
        })?;
        Ok(())
    }

    pub fn group_members(&self, group_name: &str) -> Option<&[String]> {
        self.devices_cfg.groups.get(group_name).map(|v| v.as_slice())
    }

    pub fn list_groups(&self) -> &BTreeMap<String, Vec<String>> {
        &self.devices_cfg.groups
    }

    pub fn set_group(&mut self, name: &str, members: Vec<String>) -> Result<()> {
        self.update_devices(|cfg| {
            cfg.groups.insert(name.to_string(), members);
            true
        })?;
        Ok(())
    }

    pub fn remove_group(&mut self, name: &str) -> Result<bool> {
        self.update_devices(|cfg| cfg.groups.remove(name).is_some())
    }

    pub fn save_wifi_host(&mut self, label: &str, endpoint: &str) -> Result<()> {
        self.update_devices(|cfg| {
            cfg.wifi_hosts
                .insert(label.to_string(), endpoint.to_string());
            true
        })?;
        Ok(())
    }

    pub fn list_wifi_hosts(&self) -> &BTreeMap<String, String> {
        &self.devices_cfg.wifi_hosts
    }

    pub fn list_ports(&self) -> &[PortRule] {
        &self.devices_cfg.ports
    }

    pub fn add_port_rule(&mut self, rule: PortRule) -> Result<()> {
        self.update_devices(|cfg| {
            if cfg.ports.contains(&rule) {
                return false;
            }
            cfg.ports.push(rule);
            true
        })?;
        Ok(())
    }

    pub fn remove_port_rule(&mut self, rule: &PortRule) -> Result<bool> {
        self.update_devices(|cfg| {
            let before = cfg.ports.len();
            cfg.ports.retain(|r| r != rule);
            before != cfg.ports.len()
        })
    }

    pub fn list_macros(&self) -> &BTreeMap<String, Vec<String>> {
        &self.devices_cfg.macros
    }

    pub fn get_macro(&self, name: &str) -> Option<&[String]> {
        self.devices_cfg.macros.get(name).map(|v| v.as_slice())
    }

    pub fn set_macro(&mut self, name: &str, args: Vec<String>) -> Result<()> {
        self.update_devices(|cfg| {
            cfg.macros.insert(name.to_string(), args);
            true
        })?;
        Ok(())
    }

    pub fn remove_macro(&mut self, name: &str) -> Result<bool> {
        self.update_devices(|cfg| cfg.macros.remove(name).is_some())
    }

    pub fn list_log_highlights(&self) -> &BTreeMap<String, String> {
        &self.devices_cfg.log_highlights
    }

    pub fn session_target(&self, id: &str) -> Result<Option<DeviceTarget>> {
        let Some(entry) = self.session_state.locked.get(id) else {
            return Ok(None);
        };
        if let Some(target) = &entry.target {
            return Ok(Some(target.clone()));
        }
        // Old sessions stored a single string. A name shared with a group cannot
        // safely be recovered if the originally locked device is now offline.
        let raw = &entry.serial;
        if raw == "all" {
            return Ok(Some(DeviceTarget::All));
        }
        if self.group_members(raw).is_some() {
            bail!("旧会话锁 {raw} 未记录目标类型，请用 adbx use --group {raw} 或 adbx use <完整序列号> 重新锁定");
        }
        Ok(Some(DeviceTarget::Fixed(
            raw.split(',').map(str::to_owned).collect(),
        )))
    }

    pub fn select_target(&self, input: &str, online: &[DeviceInfo]) -> Result<DeviceTarget> {
        let spec = input.trim_start_matches('@');
        if spec.eq_ignore_ascii_case("all") {
            return Ok(DeviceTarget::All);
        }
        if self.group_members(spec).is_some()
            && !online.iter().any(|d| d.serial == spec)
            && !self.devices_cfg.aliases.values().any(|a| a == spec)
        {
            return Ok(DeviceTarget::Group(spec.to_owned()));
        }
        self.select_devices(spec, online)
    }

    pub fn select_devices(&self, spec: &str, online: &[DeviceInfo]) -> Result<DeviceTarget> {
        let mut serials = Vec::new();
        for part in spec.split(',').map(str::trim) {
            if part.is_empty() {
                bail!("目标序列号不能为空");
            }
            let serial = if online.iter().any(|d| d.serial == part) {
                part.to_owned()
            } else {
                self.resolve_input_to_serial(part, online)?
            };
            if !online.iter().any(|d| d.serial == serial) {
                bail!("设备 {serial} 未在线");
            }
            if !serials.contains(&serial) {
                serials.push(serial);
            }
        }
        Ok(DeviceTarget::Fixed(serials))
    }

    pub fn locked_devices(
        &self,
        target: &DeviceTarget,
        online: &[DeviceInfo],
    ) -> Result<Vec<DeviceInfo>> {
        match target {
            DeviceTarget::All => Ok(online.to_vec()),
            DeviceTarget::Group(name) => self.expand_group_online(name, online),
            DeviceTarget::Fixed(serials) => serials.iter().map(|serial| {
                online.iter().find(|d| &d.serial == serial).cloned().with_context(|| format!(
                    "当前会话锁定的设备 {} ({serial}) 未在线。请重新连接，或使用 adbx use 重新选择、adbx unuse 解除锁定。",
                    self.display_label(serial)
                ))
            }).collect(),
        }
    }

    pub fn set_session_target(&mut self, id: &str, target: &DeviceTarget) -> Result<()> {
        self.update_sessions(|state| {
            state.locked.insert(
                id.to_owned(),
                SessionEntry {
                    serial: target.spec(),
                    target: Some(target.clone()),
                    updated_at: now_secs(),
                },
            );
        })
    }

    pub fn clear_session_device(&mut self, session_id: &str) -> Result<()> {
        self.update_sessions(|state| {
            state.locked.remove(session_id);
        })
    }

    pub fn get_device_flags(&self, serial: &str) -> DeviceRuntimeFlags {
        self.session_state
            .device_flags
            .get(serial)
            .cloned()
            .unwrap_or_default()
    }

    pub fn update_device_flags(
        &mut self,
        serial: &str,
        edit: impl FnOnce(&mut DeviceRuntimeFlags),
    ) -> Result<()> {
        self.update_sessions(|state| edit(state.device_flags.entry(serial.to_owned()).or_default()))
    }

    fn update_sessions(&mut self, edit: impl FnOnce(&mut SessionState)) -> Result<()> {
        with_store_lock(&self.config_dir, || {
            let mut state = read_sessions(&self.config_dir)?;
            edit(&mut state);
            atomic_write(
                &self.config_dir.join("sessions.json"),
                &serde_json::to_string_pretty(&state)?,
            )?;
            self.session_state = state;
            Ok(())
        })
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
    fn legacy_lock_requires_reselection_when_its_type_is_ambiguous() {
        let mut store = sample_store();
        let entry = |serial: &str| SessionEntry {
            serial: serial.to_owned(),
            updated_at: now_secs(),
            target: None,
        };
        store
            .session_state
            .locked
            .insert("terminal".into(), entry("QA"));
        assert!(store
            .session_target("terminal")
            .unwrap_err()
            .to_string()
            .contains("use --group"));
        store
            .session_state
            .locked
            .insert("terminal".into(), entry("A,B"));
        assert_eq!(
            store.session_target("terminal").unwrap(),
            Some(DeviceTarget::Fixed(vec!["A".into(), "B".into()]))
        );
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

        st.set_session_target("sess-1", &DeviceTarget::single("DEV001")).unwrap();
        assert_eq!(st.session_target("sess-1").unwrap().as_ref().and_then(DeviceTarget::single_serial), Some("DEV001"));
        let mut stale = sample_store();
        stale.config_dir = dir.clone();
        stale.set_session_target("sess-2", &DeviceTarget::single("DEV002")).unwrap();
        assert_eq!(stale.session_target("sess-1").unwrap().as_ref().and_then(DeviceTarget::single_serial), Some("DEV001"));
        st.clear_session_device("sess-1").unwrap();
        assert!(st.session_target("sess-1").unwrap().is_none());
        assert_eq!(st.session_target("sess-2").unwrap().as_ref().and_then(DeviceTarget::single_serial), Some("DEV002"));

        st.update_device_flags("DEV001", |flags| {
            flags.wants_root = true;
            flags.wants_remount = true;
        }).unwrap();
        let flags = st.get_device_flags("DEV001");
        assert!(flags.wants_root && flags.wants_remount);

        let _ = fs::remove_dir_all(&dir);
    }
}
