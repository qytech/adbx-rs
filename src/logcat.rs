use crate::adb;
use crate::highlight;
use crate::store::Store;
use anyhow::{Context, Result};
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
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use regex::{Regex, RegexBuilder};
use std::{
    collections::{HashSet, VecDeque},
    fs::File,
    io::{self, BufRead, BufReader, IsTerminal, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex, RwLock,
    },
    thread,
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Verbose = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Fatal = 5,
}

impl LogLevel {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "D" | "DEBUG" => Self::Debug,
            "I" | "INFO" => Self::Info,
            "W" | "WARN" => Self::Warn,
            "E" | "ERROR" => Self::Error,
            "F" | "FATAL" => Self::Fatal,
            _ => Self::Verbose,
        }
    }

    pub fn from_char(c: char) -> Self {
        match c {
            'D' => Self::Debug,
            'I' => Self::Info,
            'W' => Self::Warn,
            'E' => Self::Error,
            'F' | 'A' => Self::Fatal,
            _ => Self::Verbose,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Verbose => "V",
            Self::Debug => "D",
            Self::Info => "I",
            Self::Warn => "W",
            Self::Error => "E",
            Self::Fatal => "F",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Self::Verbose => Color::DarkGray,
            Self::Debug => Color::Cyan,
            Self::Info => Color::Green,
            Self::Warn => Color::Yellow,
            Self::Error => Color::Red,
            Self::Fatal => Color::Magenta,
        }
    }
}

pub struct LogcatOptions {
    pub adb_path: PathBuf,
    pub serial: String,
    pub device_label: String,
    pub package: Option<String>,
    pub min_level: LogLevel,
    pub include_pattern: String,
    pub exclude_pattern: String,
    pub max_buffer: usize,
}

#[derive(Debug, Clone)]
pub struct ParsedLine {
    pub timestamp: String,
    pub pid: String,
    pub level: LogLevel,
    pub tag: String,
    pub message: String,
}

/// 公共 threadtime 解析函数（logcat TUI 与 logcat_snapshot 共用，消除重复代码）
pub fn parse_threadtime_raw(raw: &str) -> ParsedLine {
    let mut parts = raw.split_whitespace();
    let (Some(date), Some(time), Some(pid), Some(_tid), Some(lvl)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return ParsedLine {
            timestamp: String::new(),
            pid: "-".to_string(),
            level: LogLevel::Verbose,
            tag: "RAW".to_string(),
            message: raw.to_string(),
        };
    };

    let level = LogLevel::from_char(lvl.chars().next().unwrap_or('V'));
    let mut rest = raw;
    for _ in 0..5 {
        rest = rest.trim_start();
        let next_ws = rest.find(char::is_whitespace).unwrap_or(rest.len());
        rest = &rest[next_ws..];
    }
    let rest = rest.trim_start();
    let (tag, message) = match rest.split_once(':') {
        Some((t, m)) => (t.trim().to_string(), m.trim_start().to_string()),
        None => ("-".to_string(), rest.to_string()),
    };

    ParsedLine {
        timestamp: format!("{date} {time}"),
        pid: pid.to_string(),
        level,
        tag,
        message,
    }
}

#[derive(Debug, Clone)]
struct LogEntry {
    seq: u64,
    timestamp: String,
    pid: String,
    level: LogLevel,
    tag: String,
    message: String,
    raw: String,
    is_banner: bool,
    is_crash: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    EditInclude,
    EditExclude,
    EditPackage,
}

struct LogcatStopGuard {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Drop for LogcatStopGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut c) = guard.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

struct AppState {
    device_label: String,
    serial: String,
    max_buffer: usize,
    base_seq: u64,
    next_seq: u64,
    entries: VecDeque<LogEntry>,
    filtered_seqs: VecDeque<u64>,
    min_level: LogLevel,
    include_text: String,
    include_regex: Option<Regex>,
    exclude_text: String,
    exclude_regex: Option<Regex>,
    package_name: Arc<RwLock<Option<String>>>,
    tracked_pids: Arc<RwLock<HashSet<String>>>,
    pids_version: Arc<AtomicU64>,
    seen_pids_version: u64,
    paused: bool,
    scroll_from_bottom: usize,
    mode: InputMode,
    input_buf: String,
    status_notice: String,
    syntax_highlight: bool,
    custom_highlights: Vec<highlight::CustomRule>,
}

impl AppState {
    fn compile_regex(pattern: &str) -> Option<Regex> {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            return None;
        }
        RegexBuilder::new(trimmed)
            .case_insensitive(true)
            .build()
            .ok()
    }

    fn matches(&self, entry: &LogEntry, active_pkg: &Option<String>, pids: &HashSet<String>) -> bool {
        if entry.is_banner {
            return true;
        }
        if entry.level < self.min_level {
            return false;
        }
        if active_pkg.is_some() && (pids.is_empty() || !pids.contains(&entry.pid)) {
            return false;
        }
        if let Some(re) = &self.include_regex {
            if !re.is_match(&entry.raw) {
                return false;
            }
        }
        if let Some(re) = &self.exclude_regex {
            if re.is_match(&entry.raw) {
                return false;
            }
        }
        true
    }

    fn push_entry(&mut self, mut entry: LogEntry, active_pkg: &Option<String>, pids: &HashSet<String>) {
        entry.seq = self.next_seq;
        self.next_seq += 1;

        if self.entries.len() >= self.max_buffer {
            self.entries.pop_front();
            self.base_seq += 1;
            while self.filtered_seqs.front().copied().is_some_and(|s| s < self.base_seq) {
                self.filtered_seqs.pop_front();
            }
        }

        let matched = self.matches(&entry, active_pkg, pids);
        let seq = entry.seq;
        self.entries.push_back(entry);
        if matched {
            self.filtered_seqs.push_back(seq);
            if self.paused {
                self.scroll_from_bottom = self
                    .scroll_from_bottom
                    .saturating_add(1)
                    .min(self.filtered_seqs.len().saturating_sub(1));
            }
        } else if self.paused {
            self.scroll_from_bottom = self
                .scroll_from_bottom
                .min(self.filtered_seqs.len().saturating_sub(1));
        }
    }

    fn rebuild_filter_cache(&mut self) {
        let active_pkg = self.package_name.read().unwrap().clone();
        let pids = self.tracked_pids.read().unwrap().clone();
        self.filtered_seqs.clear();
        for entry in &self.entries {
            if self.matches(entry, &active_pkg, &pids) {
                self.filtered_seqs.push_back(entry.seq);
            }
        }
    }

    fn entry_by_seq(&self, seq: u64) -> Option<&LogEntry> {
        let idx = seq.checked_sub(self.base_seq)? as usize;
        self.entries.get(idx)
    }

    /// 跳转到下一个/上一个崩溃或 ANR 书签行
    fn jump_crash(&mut self, forward: bool) {
        let total = self.filtered_seqs.len();
        if total == 0 {
            return;
        }
        let crash_positions: Vec<usize> = self
            .filtered_seqs
            .iter()
            .enumerate()
            .filter_map(|(pos, &seq)| {
                self.entry_by_seq(seq)
                    .filter(|e| e.is_crash)
                    .map(|_| pos)
            })
            .collect();
        if crash_positions.is_empty() {
            self.status_notice = "当前过滤结果中未检测到 FATAL/ANR 崩溃记录".to_string();
            return;
        }

        let current_bottom_idx = total.saturating_sub(1).saturating_sub(self.scroll_from_bottom);
        let target_pos = if forward {
            crash_positions
                .iter()
                .copied()
                .find(|&p| p > current_bottom_idx)
                .unwrap_or(crash_positions[0])
        } else {
            crash_positions
                .iter()
                .rev()
                .copied()
                .find(|&p| p < current_bottom_idx)
                .unwrap_or(*crash_positions.last().unwrap())
        };

        self.paused = true;
        self.scroll_from_bottom = total.saturating_sub(1).saturating_sub(target_pos);
        self.status_notice = format!("已定位到崩溃/ANR 书签（共 {} 处）", crash_positions.len());
    }

    fn export_filtered(&self) -> Result<String> {
        let filename = format!("adbx-logcat-{}.log", Local::now().format("%Y%m%d-%H%M%S"));
        let mut file = File::create(&filename)?;
        let mut count = 0usize;
        for &seq in &self.filtered_seqs {
            if let Some(entry) = self.entry_by_seq(seq) {
                writeln!(file, "{}", entry.raw)?;
                count += 1;
            }
        }
        Ok(format!("已导出 {count} 行到 {filename}"))
    }
}

pub fn run_tui(opts: LogcatOptions) -> Result<()> {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let active_child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let _stop_guard = LogcatStopGuard {
        stop: Arc::clone(&stop_flag),
        child: Arc::clone(&active_child),
    };
    let package_name = Arc::new(RwLock::new(opts.package.clone()));
    let tracked_pids = Arc::new(RwLock::new(HashSet::<String>::new()));
    let pids_version = Arc::new(AtomicU64::new(0));
    let pkg_change_ver = Arc::new(AtomicU64::new(0));

    // 1. 后台线程：按包名/进程正则自动追踪 PID
    {
        let stop = Arc::clone(&stop_flag);
        let adb_path = opts.adb_path.clone();
        let serial = opts.serial.clone();
        let pkg_ref = Arc::clone(&package_name);
        let pids_ref = Arc::clone(&tracked_pids);
        let ver_ref = Arc::clone(&pids_version);
        let change_ref = Arc::clone(&pkg_change_ver);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let last_change = change_ref.load(Ordering::Relaxed);
                let maybe_pkg = pkg_ref.read().unwrap().clone();
                let new_pids = match maybe_pkg {
                    Some(pkg) if !pkg.trim().is_empty() => {
                        fetch_matching_pids(&adb_path, &serial, &pkg)
                    }
                    _ => HashSet::new(),
                };
                {
                    let mut guard = pids_ref.write().unwrap();
                    if *guard != new_pids {
                        *guard = new_pids;
                        ver_ref.fetch_add(1, Ordering::Relaxed);
                    }
                }
                for _ in 0..20 {
                    if stop.load(Ordering::Relaxed) || change_ref.load(Ordering::Relaxed) != last_change {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }

    // 2. 后台线程：带断线自动重连的 logcat 采集循环
    let (tx, rx) = mpsc::sync_channel::<LogEntry>(4096);
    {
        let stop = Arc::clone(&stop_flag);
        let adb_path = opts.adb_path.clone();
        let serial = opts.serial.clone();
        let child_slot = Arc::clone(&active_child);
        thread::spawn(move || {
            let mut first_connect = true;
            while !stop.load(Ordering::Relaxed) {
                let spawn_res = Command::new(&adb_path)
                    .args(["-s", &serial, "logcat", "-v", "threadtime"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn();

                if let Ok(mut child) = spawn_res {
                    let stdout = child.stdout.take();
                    {
                        let mut guard = child_slot.lock().unwrap();
                        if stop.load(Ordering::Relaxed) {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        *guard = Some(child);
                    }
                    if !first_connect {
                        let _ = tx.send(make_banner("--- [设备已重新连接，继续采集日志] ---"));
                    }
                    first_connect = false;

                    if let Some(stdout) = stdout {
                        let reader = BufReader::new(stdout);
                        for line in reader.lines() {
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            if let Ok(raw) = line {
                                if tx.send(to_log_entry(&raw)).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    if let Some(mut c) = child_slot.lock().unwrap().take() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                }

                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(make_banner("--- [设备连接已断开，2 秒后自动重连...] ---"));
                for _ in 0..20 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }

    // 3. 初始化 Ratatui + Crossterm
    enable_raw_mode()?;
    let mut term_stdout = io::stdout();
    execute!(term_stdout, EnterAlternateScreen)?;
    let _term_guard = TerminalGuard;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut state = AppState {
        device_label: opts.device_label,
        serial: opts.serial,
        max_buffer: opts.max_buffer,
        base_seq: 0,
        next_seq: 0,
        entries: VecDeque::with_capacity(opts.max_buffer.min(16_384)),
        filtered_seqs: VecDeque::with_capacity(opts.max_buffer.min(16_384)),
        min_level: opts.min_level,
        include_regex: AppState::compile_regex(&opts.include_pattern),
        include_text: opts.include_pattern,
        exclude_regex: AppState::compile_regex(&opts.exclude_pattern),
        exclude_text: opts.exclude_pattern,
        package_name,
        tracked_pids,
        pids_version,
        seen_pids_version: 0,
        paused: false,
        scroll_from_bottom: 0,
        mode: InputMode::Normal,
        input_buf: String::new(),
        status_notice: String::new(),
        syntax_highlight: true,
        custom_highlights: Store::load()
            .map(|s| highlight::compile_custom_rules(s.list_log_highlights()))
            .unwrap_or_default(),
    };

    loop {
        let cur_ver = state.pids_version.load(Ordering::Relaxed);
        if cur_ver != state.seen_pids_version {
            state.seen_pids_version = cur_ver;
            state.rebuild_filter_cache();
        }

        let active_pkg = state.package_name.read().unwrap().clone();
        let pids = state.tracked_pids.read().unwrap().clone();
        for _ in 0..2000 {
            match rx.try_recv() {
                Ok(entry) => state.push_entry(entry, &active_pkg, &pids),
                Err(_) => break,
            }
        }

        terminal.draw(|f| draw_ui(f, &state, &active_pkg, &pids))?;

        if event::poll(Duration::from_millis(33))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }

                match state.mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char(' ') => {
                            state.paused = !state.paused;
                            if !state.paused {
                                state.scroll_from_bottom = 0;
                                state.status_notice = "已恢复实时滚动 (LIVE)".to_string();
                            } else {
                                state.status_notice =
                                    "已暂停滚动（后台继续采集，按 Space 或 G 恢复实时）".to_string();
                            }
                        }
                        KeyCode::Char('h') => {
                            state.syntax_highlight = !state.syntax_highlight;
                            state.status_notice = if state.syntax_highlight {
                                "已开启语义彩色高亮".to_string()
                            } else {
                                "已关闭语义彩色高亮（纯文本模式）".to_string()
                            };
                        }
                        KeyCode::Char('n') => state.jump_crash(true),
                        KeyCode::Char('N') => state.jump_crash(false),
                        KeyCode::Char('c') => {
                            state.entries.clear();
                            state.filtered_seqs.clear();
                            state.base_seq = state.next_seq;
                            state.scroll_from_bottom = 0;
                            state.status_notice = "已清空内存日志缓冲".to_string();
                        }
                        KeyCode::Char('s') => match state.export_filtered() {
                            Ok(msg) => state.status_notice = msg,
                            Err(e) => state.status_notice = format!("导出失败: {e}"),
                        },
                        KeyCode::Char('/') => {
                            state.mode = InputMode::EditInclude;
                            state.input_buf = state.include_text.clone();
                        }
                        KeyCode::Char('!') => {
                            state.mode = InputMode::EditExclude;
                            state.input_buf = state.exclude_text.clone();
                        }
                        KeyCode::Char('p') => {
                            state.mode = InputMode::EditPackage;
                            state.input_buf = state
                                .package_name
                                .read()
                                .unwrap()
                                .clone()
                                .unwrap_or_default();
                        }
                        KeyCode::Char('1') => { state.min_level = LogLevel::Verbose; state.rebuild_filter_cache(); }
                        KeyCode::Char('2') => { state.min_level = LogLevel::Debug; state.rebuild_filter_cache(); }
                        KeyCode::Char('3') => { state.min_level = LogLevel::Info; state.rebuild_filter_cache(); }
                        KeyCode::Char('4') => { state.min_level = LogLevel::Warn; state.rebuild_filter_cache(); }
                        KeyCode::Char('5') => { state.min_level = LogLevel::Error; state.rebuild_filter_cache(); }
                        KeyCode::Char('6') => { state.min_level = LogLevel::Fatal; state.rebuild_filter_cache(); }
                        KeyCode::Up | KeyCode::Char('k') => {
                            state.paused = true;
                            state.scroll_from_bottom = state.scroll_from_bottom.saturating_add(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            state.scroll_from_bottom = state.scroll_from_bottom.saturating_sub(1);
                            if state.scroll_from_bottom == 0 {
                                state.paused = false;
                            }
                        }
                        KeyCode::PageUp => {
                            state.paused = true;
                            state.scroll_from_bottom = state.scroll_from_bottom.saturating_add(20);
                        }
                        KeyCode::PageDown => {
                            state.scroll_from_bottom = state.scroll_from_bottom.saturating_sub(20);
                            if state.scroll_from_bottom == 0 {
                                state.paused = false;
                            }
                        }
                        KeyCode::Char('G') => {
                            state.scroll_from_bottom = 0;
                            state.paused = false;
                        }
                        _ => {}
                    },
                    InputMode::EditInclude | InputMode::EditExclude | InputMode::EditPackage => {
                        match key.code {
                            KeyCode::Esc => {
                                state.mode = InputMode::Normal;
                                state.input_buf.clear();
                            }
                            KeyCode::Enter => {
                                let val = state.input_buf.trim().to_string();
                                match state.mode {
                                    InputMode::EditInclude => {
                                        state.include_regex = AppState::compile_regex(&val);
                                        state.include_text = val;
                                    }
                                    InputMode::EditExclude => {
                                        state.exclude_regex = AppState::compile_regex(&val);
                                        state.exclude_text = val;
                                    }
                                    InputMode::EditPackage => {
                                        if val.is_empty() {
                                            *state.package_name.write().unwrap() = None;
                                            state.tracked_pids.write().unwrap().clear();
                                            pkg_change_ver.fetch_add(1, Ordering::Relaxed);
                                        } else if let Err(e) = RegexBuilder::new(&val).case_insensitive(true).build() {
                                            state.status_notice = format!("包名正则无效: {e}");
                                        } else {
                                            *state.package_name.write().unwrap() = Some(val);
                                            state.tracked_pids.write().unwrap().clear();
                                            pkg_change_ver.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                    InputMode::Normal => {}
                                }
                                state.rebuild_filter_cache();
                                state.scroll_from_bottom = 0;
                                state.mode = InputMode::Normal;
                            }
                            KeyCode::Backspace => {
                                state.input_buf.pop();
                            }
                            KeyCode::Char(c) => {
                                state.input_buf.push(c);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    stop_flag.store(true, Ordering::Relaxed);
    Ok(())
}

fn make_banner(msg: &str) -> LogEntry {
    LogEntry {
        seq: 0,
        timestamp: Local::now().format("%H:%M:%S").to_string(),
        pid: "-".into(),
        level: LogLevel::Warn,
        tag: "ADBX".into(),
        message: msg.to_string(),
        raw: msg.to_string(),
        is_banner: true,
        is_crash: false,
    }
}

fn to_log_entry(raw: &str) -> LogEntry {
    let p = parse_threadtime_raw(raw);
    let is_crash = raw.contains("FATAL EXCEPTION")
        || raw.contains("ANR in ")
        || raw.contains("Fatal signal ");
    LogEntry {
        seq: 0,
        timestamp: p.timestamp,
        pid: p.pid,
        level: p.level,
        tag: p.tag,
        message: p.message,
        raw: raw.to_string(),
        is_banner: false,
        is_crash,
    }
}

fn draw_ui(
    f: &mut ratatui::Frame,
    state: &AppState,
    active_pkg: &Option<String>,
    pids: &HashSet<String>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(f.area());

    let pkg_display = match active_pkg {
        Some(p) => {
            let pid_str = if pids.is_empty() {
                "等待启动...".to_string()
            } else {
                pids.iter().cloned().collect::<Vec<_>>().join(",")
            };
            format!("pkg:{p} (PID:{pid_str})")
        }
        None => "pkg:ALL".to_string(),
    };
    let live_badge = if state.paused { "PAUSED" } else { "LIVE" };
    let header_text = format!(
        " {} ({}) | {} | Lv>={} | /{} | !{} | {}/{} | [{}]",
        state.device_label,
        state.serial,
        pkg_display,
        state.min_level.code(),
        if state.include_text.is_empty() { "*" } else { &state.include_text },
        if state.exclude_text.is_empty() { "-" } else { &state.exclude_text },
        state.filtered_seqs.len(),
        state.entries.len(),
        live_badge
    );
    let header = Paragraph::new(header_text)
        .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title(" adbx logcat "));
    f.render_widget(header, chunks[0]);

    let viewport_height = chunks[1].height.saturating_sub(2) as usize;
    let total = state.filtered_seqs.len();

    let visible_lines: Vec<Line> = if total == 0 && active_pkg.is_some() && pids.is_empty() {
        vec![Line::from(Span::styled(
            format!(
                "正在等待应用 {} 启动（后台每 2 秒自动检测 PID，启动后日志将自动接续）...",
                active_pkg.as_deref().unwrap_or("")
            ),
            Style::default().fg(Color::Yellow),
        ))]
    } else {
        let max_offset = total.saturating_sub(viewport_height);
        let offset = state.scroll_from_bottom.min(max_offset);
        let end = total.saturating_sub(offset);
        let start = end.saturating_sub(viewport_height);

        (start..end)
            .filter_map(|i| state.filtered_seqs.get(i).copied())
            .filter_map(|seq| state.entry_by_seq(seq))
            .map(|entry| {
                if entry.is_banner {
                    return Line::from(Span::styled(
                        format!("{} {}", entry.timestamp, entry.message),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ));
                }
                let lvl_badge_style = match entry.level {
                    LogLevel::Verbose => Style::default().fg(Color::Gray).bg(Color::DarkGray),
                    LogLevel::Debug => Style::default().fg(Color::White).bg(Color::Blue).add_modifier(Modifier::BOLD),
                    LogLevel::Info => Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD),
                    LogLevel::Warn => Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
                    LogLevel::Error => Style::default().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD),
                    LogLevel::Fatal => Style::default().fg(Color::White).bg(Color::Magenta).add_modifier(Modifier::BOLD),
                };
                let tag_col = if state.syntax_highlight {
                    highlight::tag_color(&entry.tag)
                } else {
                    entry.level.color()
                };
                let mut spans = vec![
                    Span::styled(
                        format!("{} ", entry.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{:>5} ", entry.pid),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!(" {} ", entry.level.code()), lvl_badge_style),
                    Span::raw(" "),
                    Span::styled(
                        format!("{:<18} ", truncate_str(&entry.tag, 18)),
                        Style::default().fg(tag_col).add_modifier(Modifier::BOLD),
                    ),
                ];
                if entry.is_crash {
                    spans.push(Span::styled(
                        &entry.message,
                        Style::default().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.extend(highlight::highlight_message(
                        &entry.message,
                        state.include_regex.as_ref(),
                        &state.custom_highlights,
                        state.syntax_highlight,
                    ));
                }
                Line::from(spans)
            })
            .collect()
    };

    let log_view = Paragraph::new(visible_lines).block(Block::default().borders(Borders::ALL));
    f.render_widget(log_view, chunks[1]);

    let footer_text = match state.mode {
        InputMode::EditInclude => format!("包含正则 (/): {}_", state.input_buf),
        InputMode::EditExclude => format!("排除正则 (!): {}_", state.input_buf),
        InputMode::EditPackage => format!("包名/进程正则 (p，留空看全部): {}_", state.input_buf),
        InputMode::Normal => {
            if !state.status_notice.is_empty() {
                format!(
                    "{}  |  [/]正则 [!]排除 [p]包名 [n/N]崩溃跳转 [1-6]级别 [Space]暂停 [s]导出 [q]退出",
                    state.status_notice
                )
            } else {
                "[/]正则 [!]排除 [p]包名 [h]高亮开关 [n/N]崩溃书签 [1-6]V-F [Space]暂停 [s]导出 [c]清屏 [q]退出".to_string()
            }
        }
    };
    let footer_style = if state.mode == InputMode::Normal {
        Style::default().fg(Color::Gray)
    } else {
        Style::default().fg(Color::Yellow)
    };
    let footer = Paragraph::new(footer_text)
        .style(footer_style)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, chunks[2]);
}

/// 从 `ps -A -o PID,NAME` 或 `ps` 输出中按正则匹配进程名并返回对应的 PID 集合
pub fn parse_ps_pids_matching(ps_output: &str, pattern: &str) -> HashSet<String> {
    let trimmed = pattern.trim();
    let Ok(re) = RegexBuilder::new(trimmed).case_insensitive(true).build() else {
        return HashSet::new();
    };
    if trimmed.is_empty() {
        return HashSet::new();
    }

    let mut pids = HashSet::new();
    for line in ps_output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 2 {
            continue;
        }
        if matches!(tokens[0], "PID" | "USER" | "UID") {
            continue;
        }
        let (pid, proc_name) = if tokens[0].chars().all(|c| c.is_ascii_digit()) {
            (tokens[0], tokens[tokens.len() - 1])
        } else if let Some(pid_tok) = tokens.iter().skip(1).find(|t| t.chars().all(|c| c.is_ascii_digit())) {
            (*pid_tok, tokens[tokens.len() - 1])
        } else {
            continue;
        };
        if re.is_match(proc_name) {
            pids.insert(pid.to_string());
        }
    }
    pids
}

/// 按包名/进程正则获取设备上匹配的 PID 列表（优先 `ps -A -o PID,NAME`，回退 `ps` 与精确 `pidof`）
pub fn fetch_matching_pids(adb_path: &PathBuf, serial: &str, pattern: &str) -> HashSet<String> {
    if let Ok(cap) = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["shell".into(), "ps".into(), "-A".into(), "-o".into(), "PID,NAME".into()],
        3,
    ) {
        if cap.exit_code == 0 {
            let pids = parse_ps_pids_matching(&String::from_utf8_lossy(&cap.stdout), pattern);
            if !pids.is_empty() {
                return pids;
            }
        }
    }

    if let Ok(cap) = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["shell".into(), "ps".into()],
        3,
    ) {
        if cap.exit_code == 0 {
            let pids = parse_ps_pids_matching(&String::from_utf8_lossy(&cap.stdout), pattern);
            if !pids.is_empty() {
                return pids;
            }
        }
    }

    if adb::validate_package_name(pattern).is_ok() {
        if let Ok(cap) = adb::run_captured_with_timeout(
            adb_path,
            Some(serial),
            &["shell".into(), "pidof".into(), pattern.trim().into()],
            3,
        ) {
            if cap.exit_code == 0 {
                return String::from_utf8_lossy(&cap.stdout)
                    .split_whitespace()
                    .filter(|s| s.chars().all(|c| c.is_ascii_digit()))
                    .map(|s| s.to_string())
                    .collect();
            }
        }
    }

    HashSet::new()
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// 判断 `logcat` 命令是否适合启用终端实时彩色高亮
pub fn should_colorize_logcat(args: &[String]) -> bool {
    if !std::io::stdout().is_terminal() {
        return false;
    }
    if args.first().map(|s| s.as_str()) != Some("logcat") {
        return false;
    }
    !args[1..].iter().any(|a| {
        matches!(
            a.as_str(),
            "--no-color" | "-c" | "--clear" | "-g" | "-S" | "-L" | "-h" | "--help" | "--version" | "-f" | "-B" | "--binary"
        ) || a.starts_with("-f")
    })
}

fn level_badge_ansi(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Verbose => "\x1b[37;100m V \x1b[0m",
        LogLevel::Debug => "\x1b[1;97;44m D \x1b[0m",
        LogLevel::Info => "\x1b[1;30;42m I \x1b[0m",
        LogLevel::Warn => "\x1b[1;30;43m W \x1b[0m",
        LogLevel::Error => "\x1b[1;97;41m E \x1b[0m",
        LogLevel::Fatal => "\x1b[1;97;45m F \x1b[0m",
    }
}

fn level_msg_ansi(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Verbose => "\x1b[90m",
        LogLevel::Debug => "\x1b[36m",
        LogLevel::Info => "",
        LogLevel::Warn => "\x1b[33m",
        LogLevel::Error => "\x1b[91m",
        LogLevel::Fatal => "\x1b[1;95m",
    }
}

fn level_line_ansi(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Verbose => "\x1b[90m",
        LogLevel::Debug => "\x1b[36m",
        LogLevel::Info => "\x1b[32m",
        LogLevel::Warn => "\x1b[33m",
        LogLevel::Error => "\x1b[1;91m",
        LogLevel::Fatal => "\x1b[1;95m",
    }
}

/// 将已解析的日志字段格式化为带等级徽章、Tag 配色与语义高亮的 ANSI 行
pub fn format_ansi_fields(
    timestamp: &str,
    pid: &str,
    tid: Option<&str>,
    level: LogLevel,
    tag: &str,
    message: &str,
    custom_rules: &[highlight::CustomRule],
) -> String {
    let badge = level_badge_ansi(level);
    let tag_ansi = highlight::color_to_ansi_fg(highlight::tag_color(tag));
    let rendered_msg = highlight::render_ansi_message(message, custom_rules, level_msg_ansi(level));
    let pid_part = match tid {
        Some(t) => format!("{pid:>5} {t:>5}"),
        None => format!("{pid:>5}"),
    };
    format!(
        "\x1b[90m{timestamp} {pid_part}\x1b[0m {badge} \x1b[1m{tag_ansi}{tag:<18}\x1b[0m {rendered_msg}"
    )
}

fn try_parse_threadtime_strict(line: &str) -> Option<(String, &str, &str, LogLevel, String, String)> {
    let mut parts = line.split_whitespace();
    let (Some(date), Some(time), Some(pid), Some(tid), Some(lvl)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };

    if !date.contains('-')
        || !time.contains(':')
        || pid.is_empty()
        || !pid.chars().all(|c| c.is_ascii_digit())
        || tid.is_empty()
        || !tid.chars().all(|c| c.is_ascii_digit())
        || lvl.len() != 1
    {
        return None;
    }

    let lvl_ch = lvl.chars().next()?;
    if !matches!(lvl_ch, 'V' | 'D' | 'I' | 'W' | 'E' | 'F' | 'A') {
        return None;
    }
    let level = LogLevel::from_char(lvl_ch);

    let mut rest = line;
    for _ in 0..5 {
        rest = rest.trim_start();
        let next_ws = rest.find(char::is_whitespace).unwrap_or(rest.len());
        rest = &rest[next_ws..];
    }
    let rest = rest.trim_start();
    let (tag, message) = match rest.split_once(':') {
        Some((t, m)) => (t.trim().to_string(), m.trim_start().to_string()),
        None => ("-".to_string(), rest.to_string()),
    };

    Some((format!("{date} {time}"), pid, tid, level, tag, message))
}

fn detect_non_threadtime_level(line: &str) -> Option<LogLevel> {
    let bytes = line.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[1], b'/' | b'(') {
        let c = bytes[0] as char;
        if matches!(c, 'V' | 'D' | 'I' | 'W' | 'E' | 'F' | 'A') {
            return Some(LogLevel::from_char(c));
        }
    }
    if let Some(tok) = line.split_whitespace().nth(2) {
        let tb = tok.as_bytes();
        if tb.len() >= 2 && tb[1] == b'/' {
            let c = tb[0] as char;
            if matches!(c, 'V' | 'D' | 'I' | 'W' | 'E' | 'F' | 'A') {
                return Some(LogLevel::from_char(c));
            }
        }
    }
    None
}

/// 将单行 logcat 输出格式化为按日志等级区分颜色的 ANSI 文本
pub fn format_colored_logcat_line(raw: &str, custom_rules: &[highlight::CustomRule]) -> String {
    let line = raw.trim_end_matches('\r');
    if line.starts_with("--------- ") {
        return format!("\x1b[90m{line}\x1b[0m");
    }
    if let Some((timestamp, pid, tid, level, tag, message)) = try_parse_threadtime_strict(line) {
        return format_ansi_fields(
            &timestamp,
            pid,
            Some(tid),
            level,
            &tag,
            &message,
            custom_rules,
        );
    }
    if let Some(level) = detect_non_threadtime_level(line) {
        return format!("{}{line}\x1b[0m", level_line_ansi(level));
    }
    line.to_string()
}

/// 流式执行 `adb logcat`（或 `adb shell logcat`），按日志等级与语义高亮实时输出到终端
pub fn run_colored_stream(
    adb_path: &PathBuf,
    serial: &str,
    args: &[String],
    store: &Store,
) -> Result<u8> {
    adb::ensure_ctrlc_ignored();
    let custom_rules = highlight::compile_custom_rules(store.list_log_highlights());

    let mut child = Command::new(adb_path)
        .arg("-s")
        .arg(serial)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("启动 {} 失败", adb_path.display()))?;

    if let Some(stdout) = child.stdout.take() {
        let mut reader = BufReader::new(stdout);
        let out = io::stdout();
        let mut lock = out.lock();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    let raw = String::from_utf8_lossy(&buf);
                    let trimmed = raw.trim_end_matches(&['\n', '\r'][..]);
                    let colored = format_colored_logcat_line(trimmed, &custom_rules);
                    if let Err(e) = writeln!(lock, "{colored}") {
                        if e.kind() == io::ErrorKind::BrokenPipe {
                            let _ = child.kill();
                        }
                        break;
                    }
                    let _ = lock.flush();
                }
                Err(_) => break,
            }
        }
    }

    let status = child.wait()?;
    Ok(status.code().unwrap_or(0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_threadtime_lines_with_level_colors() {
        let err = format_colored_logcat_line(
            "11-26 22:15:01.123  4321  4355 E OkHttp  : timeout on /api/v1",
            &[],
        );
        let warn = format_colored_logcat_line(
            "11-26 22:15:01.124  4321  4355 W AudioFlinger: underrun",
            &[],
        );
        let info = format_colored_logcat_line(
            "11-26 22:15:01.125  4321  4355 I ActivityManager: Displayed",
            &[],
        );
        let debug = format_colored_logcat_line(
            "11-26 22:15:01.126  4321  4355 D MyApp   : state=ready",
            &[],
        );
        let verbose = format_colored_logcat_line(
            "11-26 22:15:01.127  4321  4355 V Trace   : tick",
            &[],
        );
        let fatal = format_colored_logcat_line(
            "11-26 22:15:01.128  4321  4355 F libc    : Fatal signal 11",
            &[],
        );

        assert!(err.contains("\x1b[1;97;41m E \x1b[0m"));
        assert!(err.contains("\x1b[91m"));
        assert!(warn.contains("\x1b[1;30;43m W \x1b[0m"));
        assert!(warn.contains("\x1b[33m"));
        assert!(info.contains("\x1b[1;30;42m I \x1b[0m"));
        assert!(debug.contains("\x1b[1;97;44m D \x1b[0m"));
        assert!(verbose.contains("\x1b[37;100m V \x1b[0m"));
        assert!(fatal.contains("\x1b[1;97;45m F \x1b[0m"));
    }

    #[test]
    fn formats_brief_and_time_logcat_lines_with_level_colors() {
        let brief_err = format_colored_logcat_line("E/OkHttp(123): failed", &[]);
        assert!(brief_err.starts_with("\x1b[1;91m"));
        assert!(brief_err.ends_with("\x1b[0m"));

        let time_warn = format_colored_logcat_line("11-26 22:15:01.123 W/Audio(1): low buffer", &[]);
        assert!(time_warn.starts_with("\x1b[33m"));

        let banner = format_colored_logcat_line("--------- beginning of main", &[]);
        assert!(banner.starts_with("\x1b[90m"));
    }

    #[test]
    fn pause_freezes_viewport_while_buffering_new_entries() {
        let mut state = AppState {
            device_label: "dev".into(),
            serial: "SER".into(),
            max_buffer: 100,
            base_seq: 0,
            next_seq: 0,
            entries: VecDeque::new(),
            filtered_seqs: VecDeque::new(),
            min_level: LogLevel::Verbose,
            include_text: String::new(),
            include_regex: None,
            exclude_text: String::new(),
            exclude_regex: None,
            package_name: Arc::new(RwLock::new(None)),
            tracked_pids: Arc::new(RwLock::new(HashSet::new())),
            pids_version: Arc::new(AtomicU64::new(0)),
            seen_pids_version: 0,
            paused: false,
            scroll_from_bottom: 0,
            mode: InputMode::Normal,
            input_buf: String::new(),
            status_notice: String::new(),
            syntax_highlight: true,
            custom_highlights: Vec::new(),
        };
        let pids = HashSet::new();
        for i in 0..5 {
            state.push_entry(to_log_entry(&format!("11-27 10:00:0{i}.000  100  100 I Tag: msg {i}")), &None, &pids);
        }
        assert_eq!(state.filtered_seqs.len(), 5);
        assert_eq!(state.scroll_from_bottom, 0);

        state.paused = true;
        let pinned_end = state.filtered_seqs.len() - state.scroll_from_bottom;
        for i in 5..8 {
            state.push_entry(to_log_entry(&format!("11-27 10:00:0{i}.000  100  100 I Tag: msg {i}")), &None, &pids);
        }
        assert_eq!(state.filtered_seqs.len(), 8);
        assert_eq!(state.scroll_from_bottom, 3);
        assert_eq!(state.filtered_seqs.len() - state.scroll_from_bottom, pinned_end);
    }

    #[test]
    fn matches_package_pids_with_regex_from_ps_output() {
        let ps_o = "  PID NAME\n  939 com.qytech.nav\n 1541 com.cayin.player\n  723 system_server\n";
        let matched = parse_ps_pids_matching(ps_o, "qytech|cayin");
        assert!(matched.contains("939"));
        assert!(matched.contains("1541"));
        assert!(!matched.contains("723"));

        let ps_full = "USER           PID  PPID     VSZ    RSS WCHAN            ADDR S NAME\nu0_a123       4321   500 1234567  98765 0                   0 S com.example.audio\n";
        let matched_full = parse_ps_pids_matching(ps_full, r"^com\.example\..*$");
        assert!(matched_full.contains("4321"));
    }
}
