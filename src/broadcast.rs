use crate::adb::{self, DeviceInfo};
use crate::selector;
use crate::store::Store;
use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, IsTerminal};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Grouped,
    Stream,
}

#[derive(Debug, Clone)]
pub struct BroadcastOptions {
    pub mode_override: Option<OutputMode>,
    pub max_jobs: usize,
    pub timeout_secs: u64,
    pub assume_yes: bool,
    pub no_color: bool,
    pub json_output: bool,
    pub adb_args: Vec<String>,
}

struct TargetDevice {
    index: usize,
    label: String,
    serial: String,
    color_code: &'static str,
}

#[derive(Debug, Serialize)]
struct JsonDeviceResult {
    label: String,
    serial: String,
    exit_code: i32,
    timed_out: bool,
    elapsed_ms: u128,
    stdout: String,
    stderr: String,
}

struct DeviceResult {
    index: usize,
    label: String,
    serial: String,
    color_code: &'static str,
    exit_code: i32,
    timed_out: bool,
    elapsed_ms: u128,
    stdout: String,
    stderr: String,
}

const ANSI_PALETTE: &[&str] = &[
    "\x1b[36m",
    "\x1b[32m",
    "\x1b[33m",
    "\x1b[35m",
    "\x1b[34m",
    "\x1b[96m",
];
const ANSI_RESET: &str = "\x1b[0m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_GREEN: &str = "\x1b[32m";

pub fn parse_broadcast_args(raw: &[String]) -> Result<BroadcastOptions> {
    let mut mode_override = None;
    let mut max_jobs = 8usize;
    let mut timeout_secs = 30u64;
    let mut assume_yes = false;
    let mut no_color = false;
    let mut json_output = false;
    let mut i = 0usize;

    while i < raw.len() {
        match raw[i].as_str() {
            "--stream" => {
                mode_override = Some(OutputMode::Stream);
                i += 1;
            }
            "--grouped" | "--group" => {
                mode_override = Some(OutputMode::Grouped);
                i += 1;
            }
            "-y" | "--yes" => {
                assume_yes = true;
                i += 1;
            }
            "--no-color" => {
                no_color = true;
                i += 1;
            }
            "--json" => {
                json_output = true;
                no_color = true;
                mode_override = Some(OutputMode::Grouped);
                i += 1;
            }
            "-j" | "--jobs" => {
                let Some(val) = raw.get(i + 1) else {
                    bail!("-j / --jobs 需要指定一个正整数，例如 -j 4");
                };
                max_jobs = val
                    .parse::<usize>()
                    .map_err(|_| anyhow::anyhow!("无效并发数: {val}"))?
                    .max(1);
                i += 2;
            }
            "--timeout" => {
                let Some(val) = raw.get(i + 1) else {
                    bail!("--timeout 需要指定超时秒数，例如 --timeout 15");
                };
                timeout_secs = val
                    .parse::<u64>()
                    .map_err(|_| anyhow::anyhow!("无效超时秒数: {val}"))?
                    .max(1);
                i += 2;
            }
            "--" => {
                i += 1;
                break;
            }
            _ => break,
        }
    }

    let adb_args = raw[i..].to_vec();
    if adb_args.is_empty() {
        bail!("请提供要在多台设备上执行的 adb 命令，例如: adbx --all install -r app.apk");
    }

    Ok(BroadcastOptions {
        mode_override,
        max_jobs,
        timeout_secs,
        assume_yes,
        no_color,
        json_output,
        adb_args,
    })
}

pub fn run_broadcast(
    target_spec: &str,
    opts: BroadcastOptions,
    adb_path: &PathBuf,
    store: &Store,
) -> Result<u8> {
    if is_interactive_command(&opts.adb_args) {
        bail!(
            "命令 '{}' 属于交互式会话，不支持多设备并发广播；请带上具体指令，例如: adbx @{target_spec} shell getprop ro.product.model",
            opts.adb_args.join(" ")
        );
    }

    let all_devices = adb::list_devices(adb_path)?;
    let (online, skipped): (Vec<_>, Vec<_>) =
        all_devices.into_iter().partition(|d| d.state == "device");

    if !skipped.is_empty() && !opts.json_output {
        let skipped_names: Vec<String> = skipped
            .iter()
            .map(|d| format!("{}[{}]", store.display_label(&d.serial), d.state))
            .collect();
        eprintln!("提示: 已自动跳过未就绪设备 -> {}", skipped_names.join(", "));
    }

    if online.is_empty() {
        bail!("当前没有在线的 ADB 设备");
    }

    let selected_devices: Vec<DeviceInfo> = if target_spec == "all" {
        online
    } else if target_spec == "pick" {
        selector::pick_multiple_devices(&online, store)?
    } else if store.group_members(target_spec).is_some() {
        store.expand_group_online(target_spec, &online)?
    } else {
        let mut list = Vec::new();
        for token in target_spec
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            if store.group_members(token).is_some() {
                for d in store.expand_group_online(token, &online)? {
                    if !list.iter().any(|x: &DeviceInfo| x.serial == d.serial) {
                        list.push(d);
                    }
                }
            } else {
                let serial = store.resolve_input_to_serial(token, &online)?;
                if !list.iter().any(|d: &DeviceInfo| d.serial == serial) {
                    if let Some(found) = online.iter().find(|d| d.serial == serial) {
                        list.push(found.clone());
                    }
                }
            }
        }
        if list.is_empty() {
            bail!("未匹配到任何在线设备: @{target_spec}");
        }
        list
    };

    let targets: Vec<TargetDevice> = selected_devices
        .into_iter()
        .enumerate()
        .map(|(idx, d)| TargetDevice {
            index: idx,
            label: store.display_label(&d.serial),
            serial: d.serial,
            color_code: ANSI_PALETTE[idx % ANSI_PALETTE.len()],
        })
        .collect();

    if !opts.assume_yes && adb::is_dangerous_command(&opts.adb_args) {
        let preview_list: Vec<(String, String)> = targets
            .iter()
            .map(|t| (t.label.clone(), t.serial.clone()))
            .collect();
        let cmd_preview = opts.adb_args.join(" ");
        if !selector::confirm_batch_dangerous(&preview_list, &cmd_preview)? {
            println!("已取消批量执行。");
            return Ok(0);
        }
    }

    let mode = if opts.json_output {
        OutputMode::Grouped
    } else {
        opts.mode_override
            .unwrap_or_else(|| default_mode_for(&opts.adb_args))
    };

    match mode {
        OutputMode::Grouped => run_grouped(adb_path, &targets, &opts),
        OutputMode::Stream => run_streamed(adb_path, &targets, &opts),
    }
}

fn is_interactive_command(args: &[String]) -> bool {
    if args.is_empty() {
        return true;
    }
    matches!(args[0].as_str(), "log" | "menu" | "logcat" | "mcp")
        || (args[0] == "shell" && args.len() == 1)
}

fn default_mode_for(args: &[String]) -> OutputMode {
    if let Some(first) = args.first() {
        if matches!(
            first.as_str(),
            "install" | "install-multiple" | "push" | "pull" | "sync"
        ) {
            return OutputMode::Stream;
        }
    }
    OutputMode::Grouped
}

fn run_grouped(adb_path: &PathBuf, targets: &[TargetDevice], opts: &BroadcastOptions) -> Result<u8> {
    let queue = Mutex::new(targets.iter().collect::<VecDeque<_>>());
    let results = Mutex::new(Vec::<DeviceResult>::with_capacity(targets.len()));
    let worker_count = opts.max_jobs.min(targets.len()).max(1);

    thread::scope(|s| {
        for _ in 0..worker_count {
            s.spawn(|| loop {
                let target = queue.lock().unwrap().pop_front();
                let Some(target) = target else { break };

                // 修复：复用带硬超时的执行器，单台 USB 假死不会拖死其余设备
                let res = match adb::run_captured_with_timeout(
                    adb_path,
                    Some(&target.serial),
                    &opts.adb_args,
                    opts.timeout_secs,
                ) {
                    Ok(cap) => DeviceResult {
                        index: target.index,
                        label: target.label.clone(),
                        serial: target.serial.clone(),
                        color_code: target.color_code,
                        exit_code: cap.exit_code,
                        timed_out: cap.timed_out,
                        elapsed_ms: cap.elapsed_ms,
                        stdout: String::from_utf8_lossy(&cap.stdout).to_string(),
                        stderr: String::from_utf8_lossy(&cap.stderr).to_string(),
                    },
                    Err(e) => DeviceResult {
                        index: target.index,
                        label: target.label.clone(),
                        serial: target.serial.clone(),
                        color_code: target.color_code,
                        exit_code: 1,
                        timed_out: false,
                        elapsed_ms: 0,
                        stdout: String::new(),
                        stderr: format!("启动 adb 失败: {e}"),
                    },
                };

                results.lock().unwrap().push(res);
            });
        }
    });

    let mut list = results.into_inner().unwrap();
    list.sort_by_key(|r| r.index);

    let ok_count = list.iter().filter(|r| r.exit_code == 0 && !r.timed_out).count();
    let fail_count = list.len().saturating_sub(ok_count);

    if opts.json_output {
        let json_results: Vec<JsonDeviceResult> = list
            .into_iter()
            .map(|r| JsonDeviceResult {
                label: r.label,
                serial: r.serial,
                exit_code: r.exit_code,
                timed_out: r.timed_out,
                elapsed_ms: r.elapsed_ms,
                stdout: r.stdout,
                stderr: r.stderr,
            })
            .collect();
        let payload = serde_json::json!({
            "summary": { "ok": ok_count, "fail": fail_count },
            "results": json_results
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(if fail_count == 0 { 0 } else { 1 });
    }

    let stdout_is_tty = std::io::stdout().is_terminal();
    let use_color = !opts.no_color && stdout_is_tty;

    if !stdout_is_tty && list.len() > 1 {
        for line in format_piped_grouped_lines(&list) {
            println!("{line}");
        }
        for r in &list {
            if r.timed_out {
                eprintln!("[{}] TIMEOUT ({}s)", r.label, opts.timeout_secs);
            }
            if !r.stderr.trim().is_empty() {
                for line in r.stderr.lines() {
                    eprintln!("[{}] stderr: {line}", r.label);
                }
            }
        }
        eprintln!("--- 汇总: {ok_count} 成功, {fail_count} 失败 ---");
        return Ok(if fail_count == 0 { 0 } else { 1 });
    }

    for r in &list {
        let secs = r.elapsed_ms as f64 / 1000.0;
        let status_str = if r.timed_out {
            if !use_color {
                format!("TIMEOUT ({}s)", opts.timeout_secs)
            } else {
                format!("{ANSI_RED}TIMEOUT ({}s){ANSI_RESET}", opts.timeout_secs)
            }
        } else if r.exit_code == 0 {
            if !use_color {
                format!("OK · {secs:.2}s")
            } else {
                format!("{ANSI_GREEN}OK{ANSI_RESET} · {secs:.2}s")
            }
        } else if !use_color {
            format!("FAIL (exit={}) · {secs:.2}s", r.exit_code)
        } else {
            format!("{ANSI_RED}FAIL (exit={}){ANSI_RESET} · {secs:.2}s", r.exit_code)
        };

        if !use_color {
            println!("=== [{}] ({})  {} ===", r.label, r.serial, status_str);
        } else {
            println!(
                "=== {}[{}]{} ({})  {} ===",
                r.color_code, r.label, ANSI_RESET, r.serial, status_str
            );
        }

        if !r.stdout.trim().is_empty() {
            print!("{}", r.stdout);
            if !r.stdout.ends_with('\n') {
                println!();
            }
        }
        if !r.stderr.trim().is_empty() {
            for line in r.stderr.lines() {
                eprintln!("stderr: {line}");
            }
        }
        println!();
    }

    println!("--- 汇总: {ok_count} 成功, {fail_count} 失败 ---");
    Ok(if fail_count == 0 { 0 } else { 1 })
}

fn format_piped_grouped_lines(results: &[DeviceResult]) -> Vec<String> {
    let label_width = results
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(8);
    let mut lines = Vec::new();
    for r in results {
        for raw_line in r.stdout.lines() {
            let line = raw_line.trim_end_matches('\r');
            lines.push(format!("[{:<width$}] {line}", r.label, width = label_width));
        }
    }
    lines
}

fn run_streamed(adb_path: &PathBuf, targets: &[TargetDevice], opts: &BroadcastOptions) -> Result<u8> {
    let stdout_is_tty = std::io::stdout().is_terminal();
    let use_color = !opts.no_color && stdout_is_tty;
    let label_width = targets.iter().map(|t| t.label.chars().count()).max().unwrap_or(8);
    let queue = Mutex::new(targets.iter().collect::<VecDeque<_>>());
    let summary = Mutex::new((0usize, 0usize));
    let worker_count = opts.max_jobs.min(targets.len()).max(1);
    let (tx, rx) = mpsc::channel::<String>();

    thread::scope(|s| {
        let printer = s.spawn(move || {
            for line in rx {
                println!("{line}");
            }
        });

        for _ in 0..worker_count {
            let tx_worker = tx.clone();
            let queue_ref = &queue;
            let summary_ref = &summary;
            s.spawn(move || loop {
                let target = queue_ref.lock().unwrap().pop_front();
                let Some(target) = target else { break };

                let prefix = if !use_color {
                    format!("[{:<width$}]", target.label, width = label_width)
                } else {
                    format!(
                        "{}[{:<width$}]{}",
                        target.color_code,
                        target.label,
                        ANSI_RESET,
                        width = label_width
                    )
                };

                let child = Command::new(adb_path)
                    .arg("-s")
                    .arg(&target.serial)
                    .args(&opts.adb_args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn();

                let mut child = match child {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx_worker.send(format!("{prefix} 启动 adb 失败: {e}"));
                        summary_ref.lock().unwrap().1 += 1;
                        continue;
                    }
                };

                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let mut timed_out = false;
                let deadline = Duration::from_secs(opts.timeout_secs.max(1));
                let started = Instant::now();

                let mut ok = false;
                thread::scope(|inner| {
                    if let Some(out) = stdout {
                        let tx_out = tx_worker.clone();
                        let p = prefix.clone();
                        inner.spawn(move || {
                            for line in BufReader::new(out).lines().map_while(Result::ok) {
                                let _ = tx_out.send(format!("{p} {line}"));
                            }
                        });
                    }
                    if let Some(err) = stderr {
                        let tx_err = tx_worker.clone();
                        let p = prefix.clone();
                        inner.spawn(move || {
                            for line in BufReader::new(err).lines().map_while(Result::ok) {
                                let _ = tx_err.send(format!("{p} [stderr] {line}"));
                            }
                        });
                    }

                    loop {
                        match child.try_wait() {
                            Ok(Some(st)) => {
                                ok = st.success();
                                break;
                            }
                            Ok(None) => {
                                if started.elapsed() >= deadline {
                                    timed_out = true;
                                    let _ = child.kill();
                                    let _ = child.wait();
                                    let _ = tx_worker.send(format!(
                                        "{prefix} [TIMEOUT {}s - 已中止该设备命令]",
                                        opts.timeout_secs
                                    ));
                                    break;
                                }
                                thread::sleep(Duration::from_millis(50));
                            }
                            Err(_) => break,
                        }
                    }
                });
                let ok = ok && !timed_out;
                let mut guard = summary_ref.lock().unwrap();
                if ok {
                    guard.0 += 1;
                } else {
                    guard.1 += 1;
                }
            });
        }

        drop(tx);
        let _ = printer.join();
    });

    let (ok_count, fail_count) = *summary.lock().unwrap();
    if stdout_is_tty {
        println!("--- 汇总: {ok_count} 成功, {fail_count} 失败 ---");
    } else {
        eprintln!("--- 汇总: {ok_count} 成功, {fail_count} 失败 ---");
    }
    Ok(if fail_count == 0 { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_broadcast_flags_including_timeout() {
        let raw: Vec<String> = ["--stream", "-j", "2", "--timeout", "15", "--no-color", "install", "-r", "app.apk"]
            .into_iter()
            .map(String::from)
            .collect();
        let opts = parse_broadcast_args(&raw).unwrap();
        assert_eq!(opts.mode_override, Some(OutputMode::Stream));
        assert_eq!(opts.max_jobs, 2);
        assert_eq!(opts.timeout_secs, 15);
        assert!(opts.no_color);
        assert_eq!(opts.adb_args, vec!["install", "-r", "app.apk"]);

        let grouped_raw: Vec<String> = ["--grouped", "shell", "id"].into_iter().map(String::from).collect();
        let grouped = parse_broadcast_args(&grouped_raw).unwrap();
        assert_eq!(grouped.mode_override, Some(OutputMode::Grouped));
    }

    #[test]
    fn formats_piped_grouped_output_with_per_line_device_prefix() {
        let results = vec![
            DeviceResult {
                index: 0,
                label: "cayin_dmp".into(),
                serial: "7c9349cc".into(),
                color_code: ANSI_PALETTE[0],
                exit_code: 0,
                timed_out: false,
                elapsed_ms: 10,
                stdout: "[service.adb.root]: [1]\n[ro.product.model]: [cayin]\r\n".into(),
                stderr: String::new(),
            },
            DeviceResult {
                index: 1,
                label: "mi14".into(),
                serial: "9a8b7c6d".into(),
                color_code: ANSI_PALETTE[1],
                exit_code: 0,
                timed_out: false,
                elapsed_ms: 12,
                stdout: "[service.adb.root]: [0]\n".into(),
                stderr: String::new(),
            },
        ];

        let formatted = format_piped_grouped_lines(&results);
        assert_eq!(
            formatted,
            vec![
                "[cayin_dmp] [service.adb.root]: [1]",
                "[cayin_dmp] [ro.product.model]: [cayin]",
                "[mi14     ] [service.adb.root]: [0]",
            ]
        );
    }
}
