use crate::adb;
use crate::selector;
use crate::store::Store;
use anyhow::{bail, Result};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

pub struct RebootOptions {
    pub wait_secs: u64,
    pub enter_shell: bool,
    pub restore_root: bool,
    pub restore_remount: bool,
    pub assume_yes: bool,
    pub target_mode: Option<String>,
}

/// 智能重启：重启后轮询等待 `sys.boot_completed == 1`（最长 wait_secs 秒），
/// 按需自动恢复 `adb root` 与 `adb remount`，最后直接进入 `adb shell`。
pub fn run_smart_reboot(
    adb_path: &PathBuf,
    serial: &str,
    store: &Store,
    opts: RebootOptions,
) -> Result<u8> {
    let label = store.display_label(serial);
    let mut reboot_args = vec!["reboot".to_string()];
    if let Some(ref mode) = opts.target_mode {
        reboot_args.push(mode.clone());
    }

    if !opts.assume_yes {
        let preview = reboot_args.join(" ");
        if !selector::confirm_dangerous(&label, serial, &preview)? {
            println!("已取消重启。");
            return Ok(0);
        }
    }

    println!("[adbx] 正在重启设备 {label} ({serial})...");
    let cap = adb::run_captured_with_timeout(adb_path, Some(serial), &reboot_args, 15)?;
    if cap.exit_code != 0 && !cap.stderr.is_empty() {
        eprintln!(
            "[adbx] reboot 输出提示: {}",
            String::from_utf8_lossy(&cap.stderr).trim()
        );
    }

    // 若重启进 bootloader/recovery 或未要求等待恢复，则直接结束
    if opts.target_mode.is_some() || (!opts.enter_shell && !opts.restore_root && !opts.restore_remount) {
        return Ok(0);
    }

    // 先等待 4 秒让设备真正掉线，避免误读到关机前的旧 sys.boot_completed=1
    println!(
        "[adbx] 等待设备重启完成（最长 {} 秒，检测到 sys.boot_completed=1 后将立即继续）...",
        opts.wait_secs
    );
    thread::sleep(Duration::from_secs(4));

    let started = Instant::now();
    let deadline = Duration::from_secs(opts.wait_secs.max(10));
    let mut booted = false;

    while started.elapsed() < deadline {
        if is_device_online(adb_path, serial) && is_boot_completed(adb_path, serial) {
            booted = true;
            break;
        }
        let elapsed = started.elapsed().as_secs() + 4;
        eprint!("\r[adbx] 等待 {label} 启动中... {elapsed}s / {}s", opts.wait_secs);
        thread::sleep(Duration::from_secs(2));
    }
    eprintln!();

    if !booted {
        // 超时兜底：如果设备已经 online 但 sys.boot_completed 尚未置 1，仍尝试继续恢复
        if !is_device_online(adb_path, serial) {
            bail!(
                "等待 {} 秒后设备 {label} ({serial}) 仍未上线，可使用 `--wait 120` 延长等待时间",
                opts.wait_secs
            );
        }
        eprintln!("[adbx] 提示：已达等待上限且设备已上线，尝试继续执行后续操作...");
    } else {
        println!(
            "[adbx] 设备 {label} 已启动完成（耗时 {}s）",
            started.elapsed().as_secs() + 4
        );
    }

    // 自动恢复 root（adb root 会重启设备端 adbd，需短暂等待其重新上线）
    if opts.restore_root || opts.restore_remount {
        println!("[adbx] 自动恢复 `adb root`...");
        let root_cap = adb::run_captured_with_timeout(
            adb_path,
            Some(serial),
            &["root".into()],
            15,
        )?;
        let root_msg = String::from_utf8_lossy(&root_cap.stdout).trim().to_string();
        if !root_msg.is_empty() {
            println!("[adbx] root -> {root_msg}");
        }
        thread::sleep(Duration::from_secs(2));
        wait_online(adb_path, serial, 15)?;
    }

    // 自动恢复 remount
    if opts.restore_remount {
        println!("[adbx] 自动恢复 `adb remount`...");
        let remount_cap = adb::run_captured_with_timeout(
            adb_path,
            Some(serial),
            &["remount".into()],
            20,
        )?;
        let out = String::from_utf8_lossy(&remount_cap.stdout).trim().to_string();
        let err = String::from_utf8_lossy(&remount_cap.stderr).trim().to_string();
        if !out.is_empty() {
            println!("[adbx] remount -> {out}");
        }
        if !err.is_empty() {
            eprintln!("[adbx] remount stderr -> {err}");
        }
    }

    if opts.enter_shell {
        println!(
            "[adbx] 已就绪（root={}, remount={}），正在进入增强 Shell...",
            opts.restore_root || opts.restore_remount,
            opts.restore_remount
        );
        if selector::is_interactive_tty() {
            return crate::repl::run_repl(adb_path, serial.to_string(), store);
        }
        return adb::run_passthrough(adb_path, Some(serial), &["shell".into()]);
    }

    Ok(0)
}

fn is_device_online(adb_path: &PathBuf, serial: &str) -> bool {
    adb::run_captured_with_timeout(adb_path, Some(serial), &["get-state".into()], 3)
        .map(|c| c.exit_code == 0 && String::from_utf8_lossy(&c.stdout).trim() == "device")
        .unwrap_or(false)
}

fn is_boot_completed(adb_path: &PathBuf, serial: &str) -> bool {
    adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["shell".into(), "getprop".into(), "sys.boot_completed".into()],
        3,
    )
    .map(|c| c.exit_code == 0 && String::from_utf8_lossy(&c.stdout).trim() == "1")
    .unwrap_or(false)
}

fn wait_online(adb_path: &PathBuf, serial: &str, timeout_secs: u64) -> Result<()> {
    let started = Instant::now();
    let deadline = Duration::from_secs(timeout_secs);
    while started.elapsed() < deadline {
        if is_device_online(adb_path, serial) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(500));
    }
    bail!("等待 adbd 重新上线超时（{timeout_secs}s）")
}
