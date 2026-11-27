use crate::adb;
use crate::store::{PortRule, Store};
use anyhow::{bail, Result};
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;

fn normalize_port_spec(spec: &str) -> String {
    let trimmed = spec.trim();
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        format!("tcp:{trimmed}")
    } else {
        trimmed.to_string()
    }
}

pub fn list_ports(adb_path: &PathBuf, serial: &str, store: &Store) -> Result<u8> {
    let label = store.display_label(serial);
    println!("=== 设备 {label} ({serial}) 当前 reverse 映射 ===");
    let reverse = port_command(adb_path, serial, &["reverse", "--list"])?;
    println!("{}", if reverse.trim().is_empty() { "(无)" } else { reverse.trim_end() });
    println!("\n=== 设备 {label} ({serial}) 当前 forward 映射 ===");
    let forwards = device_forwards(adb_path, serial)?;
    if forwards.is_empty() { println!("(无)"); }
    for (local, remote) in forwards { println!("{serial} {local} {remote}"); }

    println!("\n=== 已保存的端口预设 (devices.toml) ===");
    if store.list_ports().is_empty() {
        println!("(暂无预设，可用 `adbx port add reverse 8081 8081` 添加)");
    } else {
        for r in store.list_ports() {
            println!("  {:<8} {} -> {}", r.direction, r.local, r.remote);
        }
    }
    Ok(0)
}

pub fn add_port(
    adb_path: &PathBuf,
    serial: &str,
    direction: &str,
    local: &str,
    remote: &str,
    store: &mut Store,
) -> Result<u8> {
    let dir = direction.to_ascii_lowercase();
    if dir != "reverse" && dir != "forward" {
        bail!("方向参数仅支持 reverse 或 forward，收到: '{direction}'");
    }
    let local_spec = normalize_port_spec(local);
    let remote_spec = normalize_port_spec(remote);

    let cap = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &[dir.clone(), local_spec.clone(), remote_spec.clone()],
        5,
    )?;
    if cap.exit_code != 0 {
        bail!("设置端口映射失败: {}", String::from_utf8_lossy(&cap.stderr));
    }
    store.add_port_rule(PortRule {
        direction: dir.clone(),
        local: local_spec.clone(),
        remote: remote_spec.clone(),
    })?;
    println!("已生效并保存到预设: adb -s {serial} {dir} {local_spec} {remote_spec}");
    Ok(0)
}

pub fn remove_port(
    direction: &str,
    local: &str,
    remote: &str,
    store: &mut Store,
) -> Result<u8> {
    let dir = direction.to_ascii_lowercase();
    if dir != "reverse" && dir != "forward" {
        bail!("方向参数仅支持 reverse 或 forward，收到: '{direction}'");
    }
    let rule = PortRule {
        direction: dir.clone(),
        local: normalize_port_spec(local),
        remote: normalize_port_spec(remote),
    };
    if store.remove_port_rule(&rule)? {
        println!("已从预设删除: {} {} -> {}", rule.direction, rule.local, rule.remote);
    } else {
        println!("预设中未找到该规则: {} {} -> {}", rule.direction, rule.local, rule.remote);
    }
    Ok(0)
}

/// 并发将端口预设同步到全部目标设备
pub fn sync_ports(adb_path: &PathBuf, serials: &[String], store: &Store) -> Result<u8> {
    let rules = store.list_ports();
    if rules.is_empty() {
        println!("暂无已保存的端口预设，请先执行 `adbx port add reverse 8081 8081`。");
        return Ok(0);
    }
    // Fixed host listeners are shared by every transport on the ADB server.
    // Check the entire batch before applying even its reverse rules.
    let mut fixed = std::collections::HashMap::new();
    for rule in rules
        .iter()
        .filter(|r| r.direction == "forward" && r.local != "tcp:0")
    {
        if serials.len() > 1 {
            bail!(
                "固定主机端点 {} 无法同时转发到多台设备；请指定一台设备或使用 tcp:0 分配独立端口",
                rule.local
            );
        }
        if fixed
            .insert(rule.local.as_str(), rule.remote.as_str())
            .is_some_and(|previous| previous != rule.remote)
        {
            bail!("预设中的主机端点 {} 对应多个目标端点", rule.local);
        }
    }
    let mut already_applied = std::collections::HashSet::new();
    if let Some(serial) = serials.first().filter(|_| !fixed.is_empty()) {
        let listing = port_command(adb_path, serial, &["forward", "--list"])?;
        for line in listing.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if let [owner, local, remote] = fields.as_slice() {
                if let Some(expected) = fixed.get(local) {
                    if *owner != serial || expected != remote {
                        bail!("主机端点 {local} 已映射到 {owner} 的 {remote}，无法同步");
                    }
                    already_applied.insert(local.to_string());
                }
            }
        }
    }
    let logs = Mutex::new(Vec::<(usize, String, bool)>::new());

    thread::scope(|s| {
        for (idx, serial) in serials.iter().enumerate() {
            let logs_ref = &logs;
            let applied = &already_applied;
            let label = store.display_label(serial);
            s.spawn(move || {
                for r in rules {
                    if r.direction == "forward" && applied.contains(&r.local) {
                        logs_ref.lock().unwrap().push((
                            idx,
                            format!(
                                "[OK]   {label} -> forward {} {} (已生效)",
                                r.local, r.remote
                            ),
                            true,
                        ));
                        continue;
                    }
                    let mut args = vec![r.direction.clone()];
                    if r.direction == "forward" {
                        args.push("--no-rebind".into());
                    }
                    args.extend([r.local.clone(), r.remote.clone()]);
                    let res = adb::run_captured_with_timeout(adb_path, Some(serial), &args, 5);
                    match res {
                        Ok(cap) if cap.exit_code == 0 => {
                            let port = String::from_utf8_lossy(&cap.stdout);
                            let local = if r.direction == "forward"
                                && r.local == "tcp:0"
                                && !port.trim().is_empty()
                            {
                                format!("tcp:{}", port.trim())
                            } else {
                                r.local.clone()
                            };
                            logs_ref.lock().unwrap().push((
                                idx,
                                format!("[OK]   {label} -> {} {} {}", r.direction, local, r.remote),
                                true,
                            ));
                        }
                        Ok(cap) => {
                            logs_ref.lock().unwrap().push((
                                idx,
                                format!(
                                    "[FAIL] {label} -> {} {} {} ({})",
                                    r.direction,
                                    r.local,
                                    r.remote,
                                    String::from_utf8_lossy(&cap.stderr).trim()
                                ),
                                false,
                            ));
                        }
                        Err(e) => {
                            logs_ref.lock().unwrap().push((
                                idx,
                                format!(
                                    "[FAIL] {label} -> {} {} {} ({e})",
                                    r.direction, r.local, r.remote
                                ),
                                false,
                            ));
                        }
                    }
                }
            });
        }
    });

    let mut entries = logs.into_inner().unwrap();
    entries.sort_by_key(|(idx, _, _)| *idx);
    let mut failed = 0usize;
    for (_, line, ok) in entries {
        println!("{line}");
        if !ok {
            failed += 1;
        }
    }
    Ok(if failed == 0 { 0 } else { 1 })
}

fn port_command(adb_path: &PathBuf, serial: &str, args: &[&str]) -> Result<String> {
    let args: Vec<_> = args.iter().map(|s| (*s).to_owned()).collect();
    let cap = adb::run_captured_with_timeout(adb_path, Some(serial), &args, 5)?;
    if cap.exit_code != 0 {
        bail!("设备 {serial} 的 {} 失败 (exit={}): {}", args.join(" "), cap.exit_code, String::from_utf8_lossy(&cap.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&cap.stdout).into_owned())
}

fn device_forwards(adb_path: &PathBuf, serial: &str) -> Result<Vec<(String, String)>> {
    let listing = port_command(adb_path, serial, &["forward", "--list"])?;
    Ok(listing.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next()? != serial { return None; }
        Some((fields.next()?.to_owned(), fields.next()?.to_owned()))
    }).collect())
}

pub fn clear_ports(adb_path: &PathBuf, serial: &str) -> Result<u8> {
    // forward listeners are host-wide even when adb is called with -s.
    let forwards = device_forwards(adb_path, serial)?;
    port_command(adb_path, serial, &["reverse", "--remove-all"])?;
    for (local, _) in forwards { port_command(adb_path, serial, &["forward", "--remove", &local])?; }
    println!("已清空设备 {serial} 上的全部 reverse / forward 端口转发。");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_port_specs() {
        assert_eq!(normalize_port_spec("8081"), "tcp:8081");
        assert_eq!(normalize_port_spec(" tcp:8081 "), "tcp:8081");
        assert_eq!(
            normalize_port_spec("localabstract:scrcpy"),
            "localabstract:scrcpy"
        );
    }
}
