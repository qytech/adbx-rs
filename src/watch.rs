use crate::adb;
use crate::session;
use crate::store::Store;
use anyhow::Result;
use chrono::Local;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Terminal,
};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
struct DeviceMetrics {
    model: String,
    android_ver: String,
    battery_pct: String,
    temp_c: String,
}

#[derive(Debug, Clone)]
struct WatchedDevice {
    serial: String,
    state: String,
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// 启动基于本地 adb-server (`ANDROID_ADB_SERVER_PORT` 或默认 5037) `host:track-devices` 长连接的多机实时监控面板
pub fn run_watch_tui(adb_path: PathBuf) -> Result<()> {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let tracked: Arc<RwLock<Vec<WatchedDevice>>> = Arc::new(RwLock::new(Vec::new()));
    let metrics_map: Arc<RwLock<BTreeMap<String, DeviceMetrics>>> =
        Arc::new(RwLock::new(BTreeMap::new()));
    let event_log: Arc<RwLock<VecDeque<String>>> = Arc::new(RwLock::new(VecDeque::with_capacity(64)));

    // 1. 后台线程：直连本地 adb-server 发送 `host:track-devices` 实时感知热插拔
    {
        let stop = Arc::clone(&stop_flag);
        let tracked_ref = Arc::clone(&tracked);
        let log_ref = Arc::clone(&event_log);
        let adb_clone = adb_path.clone();
        let addr = adb_server_addr();
        thread::spawn(move || {
            let mut prev_map: BTreeMap<String, String> = BTreeMap::new();
            while !stop.load(Ordering::Relaxed) {
                let conn = TcpStream::connect(&addr).or_else(|_| {
                    let _ = adb::run_captured_with_timeout(
                        &adb_clone,
                        None,
                        &["start-server".into()],
                        5,
                    );
                    TcpStream::connect(&addr)
                });

                let Ok(mut stream) = conn else {
                    push_log(&log_ref, &format!("无法连接 {addr}，2 秒后重试..."));
                    thread::sleep(Duration::from_secs(2));
                    continue;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));

                if stream.write_all(b"0012host:track-devices").is_err() {
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }

                let mut status = [0u8; 4];
                if read_exact_or_stop(&mut stream, &mut status, &stop).is_err() || &status != b"OKAY" {
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
                push_log(&log_ref, &format!("已连接 adb-server ({addr}) 实时热插拔通道"));

                while !stop.load(Ordering::Relaxed) {
                    let mut len_hex = [0u8; 4];
                    match read_exact_or_stop(&mut stream, &mut len_hex, &stop) {
                        Ok(()) => {}
                        Err(_) => break,
                    }
                    let Ok(len_str) = std::str::from_utf8(&len_hex) else {
                        break;
                    };
                    let Ok(payload_len) = usize::from_str_radix(len_str, 16) else {
                        break;
                    };
                    let mut payload = vec![0u8; payload_len];
                    if payload_len > 0 && read_exact_or_stop(&mut stream, &mut payload, &stop).is_err() {
                        break;
                    }

                    let parsed = parse_track_devices_payload(&String::from_utf8_lossy(&payload));
                    let new_map: BTreeMap<String, String> = parsed
                        .iter()
                        .map(|d| (d.serial.clone(), d.state.clone()))
                        .collect();

                    // 记录热插拔变更事件
                    for (s, st) in &new_map {
                        match prev_map.get(s) {
                            None => push_log(&log_ref, &format!("+ 设备接入: {s} [{st}]")),
                            Some(old) if old != st => {
                                push_log(&log_ref, &format!("~ 状态变更: {s} {old} -> {st}"))
                            }
                            _ => {}
                        }
                    }
                    for s in prev_map.keys() {
                        if !new_map.contains_key(s) {
                            push_log(&log_ref, &format!("- 设备断开: {s}"));
                        }
                    }
                    prev_map = new_map;
                    *tracked_ref.write().unwrap() = parsed;
                }
            }
        });
    }

    // 2. 后台线程：每 6 秒低频刷新在线设备的型号、系统版本、电量与温度
    {
        let stop = Arc::clone(&stop_flag);
        let tracked_ref = Arc::clone(&tracked);
        let metrics_ref = Arc::clone(&metrics_map);
        let adb_clone = adb_path.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let online_serials: Vec<String> = tracked_ref
                    .read()
                    .unwrap()
                    .iter()
                    .filter(|d| d.state == "device")
                    .map(|d| d.serial.clone())
                    .collect();

                for serial in online_serials {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let m = fetch_device_metrics(&adb_clone, &serial);
                    metrics_ref.write().unwrap().insert(serial, m);
                }

                for _ in 0..60 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }

    // 3. 启动 Ratatui 面板
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let _term_guard = TerminalGuard;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut selected: usize = 0;
    let mut store = Store::load()?;
    let session_id = session::current_session_id();
    let mut notice = String::new();

    loop {
        let devices = tracked.read().unwrap().clone();
        let metrics = metrics_map.read().unwrap().clone();
        let logs = event_log.read().unwrap().clone();
        if !devices.is_empty() && selected >= devices.len() {
            selected = devices.len() - 1;
        }
        let locked = store.get_session_device(&session_id);

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(8),
                    Constraint::Length(7),
                    Constraint::Length(3),
                ])
                .split(f.area());

            let header = Row::new(["", "别名", "序列号", "状态", "型号", "Android", "电量", "温度"])
                .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

            let rows: Vec<Row> = devices
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let is_sel = i == selected;
                    let is_locked = locked.as_deref() == Some(d.serial.as_str());
                    let mark = match (is_sel, is_locked) {
                        (true, true) => ">*",
                        (true, false) => "> ",
                        (false, true) => " *",
                        (false, false) => "  ",
                    };
                    let alias = store.alias_for(&d.serial).unwrap_or("-").to_string();
                    let m = metrics.get(&d.serial).cloned().unwrap_or_default();
                    let st_color = match d.state.as_str() {
                        "device" => Color::Green,
                        "unauthorized" => Color::Yellow,
                        _ => Color::Red,
                    };
                    let row_style = if is_sel {
                        Style::default().bg(Color::DarkGray).fg(Color::White)
                    } else {
                        Style::default()
                    };
                    Row::new(vec![
                        Cell::from(mark),
                        Cell::from(alias),
                        Cell::from(d.serial.clone()),
                        Cell::from(d.state.clone()).style(Style::default().fg(st_color)),
                        Cell::from(if m.model.is_empty() { "-".into() } else { m.model }),
                        Cell::from(if m.android_ver.is_empty() { "-".into() } else { m.android_ver }),
                        Cell::from(if m.battery_pct.is_empty() { "-".into() } else { m.battery_pct }),
                        Cell::from(if m.temp_c.is_empty() { "-".into() } else { m.temp_c }),
                    ])
                    .style(row_style)
                })
                .collect();

            let table = Table::new(
                rows,
                [
                    Constraint::Length(3),
                    Constraint::Length(14),
                    Constraint::Length(22),
                    Constraint::Length(13),
                    Constraint::Length(16),
                    Constraint::Length(9),
                    Constraint::Length(7),
                    Constraint::Length(8),
                ],
            )
            .header(header)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" adbx watch · 5037 实时设备监控 ({} 台) ", devices.len())),
            );
            f.render_widget(table, chunks[0]);

            let log_lines: Vec<Line> = logs
                .iter()
                .rev()
                .take(5)
                .rev()
                .map(|s| Line::from(Span::raw(s.as_str())))
                .collect();
            let log_box = Paragraph::new(log_lines)
                .block(Block::default().borders(Borders::ALL).title(" 热插拔事件日志 "));
            f.render_widget(log_box, chunks[1]);

            let footer_text = if notice.is_empty() {
                "[j/k]选择设备  [Enter]锁定到当前终端会话  [q]退出".to_string()
            } else {
                format!("{notice}  |  [j/k]选择  [Enter]锁定  [q]退出")
            };
            let footer = Paragraph::new(footer_text)
                .style(Style::default().fg(Color::Gray))
                .block(Block::default().borders(Borders::ALL));
            f.render_widget(footer, chunks[2]);
        })?;

        if event::poll(Duration::from_millis(120))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Down | KeyCode::Char('j') => {
                        if !devices.is_empty() {
                            selected = (selected + 1).min(devices.len() - 1);
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        selected = selected.saturating_sub(1);
                    }
                    KeyCode::Enter => {
                        if let Some(d) = devices.get(selected) {
                            store.set_session_device(&session_id, &d.serial)?;
                            notice = format!(
                                "已锁定当前终端会话 -> {} ({})",
                                store.display_label(&d.serial),
                                d.serial
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    stop_flag.store(true, Ordering::Relaxed);
    Ok(())
}

fn read_exact_or_stop(stream: &mut TcpStream, buf: &mut [u8], stop: &AtomicBool) -> io::Result<()> {
    let mut offset = 0;
    while offset < buf.len() {
        if stop.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        match stream.read(&mut buf[offset..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof")),
            Ok(n) => offset += n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn push_log(log: &Arc<RwLock<VecDeque<String>>>, msg: &str) {
    let ts = Local::now().format("%H:%M:%S");
    let mut guard = log.write().unwrap();
    if guard.len() >= 50 {
        guard.pop_front();
    }
    guard.push_back(format!("[{ts}] {msg}"));
}

fn parse_track_devices_payload(payload: &str) -> Vec<WatchedDevice> {
    payload
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            Some(WatchedDevice { serial, state })
        })
        .collect()
}

fn fetch_device_metrics(adb_path: &PathBuf, serial: &str) -> DeviceMetrics {
    let mut m = DeviceMetrics::default();
    if let Ok(cap) = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["shell".into(), "getprop".into(), "ro.product.model".into()],
        2,
    ) {
        m.model = String::from_utf8_lossy(&cap.stdout).trim().to_string();
    }
    if let Ok(cap) = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["shell".into(), "getprop".into(), "ro.build.version.release".into()],
        2,
    ) {
        m.android_ver = String::from_utf8_lossy(&cap.stdout).trim().to_string();
    }
    if let Ok(cap) = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["shell".into(), "dumpsys".into(), "battery".into()],
        3,
    ) {
        let (lvl, temp) = parse_battery_info(&String::from_utf8_lossy(&cap.stdout));
        m.battery_pct = lvl;
        m.temp_c = temp;
    }
    m
}

pub fn parse_battery_info(output: &str) -> (String, String) {
    let mut level = String::from("-");
    let mut temp = String::from("-");
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("level:") {
            level = format!("{}%", v.trim());
        } else if let Some(v) = trimmed.strip_prefix("temperature:") {
            if let Ok(tenths) = v.trim().parse::<f32>() {
                temp = format!("{:.1}°C", tenths / 10.0);
            }
        }
    }
    (level, temp)
}

fn adb_server_addr() -> String {
    let port = std::env::var("ANDROID_ADB_SERVER_PORT")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .filter(|&p| p > 0)
        .unwrap_or(5037);
    format!("127.0.0.1:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_track_devices_and_battery_info() {
        let devs = parse_track_devices_payload("R5CT10ABCDE\tdevice\n9A8B7C6D\toffline\n");
        assert_eq!(devs.len(), 2);
        assert_eq!(devs[0].serial, "R5CT10ABCDE");
        assert_eq!(devs[1].state, "offline");

        let (lvl, temp) = parse_battery_info("  level: 87\n  temperature: 345\n");
        assert_eq!(lvl, "87%");
        assert_eq!(temp, "34.5°C");
    }
}
