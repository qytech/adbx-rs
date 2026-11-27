use crate::adb;
use crate::cache;
use crate::session;
use crate::store::Store;
use clap_complete::engine::CompletionCandidate;
use std::ffi::OsStr;

/// 动态补全 `all`、已配置设备分组、在线设备别名与序列号
pub fn complete_devices(current: &OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let store = Store::load().ok();
    let devices = cache::get_devices_fast();
    let mut out = Vec::new();

    if "all".starts_with(prefix.as_ref()) {
        out.push(CompletionCandidate::new("all").help(Some("全部在线设备并行模式".into())));
    }

    if let Some(st) = &store {
        for (grp, members) in st.list_groups() {
            if grp.starts_with(prefix.as_ref()) {
                out.push(
                    CompletionCandidate::new(grp)
                        .help(Some(format!("设备分组 ({})", members.join(", ")).into())),
                );
            }
        }
    }

    for d in &devices {
        if let Some(st) = &store {
            if let Some(alias) = st.alias_for(&d.serial) {
                if alias.starts_with(prefix.as_ref()) {
                    out.push(
                        CompletionCandidate::new(alias)
                            .help(Some(format!("{} ({})", d.model, d.serial).into())),
                    );
                }
            }
        }
        if d.serial.starts_with(prefix.as_ref()) {
            out.push(
                CompletionCandidate::new(&d.serial)
                    .help(Some(format!("{} [{}]", d.model, d.state).into())),
            );
        }
    }

    out
}

pub fn complete_packages(current: &OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let serial = resolve_completion_serial();
    let Some(serial) = serial else {
        return Vec::new();
    };

    cache::get_packages_fast(&serial)
        .into_iter()
        .filter(|pkg| pkg.starts_with(prefix.as_ref()))
        .map(CompletionCandidate::new)
        .collect()
}

fn resolve_completion_serial() -> Option<String> {
    let store = Store::load().ok();
    let devices = cache::get_devices_fast();
    let online: Vec<_> = devices.into_iter().filter(|d| d.state == "device").collect();

    let all_args: Vec<String> = std::env::args().collect();
    let after_sep: Vec<&str> = match all_args.iter().position(|a| a == "--") {
        Some(idx) => all_args[idx + 1..].iter().map(|s| s.as_str()).collect(),
        None => all_args.iter().map(|s| s.as_str()).collect(),
    };
    if let Some(explicit) = extract_cli_target_from_args(&after_sep) {
        if let Some(st) = &store {
            let devs: Vec<adb::DeviceInfo> = online
                .iter()
                .map(|d| adb::DeviceInfo {
                    serial: d.serial.clone(),
                    state: d.state.clone(),
                    model: d.model.clone(),
                })
                .collect();
            if let Ok(serial) = st.resolve_input_to_serial(&explicit, &devs) {
                return Some(serial);
            }
        } else if let Some(d) = online.iter().find(|d| d.serial == explicit || d.serial.starts_with(&explicit)) {
            return Some(d.serial.clone());
        }
    }

    if let Ok(env_dev) = std::env::var("ADBX_DEVICE") {
        let trimmed = env_dev.trim();
        if !trimmed.is_empty() {
            if let Some(st) = &store {
                for d in &online {
                    if st.alias_for(&d.serial) == Some(trimmed) || d.serial == trimmed {
                        return Some(d.serial.clone());
                    }
                }
            }
        }
    }

    if let Some(st) = &store {
        let sid = session::current_session_id();
        if let Some(locked) = st.get_session_device(&sid) {
            if online.iter().any(|d| d.serial == locked) {
                return Some(locked);
            }
        }
    }

    if online.len() == 1 {
        Some(online[0].serial.clone())
    } else {
        None
    }
}

/// 宿主机 Tab 动态补全 `adbx shell <命令> <远端路径>`（如 `adbx shell cat /proc/asound/`）
pub fn complete_shell_args(current: &OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    if prefix.contains('/') || prefix.starts_with('.') || is_after_shell_command_arg() {
        let Some(serial) = resolve_completion_serial() else {
            return Vec::new();
        };
        return complete_remote_paths(&serial, prefix.as_ref());
    }

    const SHELL_CMDS: &[(&str, &str)] = &[
        ("cat", "查看远端文件内容"),
        ("ls", "列出远端目录内容"),
        ("cd", "切换目录"),
        ("getprop", "读取 Android 系统属性"),
        ("dumpsys", "导出系统服务状态"),
        ("pm", "包管理器 (pm list / pm path)"),
        ("am", "Activity 管理器 (am start / force-stop)"),
        ("settings", "读写系统设置"),
        ("logcat", "查看设备日志"),
        ("dmesg", "查看内核日志"),
        ("ps", "查看进程列表"),
        ("top", "查看进程资源占用"),
        ("ip", "查看网络接口"),
        ("wm", "窗口管理器 (wm size / density)"),
        ("df", "查看磁盘分区空间"),
        ("uname", "查看内核版本信息"),
        ("tinymix", "查看/设置音频混音器"),
        ("tinyplay", "播放 WAV 音频测试"),
        ("input", "模拟按键/触摸输入"),
        ("svc", "控制 Wi-Fi/USB/电源等服务"),
        ("find", "查找远端文件"),
        ("grep", "过滤文本"),
        ("mount", "查看或挂载文件系统"),
        ("id", "查看当前用户权限"),
        ("pidof", "查询进程 PID"),
        ("--raw", "直接进入原生 adb shell PTY"),
    ];

    SHELL_CMDS
        .iter()
        .filter(|(cmd, _)| cmd.starts_with(prefix.as_ref()))
        .map(|(cmd, desc)| CompletionCandidate::new(*cmd).help(Some((*desc).into())))
        .collect()
}

fn is_after_shell_command_arg() -> bool {
    let all_args: Vec<String> = std::env::args().collect();
    let after_sep: Vec<&str> = match all_args.iter().position(|a| a == "--") {
        Some(idx) => all_args[idx + 1..].iter().map(|s| s.as_str()).collect(),
        None => all_args.iter().map(|s| s.as_str()).collect(),
    };
    has_command_before_current(&after_sep)
}

fn extract_cli_target_from_args(args: &[&str]) -> Option<String> {
    let arg1 = args.get(1).copied()?;
    let raw = if matches!(arg1, "--device" | "--target") {
        args.get(2).copied()?
    } else if let Some(v) = arg1
        .strip_prefix("--device=")
        .or_else(|| arg1.strip_prefix("--target="))
    {
        v
    } else {
        arg1.strip_prefix('@')?
    };
    let clean = raw.trim().trim_start_matches('@');
    if clean.is_empty() || clean == "all" || clean == "pick" || clean.contains(',') {
        None
    } else {
        Some(clean.to_string())
    }
}

fn has_command_before_current(args: &[&str]) -> bool {
    let Some(shell_idx) = args.iter().position(|a| *a == "shell") else {
        return false;
    };
    args[shell_idx + 1..].len() >= 2
}

fn split_remote_path(token: &str) -> (&str, &str) {
    match token.rfind('/') {
        Some(idx) => (&token[..=idx], &token[idx + 1..]),
        None => ("", token),
    }
}

fn resolve_remote_dir(dir_part: &str) -> String {
    if dir_part.is_empty() {
        "/".to_string()
    } else if dir_part.starts_with('/') {
        dir_part.to_string()
    } else {
        format!("/{dir_part}")
    }
}

fn complete_remote_paths(serial: &str, token: &str) -> Vec<CompletionCandidate> {
    let Ok(adb_path) = adb::find_adb_binary() else {
        return Vec::new();
    };
    let (dir_part, name_prefix) = split_remote_path(token);
    let list_dir = resolve_remote_dir(dir_part);
    let quoted_dir = format!("'{}'", list_dir.replace('\'', "'\\''"));
    let flags = if name_prefix.starts_with('.') {
        "-1ap"
    } else {
        "-1p"
    };
    let ls_cmd = format!("ls {flags} {quoted_dir} 2>/dev/null");

    let Ok(cap) = adb::run_captured_with_timeout(
        &adb_path,
        Some(serial),
        &["shell".into(), ls_cmd],
        1,
    ) else {
        return Vec::new();
    };
    if cap.exit_code != 0 && cap.stdout.is_empty() {
        return Vec::new();
    }

    String::from_utf8_lossy(&cap.stdout)
        .lines()
        .map(|l| l.trim_end_matches('\r').trim())
        .filter(|e| !e.is_empty() && *e != "./" && *e != "../" && e.starts_with(name_prefix))
        .map(|entry| CompletionCandidate::new(format!("{dir_part}{entry}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_remote_path_and_resolves_dir() {
        assert_eq!(split_remote_path("/proc/asound/"), ("/proc/asound/", ""));
        assert_eq!(resolve_remote_dir("/proc/asound/"), "/proc/asound/");

        assert_eq!(split_remote_path("/proc/asound/c"), ("/proc/asound/", "c"));
        assert_eq!(split_remote_path("proc/asound/c"), ("proc/asound/", "c"));
        assert_eq!(resolve_remote_dir("proc/asound/"), "/proc/asound/");

        assert_eq!(split_remote_path("cards"), ("", "cards"));
        assert_eq!(resolve_remote_dir(""), "/");
    }

    #[test]
    fn detects_command_vs_argument_position_after_shell() {
        assert!(!has_command_before_current(&["adbx", "shell", ""]));
        assert!(!has_command_before_current(&["adbx", "shell", "cat"]));
        assert!(has_command_before_current(&["adbx", "shell", "cat", ""]));
        assert!(has_command_before_current(&[
            "adbx",
            "shell",
            "cat",
            "/proc/asound/"
        ]));
    }

    #[test]
    fn extracts_explicit_device_from_completion_args() {
        assert_eq!(
            extract_cli_target_from_args(&["adbx", "--device", "pixel", "shell", "cat", "/proc/"]),
            Some("pixel".into())
        );
        assert_eq!(
            extract_cli_target_from_args(&["adbx", "--target=mi14", "shell", "ls"]),
            Some("mi14".into())
        );
        assert_eq!(
            extract_cli_target_from_args(&["adbx", "@pixel", "shell", "ls"]),
            Some("pixel".into())
        );
        assert_eq!(
            extract_cli_target_from_args(&["adbx", "--all", "shell", "ls"]),
            None
        );
    }
}
