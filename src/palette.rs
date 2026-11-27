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
        id: "logcat",
        title: "打开交互式 Logcat TUI（支持实时正则/包名追 PID）",
        keywords: "logcat 日志 过滤",
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
        id: "clear-restart",
        title: "清除选中应用数据并重新拉起",
        keywords: "clear restart 清除数据 重启 app",
    },
    MacroAction {
        id: "meminfo",
        title: "查看选中应用的内存占用（dumpsys meminfo）",
        keywords: "meminfo 内存 性能",
    },
    MacroAction {
        id: "wifi-adb",
        title: "查看设备局域网 IP 并开启 5555 无线调试",
        keywords: "wifi tcpip 无线 调试 ip",
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
        "meminfo" => {
            let Some(pkg) = pick_optional_package(serial)? else {
                println!("未选择应用包名，已取消。");
                return Ok(0);
            };
            run_echoed(adb_path, Some(serial), &["shell", "dumpsys", "meminfo", &pkg])
        }
        "wifi-adb" => {
            run_echoed(adb_path, Some(serial), &["tcpip", "5555"])?;
            run_echoed(adb_path, Some(serial), &["shell", "ip", "-f", "inet", "addr", "show", "wlan0"])
        }
        "restart-server" => {
            run_echoed(adb_path, None, &["kill-server"])?;
            run_echoed(adb_path, None, &["start-server"])
        }
        other => {
            eprintln!(
                "未知快捷动作: {other}（内置: logcat, screenshot, top-activity, clear-restart, meminfo, wifi-adb, restart-server；或通过 `adbx macro set` 自定义）"
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

fn run_echoed(adb_path: &PathBuf, serial: Option<&str>, args: &[&str]) -> Result<u8> {
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    match serial {
        Some(s) => eprintln!("[adbx] -> adb -s {s} {}", owned.join(" ")),
        None => eprintln!("[adbx] -> adb {}", owned.join(" ")),
    }
    adb::run_passthrough(adb_path, serial, &owned)
}
