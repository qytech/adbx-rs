use crate::adb;
use crate::selector;
use crate::store::{self, Store};
use anyhow::{bail, Result};
use chrono::Local;
use dialoguer::{Confirm, FuzzySelect};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

const MAX_HISTORY_ENTRIES: usize = 1_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub timestamp: String,
    pub target: String,
    pub args: Vec<String>,
    pub exit_code: u8,
    pub elapsed_ms: u128,
}

/// 追加一条命令历史到 history.jsonl（超限时自动滚动裁剪保留最近 1000 条）
pub fn record(store: &Store, target: &str, args: &[String], exit_code: u8, elapsed_ms: u128) {
    if args.is_empty() {
        return;
    }
    let entry = HistoryEntry {
        timestamp: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        target: target.to_string(),
        args: args.to_vec(),
        exit_code,
        elapsed_ms,
    };
    let path = store.history_path();
    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
    trim_if_needed(&path);
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
                if !e.target.to_ascii_lowercase().contains(dq) {
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
            format!(
                "{} | {:<12} | exit={} | adb {}",
                e.timestamp,
                e.target,
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
    let online: Vec<_> = adb::list_devices(adb_path)?
        .into_iter()
        .filter(|d| d.state == "device")
        .collect();

    let target_serial = store
        .resolve_input_to_serial(chosen.target.trim_start_matches('@'), &online)
        .ok();

    eprintln!(
        "[adbx replay] -> adb {} {}",
        target_serial
            .as_deref()
            .map(|s| format!("-s {s}"))
            .unwrap_or_default(),
        chosen.args.join(" ")
    );
    adb::run_passthrough(adb_path, target_serial.as_deref(), &chosen.args)
}

pub fn export_history(store: &Store, out_file: &str) -> Result<u8> {
    let entries = load_entries(store);
    let content = if out_file.ends_with(".json") {
        serde_json::to_string_pretty(&entries)?
    } else if out_file.ends_with(".sh") {
        let mut lines = vec!["#!/usr/bin/env bash".to_string()];
        for e in &entries {
            lines.push(format!("# {} [{}]", e.timestamp, e.target));
            let clean_target = e.target.trim_start_matches('@');
            let escaped_args: Vec<String> = e.args.iter().map(|a| shell_escape_arg(a)).collect();
            if clean_target.eq_ignore_ascii_case("all") {
                lines.push(format!("adbx --all {}", escaped_args.join(" ")));
            } else {
                lines.push(format!(
                    "adbx --device {} {}",
                    shell_escape_arg(clean_target),
                    escaped_args.join(" ")
                ));
            }
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
    let _ = fs::remove_file(store.history_path());
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
