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
    let rev = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["reverse".into(), "--list".into()],
        5,
    )?;
    let rev_out = String::from_utf8_lossy(&rev.stdout);
    if rev_out.trim().is_empty() {
        println!("(无)");
    } else {
        print!("{rev_out}");
    }

    println!("\n=== 设备 {label} ({serial}) 当前 forward 映射 ===");
    let fwd = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["forward".into(), "--list".into()],
        5,
    )?;
    let fwd_out = String::from_utf8_lossy(&fwd.stdout);
    if fwd_out.trim().is_empty() {
        println!("(无)");
    } else {
        print!("{fwd_out}");
    }

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
    let logs = Mutex::new(Vec::<(usize, String, bool)>::new());

    thread::scope(|s| {
        for (idx, serial) in serials.iter().enumerate() {
            let logs_ref = &logs;
            let label = store.display_label(serial);
            s.spawn(move || {
                for r in rules {
                    let res = adb::run_captured_with_timeout(
                        adb_path,
                        Some(serial),
                        &[r.direction.clone(), r.local.clone(), r.remote.clone()],
                        5,
                    );
                    match res {
                        Ok(cap) if cap.exit_code == 0 => {
                            logs_ref.lock().unwrap().push((
                                idx,
                                format!("[OK]   {label} -> {} {} {}", r.direction, r.local, r.remote),
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
                                format!("[FAIL] {label} -> {} {} {} ({e})", r.direction, r.local, r.remote),
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

pub fn clear_ports(adb_path: &PathBuf, serial: &str) -> Result<u8> {
    let _ = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["reverse".into(), "--remove-all".into()],
        5,
    );
    let _ = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["forward".into(), "--remove-all".into()],
        5,
    );
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
