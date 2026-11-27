use crate::adb;
use crate::broadcast;
use crate::selector;
use crate::store::{self, Store};
use anyhow::{bail, Result};
use chrono::Local;
use dialoguer::{Confirm, FuzzySelect};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_HISTORY_ENTRIES: usize = 1_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub timestamp: String,
    pub target: String,
    /// Exact adb serials at execution time. Older records only stored a display label.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serials: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    pub args: Vec<String>,
    pub exit_code: u8,
    pub elapsed_ms: u128,
}

fn needs_host_directory(args: &[String]) -> bool {
    let args = adb::after_wait_prefix(args);
    let Some((cmd, rest)) = args.split_first() else {
        return false;
    };
    let relative = |arg: &String| Path::new(arg).is_relative();
    match cmd.as_str() {
        "push" | "pull" => {
            let Ok(indices) = crate::pull::transfer_operands(args) else {
                return true;
            };
            let Some((last, sources)) = indices.split_last() else {
                return false;
            };
            if cmd == "pull" {
                sources.is_empty() || relative(&args[*last])
            } else {
                sources.iter().any(|i| relative(&args[*i]))
            }
        }
        "install" | "restore" | "sideload" | "keygen" | "pubkey" => {
            rest.last().is_some_and(relative)
        }
        "install-multiple" | "install-multi-package" => {
            rest.iter().any(|s| !s.starts_with('-') && relative(s))
        }
        "bugreport" => rest.first().is_none_or(relative),
        "backup" => rest
            .windows(2)
            .find(|pair| pair[0] == "-f")
            .is_none_or(|pair| relative(&pair[1])),
        "sync" => {
            std::env::var_os("ANDROID_PRODUCT_OUT").is_some_and(|out| Path::new(&out).is_relative())
        }
        _ => false,
    }
}

/// 追加一条命令历史到 history.jsonl（超限时自动滚动裁剪保留最近 1000 条）
pub fn record(
    store: &Store,
    target: &str,
    serials: &[String],
    args: &[String],
    exit_code: u8,
    elapsed_ms: u128,
) {
    if args.is_empty() {
        return;
    }
    let entry = HistoryEntry {
        timestamp: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        target: target.to_string(),
        serials: serials.to_vec(),
        cwd: if needs_host_directory(args) {
            std::env::current_dir().ok()
        } else {
            None
        },
        args: args.to_vec(),
        exit_code,
        elapsed_ms,
    };
    let path = store.history_path();
    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = store::with_store_lock(dir, || {
            let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
            writeln!(file, "{line}")?;
            drop(file);
            trim_if_needed(&path);
            Ok(())
        });
    }
}

fn trim_if_needed(path: &Path) {
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() > MAX_HISTORY_ENTRIES + 100 {
        let kept = &lines[lines.len() - MAX_HISTORY_ENTRIES..];
        let content = format!("{}\n", kept.join("\n"));
        let _ = store::atomic_write(path, &content);
    }
}

pub fn load_entries(store: &Store) -> Vec<HistoryEntry> {
    let path = store.history_path();
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_jsonl(&raw)
}

pub fn parse_jsonl(raw: &str) -> Vec<HistoryEntry> {
    raw.lines()
        .filter_map(|line| serde_json::from_str::<HistoryEntry>(line).ok())
        .collect()
}

pub fn filter_entries(
    entries: Vec<HistoryEntry>,
    device: Option<&str>,
    grep: Option<&str>,
    limit: usize,
) -> Vec<HistoryEntry> {
    let dev_q = device.map(|s| s.trim_start_matches('@').to_ascii_lowercase());
    let grep_q = grep.map(|s| s.to_ascii_lowercase());

    let mut matched: Vec<HistoryEntry> = entries
        .into_iter()
        .filter(|e| {
            if let Some(ref dq) = dev_q {
                if !e.target.to_ascii_lowercase().contains(dq)
                    && !e
                        .serials
                        .iter()
                        .any(|s| s.to_ascii_lowercase().contains(dq))
                {
                    return false;
                }
            }
            if let Some(ref gq) = grep_q {
                let cmd = e.args.join(" ").to_ascii_lowercase();
                if !cmd.contains(gq) && !e.target.to_ascii_lowercase().contains(gq) {
                    return false;
                }
            }
            true
        })
        .collect();

    if matched.len() > limit {
        matched = matched.split_off(matched.len() - limit);
    }
    matched
}

pub fn list_history(
    store: &Store,
    limit: usize,
    device: Option<&str>,
    grep: Option<&str>,
    json: bool,
) -> Result<u8> {
    let filtered = filter_entries(load_entries(store), device, grep, limit.max(1));
    if json {
        println!("{}", serde_json::to_string_pretty(&filtered)?);
        return Ok(0);
    }
    if filtered.is_empty() {
        println!("暂无匹配的命令历史记录。");
        return Ok(0);
    }
    println!(
        "{:<19} {:<14} {:<5} {:<8} COMMAND",
        "TIME", "TARGET", "EXIT", "ELAPSED"
    );
    for e in &filtered {
        println!(
            "{:<19} {:<14} {:<5} {:<8} adb {}",
            e.timestamp,
            e.target,
            e.exit_code,
            format!("{}ms", e.elapsed_ms),
            e.args.join(" ")
        );
    }
    Ok(0)
}

/// 交互式模糊搜索历史命令并一键重放
pub fn replay_history(
    adb_path: &std::path::PathBuf,
    store: &Store,
    limit: usize,
    explicit_serials: &[String],
    explicit_cwd: Option<&Path>,
) -> Result<u8> {
    if !selector::is_interactive_tty() {
        bail!("非交互终端不支持 `adbx history replay`，请使用 `adbx history list --json`");
    }
    let mut entries = filter_entries(load_entries(store), None, None, limit.max(1));
    if entries.is_empty() {
        println!("暂无可重放的历史命令。");
        return Ok(0);
    }
    entries.reverse(); // 最新执行的排在最前面

    let items: Vec<String> = entries
        .iter()
        .map(|e| {
            let targets = match e.serials.as_slice() {
                [] => format!("{}（旧记录，未保存序列号）", e.target),
                [single] if single == &e.target => e.target.clone(),
                serials => format!("{} [{}]", e.target, serials.join(", ")),
            };
            format!(
                "{} | {:<12} | exit={} | adb {}",
                e.timestamp,
                targets,
                e.exit_code,
                e.args.join(" ")
            )
        })
        .collect();

    let idx = FuzzySelect::new()
        .with_prompt("选择要重新执行的历史命令（支持关键词模糊过滤）")
        .items(&items)
        .default(0)
        .interact()?;

    let chosen = &entries[idx];
    let serials = if explicit_serials.is_empty() {
        &chosen.serials
    } else {
        explicit_serials
    };
    replay_command(
        adb_path,
        store,
        serials,
        &chosen.args,
        explicit_cwd.or(chosen.cwd.as_deref()),
    )
}

/// Replay and exported scripts share the exact-match preflight, including for a
/// batch: one missing original target must stop the whole command.
pub fn replay_command(
    adb_path: &std::path::PathBuf,
    store: &Store,
    serials: &[String],
    args: &[String],
    cwd: Option<&Path>,
) -> Result<u8> {
    if serials.is_empty() {
        bail!("缺少原始设备序列号。要重放旧记录，请使用 adbx history replay --serial <完整序列号> 显式选定设备");
    }
    if args.first().is_none_or(|a| a.starts_with('-'))
        || adb::is_global_or_explicit_adb_command(args)
    {
        bail!("历史重放需要绑定设备的 adb 子命令");
    }
    let cwd = if cwd.is_some() || needs_host_directory(args) {
        let dir = cwd.ok_or_else(|| {
            anyhow::anyhow!("这条命令使用本地相对路径，但未记录原工作目录。请用 --cwd 显式指定目录")
        })?;
        if !dir.is_dir() {
            bail!("历史重放工作目录不存在或不是目录：{}", dir.display());
        }
        Some(std::path::absolute(dir)?)
    } else {
        None
    };
    // Resolve the executable before changing a child process's working directory.
    let adb_path = &std::path::absolute(adb_path)?;
    let online = adb::list_devices(adb_path)?;
    let mut targets = Vec::new();
    for serial in serials {
        if !online
            .iter()
            .any(|d| d.state == "device" && &d.serial == serial)
        {
            bail!("历史重放目标 {serial} 未在线，本次重放已停止");
        }
        if !targets.contains(serial) {
            targets.push(serial.clone());
        }
    }
    if let [serial] = targets.as_slice() {
        let preview = args.join(" ");
        if adb::is_dangerous_command(args)
            && !selector::confirm_dangerous(&store.display_label(serial), serial, &preview)?
        {
            return Ok(0);
        }
        eprintln!("[adbx replay] -> adb -s {serial} {preview}");
        return adb::run_passthrough_in(adb_path, Some(serial), args, cwd.as_deref());
    }
    let mut opts = broadcast::parse_broadcast_args(args)?;
    opts.cwd = cwd;
    broadcast::run_on_serials(&targets, opts, adb_path, store, None)
}

pub fn export_history(store: &Store, out_file: &str) -> Result<u8> {
    let entries = load_entries(store);
    let content = if out_file.ends_with(".json") {
        serde_json::to_string_pretty(&entries)?
    } else if out_file.ends_with(".sh") {
        let mut lines = vec!["#!/usr/bin/env bash".to_string(), "set -e".to_string()];
        for e in &entries {
            lines.extend(
                format!("{} [{}]", e.timestamp, e.target)
                    .lines()
                    .map(|s| format!("# {s}")),
            );
            if e.serials.is_empty() {
                lines.push(
                    "# 旧记录未保存序列号，请先在此行的 -- 前添加 --serial <完整序列号>".into(),
                );
            }
            let mut parts = vec!["adbx history replay".to_owned()];
            for serial in &e.serials {
                parts.push(format!("--serial={}", shell_escape_arg(serial)));
            }
            if let Some(dir) = &e.cwd {
                let path = dir
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("无法导出非 UTF-8 工作目录"))?;
                parts.push(format!("--cwd={}", shell_escape_arg(path)));
            } else if needs_host_directory(&e.args) {
                lines.push("# 旧记录未保存工作目录，请添加 --cwd <目录> 后再运行".into());
            }
            parts.push("--".into());
            parts.extend(e.args.iter().map(|a| shell_escape_arg(a)));
            lines.push(parts.join(" "));
        }
        format!("{}\n", lines.join("\n"))
    } else {
        let path = store.history_path();
        if path.exists() {
            fs::read_to_string(path)?
        } else {
            String::new()
        }
    };
    store::atomic_write(Path::new(out_file), &content)?;
    println!("已导出 {} 条历史记录到: {out_file}", entries.len());
    Ok(0)
}

pub fn clear_history(store: &Store, assume_yes: bool) -> Result<u8> {
    if !assume_yes {
        if !selector::is_interactive_tty() {
            bail!("非交互终端清空历史需显式传入 `-y` / `--yes`");
        }
        let ok = Confirm::new()
            .with_prompt("确认清空全部 adbx 命令历史记录？")
            .default(false)
            .interact()?;
        if !ok {
            println!("已取消。");
            return Ok(0);
        }
    }
    let path = store.history_path();
    store::with_store_lock(
        path.parent().expect("history file has a config directory"),
        || match fs::remove_file(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => Ok(result?),
        },
    )?;
    println!("已清空命令历史文件。");
    Ok(0)
}

fn shell_escape_arg(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '=' | '@' | ',' | '+'))
    {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_filters_jsonl_history() {
        let raw = r#"{"timestamp":"2026-11-26 23:00:00","target":"pixel","args":["shell","getprop"],"exit_code":0,"elapsed_ms":120}
{"timestamp":"2026-11-26 23:01:00","target":"mi","args":["install","-r","app.apk"],"exit_code":0,"elapsed_ms":1800}"#;
        let all = parse_jsonl(raw);
        assert_eq!(all.len(), 2);
        let filtered = filter_entries(all, Some("mi"), Some("install"), 10);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].target, "mi");
    }

    #[test]
    fn escapes_shell_arguments_safely() {
        assert_eq!(shell_escape_arg("getprop"), "getprop");
        assert_eq!(shell_escape_arg("ro.product.model"), "ro.product.model");
        assert_eq!(shell_escape_arg("echo a && b"), "'echo a && b'");
        assert_eq!(shell_escape_arg("it's"), "'it'\\''s'");
    }
}
