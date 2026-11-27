use crate::adb;
use crate::cache;
use crate::logcat::{self, LogLevel, LogcatOptions};
use crate::store::Store;
use anyhow::Result;
use chrono::Local;
use dialoguer::FuzzySelect;
use std::path::PathBuf;

struct MacroAction {
    id: &'static str,
    title: &'static str,
    keywords: &'static str,
}

const ACTIONS: &[MacroAction] = &[
    MacroAction {
        id: "shell",
        title: "进入 adbx 增强 Shell（实时灰色命令建议、Tab 远端路径补全、su 提权）",
        keywords: "shell repl 终端 命令行",
    },
    MacroAction {
        id: "shell-raw",
        title: "进入原生 `adb shell` 交互式 PTY",
        keywords: "shell raw 原生 终端",
    },
    MacroAction {
        id: "logcat",
        title: "打开交互式 Logcat TUI（支持正则/包名正则追 PID、Space 暂停）",
        keywords: "logcat tui 日志 过滤",
    },
    MacroAction {
        id: "logcat-stream",
        title: "终端流式输出彩色 Logcat（按 V/D/I/W/E/F 等级与语义高亮）",
        keywords: "logcat stream 彩色 流式 日志",
    },
    MacroAction {
        id: "watch",
        title: "打开实时多机监控面板（热插拔 / 电量 / 温度）",
        keywords: "watch monitor 监控 电量 温度",
    },
    MacroAction {
        id: "screenshot",
        title: "截屏并保存到当前目录（自动按时间戳命名）",
        keywords: "screenshot 截屏 截图",
    },
    MacroAction {
        id: "top-activity",
        title: "查看当前前台顶层 Activity（纯 Rust 过滤，不依赖设备端 grep）",
        keywords: "activity top 顶层 页面",
    },
    MacroAction {
        id: "meminfo",
        title: "查看运行中进程或系统内存占用（dumpsys meminfo）",
        keywords: "meminfo 内存 性能",
    },
    MacroAction {
        id: "clear-restart",
        title: "清除选中应用数据并重新拉起",
        keywords: "clear restart 清除数据 重启 app",
    },
    MacroAction {
        id: "reboot",
        title: "智能重启（等待开机并自动恢复 root / remount 后进入 Shell）",
        keywords: "reboot rr 重启 root remount",
    },
    MacroAction {
        id: "wifi-adb",
        title: "查看设备局域网 IP 并开启 5555 无线调试",
        keywords: "wifi tcpip 无线 调试 ip",
    },
    MacroAction {
        id: "port-list",
        title: "查看当前 reverse / forward 端口转发与预设",
        keywords: "port forward reverse 端口 转发",
    },
    MacroAction {
        id: "history",
        title: "模糊搜索并一键重放历史命令",
        keywords: "history replay 历史 重放",
    },
    MacroAction {
        id: "restart-server",
        title: "重启本机 ADB 服务（kill-server & start-server）",
        keywords: "server restart 重启 服务",
    },
];

pub fn open_palette(adb_path: &PathBuf, serial: &str, store: &Store) -> Result<u8> {
    let label = store.display_label(serial);
    let mut ids: Vec<String> = Vec::new();
    let mut items: Vec<String> = Vec::new();

    for a in ACTIONS {
        ids.push(a.id.to_string());
        items.push(format!("{:<14} | {} [{}]", a.id, a.title, a.keywords));
    }

    // 合并 devices.toml 中用户自定义的 [macros]
    for (name, cmd_args) in store.list_macros() {
        ids.push(name.clone());
        items.push(format!("{:<14} | [自定义宏] adb {}", name, cmd_args.join(" ")));
    }

    let idx = FuzzySelect::new()
        .with_prompt(format!(
            "adbx 命令面板 [当前设备: {label}]（输入中英文关键词模糊筛选）"
        ))
        .items(&items)
        .default(0)
        .interact()?;

    execute_macro(&ids[idx], adb_path, serial, store)
}

pub fn execute_macro(action_id: &str, adb_path: &PathBuf, serial: &str, store: &Store) -> Result<u8> {
    // 优先检查用户自定义宏
    if let Some(custom_args) = store.get_macro(action_id) {
        eprintln!("[adbx] -> adb -s {serial} {}", custom_args.join(" "));
        return adb::run_passthrough(adb_path, Some(serial), custom_args);
    }

    let label = store.display_label(serial);
    match action_id {
        "shell" => crate::repl::run_repl(adb_path, crate::store::DeviceTarget::single(serial), store),
        "shell-raw" => run_echoed(adb_path, Some(serial), &["shell"]),
        "logcat" => {
            let pkg = pick_optional_package(serial)?;
            let opts = LogcatOptions {
                adb_path: adb_path.clone(),
                serial: serial.to_string(),
                device_label: label,
                package: pkg,
                min_level: LogLevel::Verbose,
                include_pattern: String::new(),
                exclude_pattern: String::new(),
                max_buffer: 100_000,
            };
            logcat::run_tui(opts)?;
            Ok(0)
        }
        "screenshot" => {
            let file = format!("screenshot-{}.png", Local::now().format("%Y%m%d-%H%M%S"));
            let remote = format!("/sdcard/{file}");
            run_echoed(adb_path, Some(serial), &["shell", "screencap", "-p", &remote])?;
            let code = run_echoed(adb_path, Some(serial), &["pull", &remote, &file])?;
            let _ = run_echoed(adb_path, Some(serial), &["shell", "rm", &remote]);
            println!("截图已保存到当前目录: {file}");
            Ok(code)
        }
        "top-activity" => {
            eprintln!("[adbx] -> adb -s {serial} shell dumpsys activity top");
            let cap = adb::run_captured_with_timeout(
                adb_path,
                Some(serial),
                &["shell".into(), "dumpsys".into(), "activity".into(), "top".into()],
                10,
            )?;
            let stdout = String::from_utf8_lossy(&cap.stdout);
            for line in stdout.lines() {
                if line.contains("ACTIVITY ") || line.contains("mResumedActivity") {
                    println!("{}", line.trim());
                }
            }
            Ok(cap.exit_code as u8)
        }
        "clear-restart" => {
            let Some(pkg) = pick_optional_package(serial)? else {
                println!("未选择应用包名，已取消。");
                return Ok(0);
            };
            run_echoed(adb_path, Some(serial), &["shell", "pm", "clear", &pkg])?;
            run_echoed(
                adb_path,
                Some(serial),
                &["shell", "monkey", "-p", &pkg, "-c", "android.intent.category.LAUNCHER", "1"],
            )
        }
        "logcat-stream" => logcat::run_colored_stream(adb_path, serial, &["logcat".into()], store),
        "watch" => {
            crate::watch::run_watch_tui(adb_path.clone())?;
            Ok(0)
        }
        "meminfo" => match pick_running_process_for_meminfo(adb_path, serial)? {
            Some(pid) => run_echoed(adb_path, Some(serial), &["shell", "dumpsys", "meminfo", &pid]),
            None => run_echoed(adb_path, Some(serial), &["shell", "dumpsys", "meminfo"]),
        },
        "reboot" => {
            let flags = store.get_device_flags(serial);
            crate::reboot::run_smart_reboot(
                adb_path,
                serial,
                store,
                crate::reboot::RebootOptions {
                    wait_secs: 60,
                    enter_shell: flags.wants_root || flags.wants_remount,
                    restore_root: flags.wants_root,
                    restore_remount: flags.wants_remount,
                    assume_yes: false,
                    target_mode: None,
                },
            )
        }
        "wifi-adb" => {
            run_echoed(adb_path, Some(serial), &["tcpip", "5555"])?;
            run_echoed(adb_path, Some(serial), &["shell", "ip", "-f", "inet", "addr", "show", "wlan0"])
        }
        "port-list" => crate::port::list_ports(adb_path, serial, store),
        "history" => crate::history::replay_history(adb_path, store, 100, &[], None),
        "restart-server" => {
            run_echoed(adb_path, None, &["kill-server"])?;
            run_echoed(adb_path, None, &["start-server"])
        }
        other => {
            let builtins: Vec<&str> = ACTIONS.iter().map(|a| a.id).collect();
            eprintln!(
                "未知快捷动作: {other}（内置: {}；或通过 `adbx macro set` 自定义）",
                builtins.join(", ")
            );
            Ok(1)
        }
    }
}

fn pick_optional_package(serial: &str) -> Result<Option<String>> {
    let mut pkgs = cache::get_packages_fast(serial);
    if pkgs.is_empty() {
        return Ok(None);
    }
    pkgs.insert(0, "(全部应用 / 不限制)".to_string());
    let idx = FuzzySelect::new()
        .with_prompt("选择目标第三方应用包名（支持模糊搜索）")
        .items(&pkgs)
        .default(0)
        .interact()?;
    if idx == 0 {
        Ok(None)
    } else {
        Ok(Some(pkgs[idx].clone()))
    }
}

fn parse_running_processes(ps_output: &str) -> Vec<(String, String)> {
    let mut procs = Vec::new();
    for line in ps_output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.len() < 2 || matches!(tokens[0], "PID" | "USER" | "UID") {
            continue;
        }
        let pid = if tokens[0].chars().all(|c| c.is_ascii_digit()) {
            tokens[0]
        } else if let Some(p) = tokens.iter().skip(1).find(|t| t.chars().all(|c| c.is_ascii_digit())) {
            *p
        } else {
            continue;
        };
        let name = tokens[tokens.len() - 1];
        if name.starts_with('[') && name.ends_with(']') {
            continue;
        }
        procs.push((pid.to_string(), name.to_string()));
    }
    procs.sort_by(|a, b| {
        let a_app = !a.1.contains('.');
        let b_app = !b.1.contains('.');
        a_app
            .cmp(&b_app)
            .then_with(|| a.1.to_ascii_lowercase().cmp(&b.1.to_ascii_lowercase()))
            .then_with(|| a.0.cmp(&b.0))
    });
    procs
}

fn pick_running_process_for_meminfo(adb_path: &PathBuf, serial: &str) -> Result<Option<String>> {
    let mut procs = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["shell".into(), "ps".into(), "-A".into(), "-o".into(), "PID,NAME".into()],
        3,
    )
    .ok()
    .filter(|c| c.exit_code == 0)
    .map(|c| parse_running_processes(&String::from_utf8_lossy(&c.stdout)))
    .unwrap_or_default();

    if procs.is_empty() {
        procs = adb::run_captured_with_timeout(adb_path, Some(serial), &["shell".into(), "ps".into()], 3)
            .ok()
            .filter(|c| c.exit_code == 0)
            .map(|c| parse_running_processes(&String::from_utf8_lossy(&c.stdout)))
            .unwrap_or_default();
    }

    let mut items = Vec::with_capacity(procs.len() + 1);
    items.push("(全部进程 / 系统内存总览)".to_string());
    for (pid, name) in &procs {
        items.push(format!("{name} (PID: {pid})"));
    }

    let idx = FuzzySelect::new()
        .with_prompt("选择正在运行的进程查看内存占用（支持模糊搜索，选第一项看系统总览）")
        .items(&items)
        .default(0)
        .interact()?;

    if idx == 0 {
        Ok(None)
    } else {
        Ok(Some(procs[idx - 1].0.clone()))
    }
}

fn run_echoed(adb_path: &PathBuf, serial: Option<&str>, args: &[&str]) -> Result<u8> {
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    match serial {
        Some(s) => eprintln!("[adbx] -> adb -s {s} {}", owned.join(" ")),
        None => eprintln!("[adbx] -> adb {}", owned.join(" ")),
    }
    adb::run_passthrough(adb_path, serial, &owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prioritizes_running_app_processes() {
        let sample = "  PID NAME\n   12 [kworker/0:1]\n  501 surfaceflinger\n 1541 com.qytech.app_cadenza\n  939 com.android.systemui\n";
        let procs = parse_running_processes(sample);
        assert_eq!(procs.len(), 3);
        assert_eq!(procs[0], ("939".into(), "com.android.systemui".into()));
        assert_eq!(procs[1], ("1541".into(), "com.qytech.app_cadenza".into()));
        assert_eq!(procs[2], ("501".into(), "surfaceflinger".into()));
    }

    #[test]
    fn builtin_palette_includes_core_actions() {
        let ids: std::collections::HashSet<&str> = ACTIONS.iter().map(|a| a.id).collect();
        assert_eq!(ids.len(), ACTIONS.len());
        for expected in [
            "shell",
            "shell-raw",
            "logcat",
            "logcat-stream",
            "watch",
            "reboot",
            "meminfo",
            "history",
            "port-list",
        ] {
            assert!(ids.contains(expected), "missing action {expected}");
        }
    }
}
