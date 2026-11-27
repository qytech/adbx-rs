use crate::adb;
use crate::store::Store;
use anyhow::{bail, Result};
use regex::Regex;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

static IP_RE: OnceLock<Regex> = OnceLock::new();

pub fn list_wifi(adb_path: &PathBuf, store: &Store) -> Result<u8> {
    println!("=== 已保存的无线设备 (devices.toml) ===");
    if store.list_wifi_hosts().is_empty() {
        println!("(暂无记录，可插入 USB 后执行 `adbx wifi enable` 自动记录)");
    } else {
        for (label, endpoint) in store.list_wifi_hosts() {
            println!("  {:<16} -> {}", label, endpoint);
        }
    }

    println!("\n=== 局域网 mDNS 无线调试服务 (`adb mdns services`) ===");
    let cap = adb::run_captured_with_timeout(
        adb_path,
        None,
        &["mdns".into(), "services".into()],
        5,
    )?;
    let out = String::from_utf8_lossy(&cap.stdout);
    if out.trim().is_empty() {
        println!("(未发现 mDNS 无线调试设备)");
    } else {
        print!("{out}");
    }
    Ok(0)
}

/// 扫描设备全部非回环 IPv4 地址（不再硬编码 wlan0，兼容 wlan1/eth0 等多网卡设备）
pub fn detect_lan_ip(ip_addr_output: &str) -> Option<String> {
    let ip_re = IP_RE.get_or_init(|| Regex::new(r"inet\s+(\d+\.\d+\.\d+\.\d+)/").unwrap());
    let mut iface = "";
    let mut best = None;
    for line in ip_addr_output.lines() {
        if let Some((index, rest)) = line.trim().split_once(':') {
            if index.parse::<u32>().is_ok() {
                iface = rest.trim_start().split([':', '@', ' ']).next().unwrap_or("");
            }
        }
        let Some(caps) = ip_re.captures(line) else { continue };
        let Ok(ip) = caps[1].parse::<std::net::Ipv4Addr>() else { continue };
        if ip.is_loopback() || ip.is_unspecified() || ["rmnet", "ccmni", "wwan", "tun", "p2p"].iter().any(|p| iface.starts_with(p)) { continue; }
        let rank = if iface.starts_with("wl") || iface.starts_with("wifi") { 0 }
            else if iface.starts_with("eth") || iface.starts_with("en") { 1 }
            else if ip.is_private() { 2 } else { 3 };
        if best.is_none_or(|(old, _)| rank < old) { best = Some((rank, ip)); }
    }
    best.map(|(_, ip)| ip.to_string())
}

fn connected(cap: &adb::CommandCapture) -> bool {
    let msg = String::from_utf8_lossy(&cap.stdout);
    cap.exit_code == 0 && !cap.timed_out && msg.lines().any(|line| {
        line.starts_with("connected to ") || line.starts_with("already connected to ")
    })
}

pub fn enable_wifi(adb_path: &PathBuf, serial: &str, store: &mut Store) -> Result<u8> {
    let label = store.display_label(serial);
    println!("正在为 {label} ({serial}) 开启 5555 无线调试...");
    let ip_cap = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["shell".into(), "ip".into(), "-f".into(), "inet".into(), "addr".into(), "show".into()],
        5,
    )?;
    let stdout = String::from_utf8_lossy(&ip_cap.stdout);
    let Some(ip) = detect_lan_ip(&stdout) else {
        bail!("未能在设备网卡上解析到局域网 IPv4，请确认手机已连接 Wi-Fi");
    };
    let endpoint = format!("{ip}:5555");
    let tcp = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["tcpip".into(), "5555".into()],
        10,
    )?;
    if tcp.exit_code != 0 {
        bail!("开启 tcpip 5555 失败: {}", String::from_utf8_lossy(&tcp.stderr));
    }

    // 部分机型执行 tcpip 5555 后 adbd 重启需要短暂缓冲
    std::thread::sleep(Duration::from_secs(1));



    println!("检测到设备地址 {endpoint}，正在连接...");
    let mut conn = adb::run_captured_with_timeout(
        adb_path,
        None,
        &["connect".into(), endpoint.clone()],
        8,
    )?;
    if !connected(&conn) {
        std::thread::sleep(Duration::from_millis(1500));
        conn = adb::run_captured_with_timeout(
            adb_path,
            None,
            &["connect".into(), endpoint.clone()],
            8,
        )?;
    }
    if !connected(&conn) {
        bail!("连接 {endpoint} 失败：{} {}", String::from_utf8_lossy(&conn.stdout).trim(), String::from_utf8_lossy(&conn.stderr).trim());
    }
    store.save_wifi_host(&label, &endpoint)?;
    println!("{}", String::from_utf8_lossy(&conn.stdout).trim());
    println!("已记住无线地址：{label} -> {endpoint}（后续可直接执行 `adbx wifi reconnect`）");
    Ok(conn.exit_code as u8)
}

/// 并发重连所有已保存的无线调试设备（单台离线不再串行拖慢整体耗时）
pub fn reconnect_all(adb_path: &PathBuf, store: &Store) -> Result<u8> {
    let hosts: Vec<(String, String)> = store
        .list_wifi_hosts()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if hosts.is_empty() {
        println!("暂无已保存的无线设备，请先插线执行 `adbx wifi enable`。");
        return Ok(0);
    }

    let results = std::sync::Mutex::new(Vec::<(usize, String, bool)>::with_capacity(hosts.len()));
    std::thread::scope(|s| {
        for (idx, (label, endpoint)) in hosts.iter().enumerate() {
            let res_ref = &results;
            s.spawn(move || {
                match adb::run_captured_with_timeout(
                    adb_path,
                    None,
                    &["connect".into(), endpoint.clone()],
                    6,
                ) {
                    Ok(cap) => {
                        let msg = String::from_utf8_lossy(&cap.stdout).trim().to_string();
                        let ok = connected(&cap);
                        let tag = if ok { "[OK]  " } else { "[FAIL]" };
                        let note = if cap.timed_out { "timeout (6s)".to_string() } else { msg };
                        res_ref
                            .lock()
                            .unwrap()
                            .push((idx, format!("{tag} {label} ({endpoint}) -> {note}"), ok));
                    }
                    Err(e) => {
                        res_ref
                            .lock()
                            .unwrap()
                            .push((idx, format!("[FAIL] {label} ({endpoint}) -> {e}"), false));
                    }
                }
            });
        }
    });

    let mut list = results.into_inner().unwrap();
    list.sort_by_key(|(idx, _, _)| *idx);
    let mut failed = 0usize;
    for (_, line, ok) in list {
        println!("{line}");
        if !ok {
            failed += 1;
        }
    }
    Ok(if failed == 0 { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_non_loopback_lan_ip_across_interfaces() {
        let sample = "1: lo: inet 127.0.0.1/8\n12: wlan1: inet 192.168.31.88/24 brd 192.168.31.255";
        assert_eq!(detect_lan_ip(sample).as_deref(), Some("192.168.31.88"));

        let rfc1918 = "2: rmnet0: inet 172.5.1.10/24\n3: wlan0: inet 172.16.1.10/24";
        assert_eq!(detect_lan_ip(rfc1918).as_deref(), Some("172.16.1.10"));
    }
}
