use crate::adb;
use crate::logcat::LogLevel;
use crate::logcat_snapshot;
use crate::store::Store;
use anyhow::{bail, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;

const MAX_TEXT_BYTES: usize = 16_384;

pub fn run_stdio_server(allow_destructive: bool) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    eprintln!("[adbx-mcp] stdio server started (allow_destructive={allow_destructive})");

    for line in stdin.lock().lines() {
        let raw = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let err_resp = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {e}") }
                });
                write_frame(&mut stdout, &err_resp)?;
                continue;
            }
        };

        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

        if id.is_none() {
            continue;
        }
        let id = id.unwrap();

        let response = match dispatch(method, &params, allow_destructive) {
            Ok(result) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32603, "message": format!("{err:#}") }
            }),
        };

        write_frame(&mut stdout, &response)?;
    }

    Ok(())
}

fn write_frame(stdout: &mut io::Stdout, payload: &Value) -> Result<()> {
    writeln!(stdout, "{}", serde_json::to_string(payload)?)?;
    stdout.flush()?;
    Ok(())
}

fn dispatch(method: &str, params: &Value, allow_destructive: bool) -> Result<Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-11-25",
            "capabilities": { "tools": {}, "resources": {} },
            "serverInfo": { "name": "adbx", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => handle_tool_call(params, allow_destructive),
        "resources/list" => Ok(json!({
            "resources": [{
                "uri": "adbx://devices",
                "name": "Connected Android Devices",
                "description": "List of currently connected ADB devices, aliases, and groups",
                "mimeType": "application/json"
            }]
        })),
        "resources/read" => handle_resource_read(params),
        _ => bail!("Method not found: {method}"),
    }
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_devices",
            "description": "List all connected Android ADB devices with aliases, groups, serials, states, and models.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "destructiveHint": false }
        },
        {
            "name": "get_logcat",
            "description": "Fetch a filtered snapshot of recent Android logcat lines without blocking.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": { "type": "string" },
                    "pkg": { "type": "string" },
                    "level": { "type": "string", "enum": ["V", "D", "I", "W", "E", "F"], "default": "I" },
                    "regex": { "type": "string" },
                    "exclude": { "type": "string" },
                    "max_lines": { "type": "integer", "minimum": 1, "maximum": 500, "default": 150 }
                }
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false }
        },
        {
            "name": "take_screenshot",
            "description": "Capture the current Android device screen as a PNG image for multimodal inspection.",
            "inputSchema": {
                "type": "object",
                "properties": { "device": { "type": "string" } }
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false }
        },
        {
            "name": "dump_ui_tree",
            "description": "Dump the current Android foreground UI hierarchy XML via uiautomator.",
            "inputSchema": {
                "type": "object",
                "properties": { "device": { "type": "string" } }
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false }
        },
        {
            "name": "ui_tap",
            "description": "Tap screen coordinates (x, y) on an Android device.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": { "type": "string" },
                    "x": { "type": "integer", "minimum": 0 },
                    "y": { "type": "integer", "minimum": 0 }
                },
                "required": ["x", "y"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false }
        },
        {
            "name": "ui_swipe",
            "description": "Swipe from (x1, y1) to (x2, y2) over duration_ms milliseconds.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": { "type": "string" },
                    "x1": { "type": "integer", "minimum": 0 },
                    "y1": { "type": "integer", "minimum": 0 },
                    "x2": { "type": "integer", "minimum": 0 },
                    "y2": { "type": "integer", "minimum": 0 },
                    "duration_ms": { "type": "integer", "minimum": 50, "maximum": 5000, "default": 300 }
                },
                "required": ["x1", "y1", "x2", "y2"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false }
        },
        {
            "name": "ui_input",
            "description": "Type ASCII text into the currently focused Android input field (spaces escaped safely).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["text"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false }
        },
        {
            "name": "ui_keyevent",
            "description": "Send an Android hardware/system keyevent (BACK, HOME, RECENTS, ENTER, WAKEUP, SLEEP, POWER, VOLUME_UP, VOLUME_DOWN, TAB, DEL, or numeric keycode 0..400).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": { "type": "string" },
                    "key": { "type": "string", "description": "Named key (e.g. BACK, HOME, WAKEUP) or numeric keycode." }
                },
                "required": ["key"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false }
        },
        {
            "name": "run_shell",
            "description": "Execute a non-interactive `adb shell <command>` on a device, group name, or \"all\" concurrently. In safe mode, only allowlisted read-only commands are permitted.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": { "type": "string", "description": "Device alias, serial, group name, or \"all\"." },
                    "command": { "type": "string" },
                    "timeout_sec": { "type": "integer", "minimum": 1, "maximum": 60, "default": 15 }
                },
                "required": ["command"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true }
        },
        {
            "name": "app_action",
            "description": "Perform a debugging lifecycle action on an Android app package: launch, stop, or clear.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": { "type": "string" },
                    "pkg": { "type": "string" },
                    "action": { "type": "string", "enum": ["launch", "stop", "clear"] }
                },
                "required": ["pkg", "action"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true }
        }
    ])
}

fn handle_resource_read(params: &Value) -> Result<Value> {
    let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
    if uri != "adbx://devices" {
        bail!("Unknown resource URI: {uri}");
    }
    let adb_path = adb::find_adb_binary()?;
    let store = Store::load()?;
    let list = build_device_json(&adb_path, &store)?;
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": "application/json",
            "text": serde_json::to_string_pretty(&list)?
        }]
    }))
}

fn handle_tool_call(params: &Value, allow_destructive: bool) -> Result<Value> {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

    let adb_path = adb::find_adb_binary()?;
    let store = Store::load()?;

    match name {
        "list_devices" => {
            let list = build_device_json(&adb_path, &store)?;
            Ok(text_tool_result(serde_json::to_string_pretty(&list)?, false))
        }
        "get_logcat" => {
            let serial = resolve_mcp_device(&adb_path, &store, args.get("device").and_then(|v| v.as_str()))?;
            let pkg = args.get("pkg").and_then(|v| v.as_str());
            let level = LogLevel::from_str_lossy(args.get("level").and_then(|v| v.as_str()).unwrap_or("I"));
            let regex = args.get("regex").and_then(|v| v.as_str()).unwrap_or("");
            let exclude = args.get("exclude").and_then(|v| v.as_str()).unwrap_or("");
            let max_lines = args.get("max_lines").and_then(|v| v.as_u64()).unwrap_or(150) as usize;

            let snap = logcat_snapshot::fetch_snapshot(
                &adb_path, &serial, pkg, level, regex, exclude, max_lines,
            )?;
            let mut text = serde_json::to_string_pretty(&snap)?;
            text = truncate_utf8(&text, MAX_TEXT_BYTES * 2);
            Ok(text_tool_result(text, false))
        }
        "take_screenshot" => {
            let serial = resolve_mcp_device(&adb_path, &store, args.get("device").and_then(|v| v.as_str()))?;
            let cap = adb::run_captured_with_timeout(
                &adb_path,
                Some(&serial),
                &["exec-out".into(), "screencap".into(), "-p".into()],
                15,
            )?;
            if cap.exit_code != 0 || cap.stdout.is_empty() {
                return Ok(text_tool_result(
                    format!("Screenshot failed: {}", String::from_utf8_lossy(&cap.stderr)),
                    true,
                ));
            }
            let b64 = base64::engine::general_purpose::STANDARD.encode(&cap.stdout);
            Ok(json!({
                "content": [
                    { "type": "image", "mimeType": "image/png", "data": b64 },
                    { "type": "text", "text": format!("Captured screenshot from {} ({} KB PNG)", store.display_label(&serial), cap.stdout.len() / 1024) }
                ],
                "isError": false
            }))
        }
        "dump_ui_tree" => {
            let serial = resolve_mcp_device(&adb_path, &store, args.get("device").and_then(|v| v.as_str()))?;
            let remote = "/sdcard/adbx_window_dump.xml";
            let _ = adb::run_captured_with_timeout(
                &adb_path,
                Some(&serial),
                &["shell".into(), "uiautomator".into(), "dump".into(), remote.into()],
                15,
            )?;
            let cap = adb::run_captured_with_timeout(
                &adb_path,
                Some(&serial),
                &["shell".into(), "cat".into(), remote.into()],
                10,
            )?;
            let _ = adb::run_captured_with_timeout(
                &adb_path,
                Some(&serial),
                &["shell".into(), "rm".into(), "-f".into(), remote.into()],
                5,
            );
            let xml = truncate_utf8(&String::from_utf8_lossy(&cap.stdout), MAX_TEXT_BYTES * 2);
            Ok(text_tool_result(xml, cap.exit_code != 0))
        }
        "ui_tap" => {
            let serial = resolve_mcp_device(&adb_path, &store, args.get("device").and_then(|v| v.as_str()))?;
            let Some(x) = args.get("x").and_then(|v| v.as_u64()) else {
                return Ok(text_tool_result("Missing integer parameter: x".into(), true));
            };
            let Some(y) = args.get("y").and_then(|v| v.as_u64()) else {
                return Ok(text_tool_result("Missing integer parameter: y".into(), true));
            };
            let cap = adb::run_captured_with_timeout(
                &adb_path,
                Some(&serial),
                &["shell".into(), "input".into(), "tap".into(), x.to_string(), y.to_string()],
                10,
            )?;
            Ok(text_tool_result(format!("Tapped ({x}, {y}) on {} (exit={})", store.display_label(&serial), cap.exit_code), cap.exit_code != 0))
        }
        "ui_swipe" => {
            let serial = resolve_mcp_device(&adb_path, &store, args.get("device").and_then(|v| v.as_str()))?;
            let (Some(x1), Some(y1), Some(x2), Some(y2)) = (
                args.get("x1").and_then(|v| v.as_u64()),
                args.get("y1").and_then(|v| v.as_u64()),
                args.get("x2").and_then(|v| v.as_u64()),
                args.get("y2").and_then(|v| v.as_u64()),
            ) else {
                return Ok(text_tool_result("Missing integer coordinates x1, y1, x2, y2".into(), true));
            };
            let dur = args.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(300).clamp(50, 5000);
            let cap = adb::run_captured_with_timeout(
                &adb_path,
                Some(&serial),
                &[
                    "shell".into(), "input".into(), "swipe".into(),
                    x1.to_string(), y1.to_string(), x2.to_string(), y2.to_string(), dur.to_string(),
                ],
                10,
            )?;
            Ok(text_tool_result(format!("Swiped ({x1},{y1})->({x2},{y2}) in {dur}ms (exit={})", cap.exit_code), cap.exit_code != 0))
        }
        "ui_input" => {
            let serial = resolve_mcp_device(&adb_path, &store, args.get("device").and_then(|v| v.as_str()))?;
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.is_empty() || text.len() > 500 {
                return Ok(text_tool_result("Parameter `text` must be 1..500 chars".into(), true));
            }
            // 严格转义，防止 Android sh 解释特殊元字符导致命令注入
            let mut safe = String::with_capacity(text.len());
            for ch in text.chars() {
                if ch == ' ' {
                    safe.push_str("%s");
                } else if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '@' | '/' | ':') {
                    safe.push(ch);
                } else {
                    return Ok(text_tool_result(
                        format!("Unsupported character '{ch}' in ui_input (only safe ASCII allowed)"),
                        true,
                    ));
                }
            }
            let cap = adb::run_captured_with_timeout(
                &adb_path,
                Some(&serial),
                &["shell".into(), "input".into(), "text".into(), safe],
                10,
            )?;
            Ok(text_tool_result(format!("Input text on {} (exit={})", store.display_label(&serial), cap.exit_code), cap.exit_code != 0))
        }
        "ui_keyevent" => {
            let serial = resolve_mcp_device(&adb_path, &store, args.get("device").and_then(|v| v.as_str()))?;
            let raw_key = args.get("key").and_then(|v| v.as_str()).unwrap_or("").trim();
            let keycode = match raw_key.to_ascii_uppercase().as_str() {
                "HOME" => "3".to_string(),
                "BACK" => "4".to_string(),
                "VOLUME_UP" => "24".to_string(),
                "VOLUME_DOWN" => "25".to_string(),
                "POWER" => "26".to_string(),
                "TAB" => "61".to_string(),
                "ENTER" => "66".to_string(),
                "DEL" | "BACKSPACE" => "67".to_string(),
                "RECENTS" | "APP_SWITCH" => "187".to_string(),
                "SLEEP" => "223".to_string(),
                "WAKEUP" => "224".to_string(),
                other => match other.parse::<u16>() {
                    Ok(code) if code <= 400 => code.to_string(),
                    _ => return Ok(text_tool_result(format!("Unsupported keyevent '{raw_key}'"), true)),
                },
            };
            let cap = adb::run_captured_with_timeout(
                &adb_path,
                Some(&serial),
                &["shell".into(), "input".into(), "keyevent".into(), keycode.clone()],
                10,
            )?;
            Ok(text_tool_result(
                format!("Sent keyevent {raw_key} ({keycode}) to {} (exit={})", store.display_label(&serial), cap.exit_code),
                cap.exit_code != 0,
            ))
        }
        "run_shell" => {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("").trim();
            if command.is_empty() {
                return Ok(text_tool_result("Missing required parameter: command".into(), true));
            }
            // 安全加固：默认模式下启用只读白名单，彻底杜绝黑名单绕过
            if !allow_destructive && !adb::is_readonly_mcp_shell(command) {
                return Ok(text_tool_result(
                    format!(
                        "Blocked non-readonly shell command `{command}` in safe mode. Allowed prefixes: getprop, dumpsys, pm list/path, ls, cat, ps, wm, ip, df, pidof (no shell chaining). Restart with `adbx mcp --allow-destructive` for full shell."
                    ),
                    true,
                ));
            }
            let timeout = args.get("timeout_sec").and_then(|v| v.as_u64()).unwrap_or(15).clamp(1, 60);
            let dev_arg = args.get("device").and_then(|v| v.as_str()).map(|s| s.trim_start_matches('@'));
            let online: Vec<_> = adb::list_devices(&adb_path)?
                .into_iter()
                .filter(|d| d.state == "device")
                .collect();

            let targets: Vec<(usize, String, String)> = match dev_arg {
                Some("all") => online
                    .into_iter()
                    .enumerate()
                    .map(|(i, d)| (i, store.display_label(&d.serial), d.serial))
                    .collect(),
                Some(grp) if store.group_members(grp).is_some() => store
                    .expand_group_online(grp, &online)?
                    .into_iter()
                    .enumerate()
                    .map(|(i, d)| (i, store.display_label(&d.serial), d.serial))
                    .collect(),
                other => {
                    let s = resolve_mcp_device(&adb_path, &store, other)?;
                    vec![(0, store.display_label(&s), s)]
                }
            };

            let queue = Mutex::new(targets.into_iter().collect::<VecDeque<_>>());
            let results = Mutex::new(Vec::<(usize, Value)>::new());
            let workers = 8usize.min(queue.lock().unwrap().len()).max(1);

            thread::scope(|s| {
                for _ in 0..workers {
                    s.spawn(|| loop {
                        let item = queue.lock().unwrap().pop_front();
                        let Some((idx, label, serial)) = item else { break };
                        let entry = match adb::run_captured_with_timeout(
                            &adb_path,
                            Some(&serial),
                            &["shell".into(), command.into()],
                            timeout,
                        ) {
                            Ok(cap) => {
                                let stdout_full = String::from_utf8_lossy(&cap.stdout);
                                let stderr_full = String::from_utf8_lossy(&cap.stderr);
                                json!({
                                    "device": label,
                                    "serial": serial,
                                    "exit_code": cap.exit_code,
                                    "timed_out": cap.timed_out,
                                    "elapsed_ms": cap.elapsed_ms,
                                    "truncated": stdout_full.len() > MAX_TEXT_BYTES,
                                    "stdout": truncate_utf8(&stdout_full, MAX_TEXT_BYTES),
                                    "stderr": truncate_utf8(&stderr_full, 4096)
                                })
                            }
                            Err(e) => json!({
                                "device": label,
                                "serial": serial,
                                "exit_code": 1,
                                "stderr": format!("{e:#}")
                            }),
                        };
                        results.lock().unwrap().push((idx, entry));
                    });
                }
            });

            let mut ordered = results.into_inner().unwrap();
            ordered.sort_by_key(|(idx, _)| *idx);
            let final_list: Vec<Value> = ordered.into_iter().map(|(_, v)| v).collect();
            Ok(text_tool_result(serde_json::to_string_pretty(&final_list)?, false))
        }
        "app_action" => {
            let pkg_raw = args.get("pkg").and_then(|v| v.as_str()).unwrap_or("");
            let pkg = match adb::validate_package_name(pkg_raw) {
                Ok(p) => p,
                Err(e) => return Ok(text_tool_result(format!("{e}"), true)),
            };
            let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
            if action == "clear" && !allow_destructive {
                return Ok(text_tool_result(
                    "Action `clear` is blocked in safe mode. Start with `adbx mcp --allow-destructive`.".into(),
                    true,
                ));
            }
            let serial = resolve_mcp_device(&adb_path, &store, args.get("device").and_then(|v| v.as_str()))?;
            let adb_args: Vec<String> = match action {
                "launch" => vec![
                    "shell".into(), "monkey".into(), "-p".into(), pkg.into(),
                    "-c".into(), "android.intent.category.LAUNCHER".into(), "1".into(),
                ],
                "stop" => vec!["shell".into(), "am".into(), "force-stop".into(), pkg.into()],
                "clear" => vec!["shell".into(), "pm".into(), "clear".into(), pkg.into()],
                other => return Ok(text_tool_result(format!("Unsupported action: {other}"), true)),
            };
            let cap = adb::run_captured_with_timeout(&adb_path, Some(&serial), &adb_args, 15)?;
            let out = json!({
                "device": store.display_label(&serial),
                "serial": serial,
                "executed": format!("adb -s {serial} {}", adb_args.join(" ")),
                "exit_code": cap.exit_code,
                "stdout": String::from_utf8_lossy(&cap.stdout).trim(),
                "stderr": String::from_utf8_lossy(&cap.stderr).trim()
            });
            Ok(text_tool_result(serde_json::to_string_pretty(&out)?, cap.exit_code != 0))
        }
        _ => Ok(text_tool_result(format!("Unknown tool: {name}"), true)),
    }
}

fn build_device_json(adb_path: &PathBuf, store: &Store) -> Result<Value> {
    let devices = adb::list_devices(adb_path)?;
    let arr: Vec<Value> = devices
        .into_iter()
        .map(|d| {
            json!({
                "alias": store.alias_for(&d.serial),
                "serial": d.serial,
                "state": d.state,
                "model": d.model
            })
        })
        .collect();
    Ok(json!({
        "devices": arr,
        "groups": store.list_groups()
    }))
}

fn resolve_mcp_device(adb_path: &PathBuf, store: &Store, explicit: Option<&str>) -> Result<String> {
    let online: Vec<_> = adb::list_devices(adb_path)?
        .into_iter()
        .filter(|d| d.state == "device")
        .collect();

    if let Some(target) = explicit.filter(|s| !s.trim().is_empty()) {
        return store.resolve_input_to_serial(target.trim(), &online);
    }
    match online.len() {
        0 => bail!("No online Android devices connected"),
        1 => Ok(online[0].serial.clone()),
        _ => {
            let names: Vec<String> = online
                .iter()
                .map(|d| format!("{} ({})", store.display_label(&d.serial), d.serial))
                .collect();
            bail!(
                "Multiple devices online; specify `device` parameter. Available: {}",
                names.join(", ")
            )
        }
    }
}

fn text_tool_result(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error
    })
}

fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... [truncated to {end} bytes]", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_initialize_ping_and_tools_list() {
        let init = dispatch("initialize", &json!({}), false).unwrap();
        assert_eq!(init["serverInfo"]["name"], "adbx");

        let ping = dispatch("ping", &json!({}), false).unwrap();
        assert_eq!(ping, json!({}));

        let tools = dispatch("tools/list", &json!({}), false).unwrap();
        let arr = tools["tools"].as_array().unwrap();
        assert!(arr.iter().any(|t| t["name"] == "run_shell"));
        assert!(arr.iter().any(|t| t["name"] == "get_logcat"));
    }

    #[test]
    fn truncate_utf8_respects_char_boundaries() {
        let s = "你好世界adbx";
        let t = truncate_utf8(s, 7);
        assert!(t.starts_with("你好"));
        assert!(t.contains("truncated"));
    }

    #[test]
    fn safe_mode_blocks_destructive_shell_and_clear() {
        if adb::find_adb_binary().is_ok() {
            let shell_res = dispatch(
                "tools/call",
                &json!({ "name": "run_shell", "arguments": { "command": "rm -rf /sdcard/a" } }),
                false,
            )
            .unwrap();
            assert_eq!(shell_res["isError"], true);

            let clear_res = dispatch(
                "tools/call",
                &json!({ "name": "app_action", "arguments": { "pkg": "com.example.app", "action": "clear" } }),
                false,
            )
            .unwrap();
            assert_eq!(clear_res["isError"], true);
        }
    }
}
