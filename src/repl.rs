use crate::adb;
use crate::broadcast;
use crate::cache;
use crate::history;
use crate::logcat;
use crate::selector;
use crate::store::{DeviceTarget, Store};
use anyhow::Result;
use reedline::{ColumnarMenu, Completer, DefaultPrompt, Emacs, FileBackedHistory, Hinter, History as InputHistory,
    HistoryItem, KeyCode, KeyModifiers, MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch,
    Reedline, ReedlineEvent, ReedlineMenu, SearchQuery, Signal, Span, Suggestion};
use rustyline::completion::Pair;
use rustyline::history::{DefaultHistory, History};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RootMode {
    None,
    NativeRoot,
    SuDashC,
    SuZero,
}

enum RootProbeResult {
    AlreadyRoot,
    Mode(RootMode),
    Unavailable,
}

struct ReplState {
    current_target: String,
    remote_cwd: String,
    is_multi: bool,
    root_mode: RootMode,
}

type LsCacheKey = (String, RootMode, String, bool);

struct ReplHelper {
    adb_path: PathBuf,
    state: Arc<Mutex<ReplState>>,
    suggestions: Vec<String>,
    device_groups: Vec<String>,
    ls_cache: Mutex<HashMap<LsCacheKey, (Instant, Vec<String>)>>,
}

impl ReplHelper {
    fn new(adb_path: PathBuf, state: Arc<Mutex<ReplState>>, store: &Store) -> Self {
        let mut suggestions = vec![
            ":help".to_string(),
            ":devices".to_string(),
            ":history".to_string(),
            ":raw".to_string(),
            ":use ".to_string(),
            ":clear".to_string(),
            "exit".to_string(),
            // 常用 Android / Linux 调试命令建议
            "cat /proc/asound/cards".to_string(),
            "cat /proc/cpuinfo".to_string(),
            "cat /proc/meminfo".to_string(),
            "cat /proc/version".to_string(),
            "getprop ro.product.model".to_string(),
            "getprop ro.build.version.release".to_string(),
            "getprop ".to_string(),
            "dumpsys activity top".to_string(),
            "dumpsys activity activities".to_string(),
            "dumpsys audio".to_string(),
            "dumpsys battery".to_string(),
            "dumpsys meminfo ".to_string(),
            "dumpsys window displays".to_string(),
            "pm list packages -3".to_string(),
            "pm list packages".to_string(),
            "pm path ".to_string(),
            "am start -n ".to_string(),
            "am force-stop ".to_string(),
            "settings list system".to_string(),
            "settings list global".to_string(),
            "settings list secure".to_string(),
            "tinymix".to_string(),
            "tinyplay ".to_string(),
            "dmesg | tail -n 50".to_string(),
            "logcat -d -v threadtime".to_string(),
            "ps -A".to_string(),
            "top -n 1".to_string(),
            "ls -la".to_string(),
            "df -h".to_string(),
            "ip addr".to_string(),
            "wm size".to_string(),
            "wm density".to_string(),
            "uname -a".to_string(),
            "input keyevent ".to_string(),
            "input tap ".to_string(),
            "svc wifi enable".to_string(),
            "svc wifi disable".to_string(),
        ];

        for args in store.list_macros().values() {
            if args.first().map(|s| s.as_str()) == Some("shell") && args.len() >= 2 {
                let cmd = args[1..].join(" ");
                if !suggestions.contains(&cmd) {
                    suggestions.push(cmd);
                }
            }
        }

        let device_groups = store.list_groups().keys().cloned().collect();

        Self {
            adb_path,
            state,
            suggestions,
            device_groups,
            ls_cache: Mutex::new(HashMap::new()),
        }
    }
}

struct ShellPrompt<'a>(&'a str);

impl Prompt for ShellPrompt<'_> {
    fn render_prompt_left(&self) -> Cow<'_, str> { Cow::Borrowed(self.0) }
    fn render_prompt_right(&self) -> Cow<'_, str> { Cow::Borrowed("") }
    fn render_prompt_indicator(&self, _: PromptEditMode) -> Cow<'_, str> { Cow::Borrowed("") }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> { Cow::Borrowed("> ") }
    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        DefaultPrompt::default().render_prompt_history_search_indicator(search).into_owned().into()
    }
}

struct ShellHints {
    suggestions: Vec<String>,
    current: String,
}

impl Hinter for ShellHints {
    fn handle(&mut self, line: &str, pos: usize, history: &dyn InputHistory, color: bool, _: &str) -> String {
        self.current.clear();
        if line.is_empty() || pos != line.len() { return String::new(); }
        self.current = history.search(SearchQuery::last_with_prefix(line.into(), None)).ok()
            .and_then(|items| items.first()?.command_line.strip_prefix(line).map(str::to_owned))
            .filter(|hint| !hint.is_empty())
            .or_else(|| suggest_inline_suffix(&self.suggestions, line)).unwrap_or_default();
        if color { format!("\x1b[90m{}\x1b[0m", self.current) } else { self.current.clone() }
    }
    fn complete_hint(&self) -> String { self.current.clone() }
    fn next_hint_token(&self) -> String {
        self.current.split_inclusive(char::is_whitespace).next().unwrap_or("").to_owned()
    }
}

impl Completer for ReplHelper {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let (start, items) = self.candidates(line, pos);
        items.into_iter().map(|item| Suggestion {
            value: item.replacement, display_override: Some(item.display),
            span: Span::new(start, pos), ..Suggestion::default()
        }).collect()
    }
}

impl ReplHelper {
    fn candidates(&self, line: &str, pos: usize) -> (usize, Vec<Pair>) {
        let prefix = &line[..pos];

        // Case A: `:use <target>`
        if let Some(target_prefix) = prefix.strip_prefix(":use ") {
            let clean_prefix = target_prefix.trim_start_matches('@');
            let mut seen = HashSet::new();
            let mut pairs = Vec::new();
            let store = Store::load().ok();

            if "all".starts_with(clean_prefix) && seen.insert("all".to_string()) {
                pairs.push(Pair {
                    display: "all (全部在线设备)".to_string(),
                    replacement: "all".to_string(),
                });
            }

            for grp in &self.device_groups {
                if grp.starts_with(clean_prefix) && seen.insert(grp.clone()) {
                    pairs.push(Pair {
                        display: format!("{grp} (分组)"),
                        replacement: grp.clone(),
                    });
                }
            }

            for dev in cache::get_devices_fast() {
                if let Some(ref st) = store {
                    if let Some(alias) = st.alias_for(&dev.serial) {
                        if alias.starts_with(clean_prefix) && seen.insert(alias.to_string()) {
                            pairs.push(Pair {
                                display: format!("{alias} ({})", dev.model),
                                replacement: alias.to_string(),
                            });
                        }
                    }
                }
                if dev.serial.starts_with(clean_prefix) && seen.insert(dev.serial.clone()) {
                    pairs.push(Pair {
                        display: format!("{} ({})", dev.serial, dev.model),
                        replacement: dev.serial,
                    });
                }
            }

            return (":use ".len(), pairs);
        }

        // Case B: REPL 内置指令 (`:...`)
        if prefix.starts_with(':') && !prefix.contains(' ') {
            let builtins = [":help", ":devices", ":history", ":use ", ":raw", ":clear", ":q"];
            let pairs = builtins
                .iter()
                .filter(|cmd| cmd.starts_with(prefix))
                .map(|cmd| Pair {
                    display: (*cmd).to_string(),
                    replacement: (*cmd).to_string(),
                })
                .collect();
            return (0, pairs);
        }

        // Case C: 首个命令名补全（无空白、非路径前缀）
        if !prefix.contains(char::is_whitespace)
            && !prefix.contains('/')
            && !prefix.starts_with('.')
        {
            let mut seen = HashSet::new();
            let mut pairs = Vec::new();
            for cmd in ["cd", "exit", "quit"]
                .into_iter()
                .chain(self.suggestions.iter().filter_map(|s| s.split_whitespace().next()))
            {
                if cmd.starts_with(':') {
                    continue;
                }
                if cmd.starts_with(prefix) && seen.insert(cmd.to_string()) {
                    pairs.push(Pair {
                        display: cmd.to_string(),
                        replacement: cmd.to_string(),
                    });
                }
            }
            return (0, pairs);
        }

        // Case D: 单机模式下远端设备路径补全
        let (is_multi, serial, cwd, root_mode) = {
            let st = self.state.lock().unwrap();
            (
                st.is_multi,
                st.current_target.clone(),
                st.remote_cwd.clone(),
                st.root_mode,
            )
        };
        if is_multi {
            return (pos, Vec::new());
        }

        let token_start = prefix
            .rfind(char::is_whitespace)
            .map(|idx| idx + 1)
            .unwrap_or(0);
        let token = &prefix[token_start..];
        let (dir_part, name_prefix) = split_remote_path_prefix(token);
        let list_dir = resolve_remote_list_dir(&cwd, dir_part);

        let show_hidden = name_prefix.starts_with('.');
        let cache_key: LsCacheKey = (serial.clone(), root_mode, list_dir.clone(), show_hidden);
        if let Some((ts, cached)) = self.ls_cache.lock().unwrap().get(&cache_key).cloned() {
            if ts.elapsed() < Duration::from_secs(3) {
                let pairs = cached
                    .into_iter()
                    .filter(|entry| entry.starts_with(name_prefix))
                    .map(|entry| Pair {
                        display: entry.clone(),
                        replacement: format!("{dir_part}{entry}"),
                    })
                    .collect();
                return (token_start, pairs);
            }
        }

        let ls_flags = if show_hidden { "-1ap" } else { "-1p" };
        let ls_cmd = wrap_with_root(
            &format!("ls {ls_flags} {} 2>/dev/null", shell_quote(&list_dir)),
            root_mode,
        );

        let Ok(cap) = adb::run_captured_with_timeout(
            &self.adb_path,
            Some(&serial),
            &["shell".into(), ls_cmd],
            2,
        ) else {
            return (pos, Vec::new());
        };

        if cap.exit_code != 0 && cap.stdout.is_empty() {
            return (pos, Vec::new());
        }

        let stdout = String::from_utf8_lossy(&cap.stdout);
        let entries: Vec<String> = stdout
            .lines()
            .map(|l| l.trim_end_matches('\r').trim())
            .filter(|e| !e.is_empty() && *e != "./" && *e != "../")
            .map(|e| e.to_string())
            .collect();

        {
            let mut cache = self.ls_cache.lock().unwrap();
            if cache.len() >= 64 {
                cache.clear();
            }
            cache.insert(cache_key, (Instant::now(), entries.clone()));
        }

        let pairs = entries
            .into_iter()
            .filter(|entry| entry.starts_with(name_prefix))
            .map(|entry| Pair {
                display: entry.clone(),
                replacement: format!("{dir_part}{entry}"),
            })
            .collect();

        (token_start, pairs)
    }
}

fn suggest_inline_suffix(suggestions: &[String], line: &str) -> Option<String> {
    suggestions
        .iter()
        .find(|s| s.starts_with(line) && s.len() > line.len())
        .map(|s| s[line.len()..].to_string())
}

fn split_remote_path_prefix(token: &str) -> (&str, &str) {
    match token.rfind('/') {
        Some(idx) => (&token[..=idx], &token[idx + 1..]),
        None => ("", token),
    }
}

fn resolve_remote_list_dir(cwd: &str, dir_part: &str) -> String {
    if dir_part.is_empty() {
        return if cwd.is_empty() {
            "/".to_string()
        } else {
            cwd.to_string()
        };
    }
    if dir_part.starts_with('/') {
        return dir_part.to_string();
    }
    if cwd == "/" || cwd.is_empty() {
        format!("/{dir_part}")
    } else {
        format!("{}/{}", cwd.trim_end_matches('/'), dir_part)
    }
}

/// 交互式宿主机 Shell REPL：
/// - 实时灰色内联命令建议（`→` / `Ctrl+E` 一键采纳历史或常用 Android 调试命令）
/// - `Tab` 补全常用命令、远端设备路径与 `:use` 目标，`↑`/`↓` 与 `Ctrl+R` 检索历史
/// - 每条输入的远端命令自动写入 history.jsonl（支持后续 `adbx history` 检索与重放）
/// - 支持虚拟工作目录 `cd <dir>`、`:use <设备/all/分组>` 热切换与 `:raw` 切入原生 PTY shell
pub fn run_repl(adb_path: &PathBuf, mut target: DeviceTarget, store: &Store) -> Result<u8> {
    let repl_hist_file = store.repl_history_path();
    let mut saved = DefaultHistory::new();
    let _ = saved.load(&repl_hist_file);
    if saved.is_empty() {
        for entry in history::load_entries(store) {
            if entry.args.first().is_some_and(|a| a == "shell") && entry.args.len() >= 2 {
                let _ = saved.add(&entry.args[1..].join(" "));
            }
        }
    }
    let mut input_history = FileBackedHistory::default();
    for line in &saved {
        input_history.save(HistoryItem::from_command_line(line.as_str()))?;
    }

    if matches!(target, DeviceTarget::Fixed(_)) {
        let online: Vec<_> = adb::list_devices(adb_path)?.into_iter().filter(|d| d.state == "device").collect();
        store.locked_devices(&target, &online)?;
    }
    let mut current_target = target.spec();
    let mut remote_cwd = "/".to_string();
    let mut root_mode = if target.single_serial().is_some()
        && is_native_root(adb_path, &current_target)
    {
        RootMode::NativeRoot
    } else {
        RootMode::None
    };
    let state = Arc::new(Mutex::new(ReplState {
        current_target: current_target.clone(),
        remote_cwd: remote_cwd.clone(),
        is_multi: target.single_serial().is_none(),
        root_mode,
    }));

    let helper = ReplHelper::new(adb_path.clone(), Arc::clone(&state), store);
    let hints = ShellHints { suggestions: helper.suggestions.clone(), current: String::new() };
    let mut bindings = reedline::default_emacs_keybindings();
    bindings.add_binding(KeyModifiers::NONE, KeyCode::Tab, ReedlineEvent::UntilFound(vec![
        ReedlineEvent::Menu("completion".into()), ReedlineEvent::MenuNext,
    ]));
    let interrupt = Arc::new(AtomicBool::new(false));
    let mut editor = Reedline::create()
        .with_history(Box::new(input_history))
        .with_completer(Box::new(helper))
        .with_hinter(Box::new(hints))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(ColumnarMenu::default().with_name("completion"))))
        .with_edit_mode(Box::new(Emacs::new(bindings)))
        .with_break_signal(Arc::clone(&interrupt));
    let mut exit_code = 0;

    eprintln!("已进入 adbx 增强 Shell（实时灰色命令建议按 → / Ctrl+E 采纳，Tab 补全命令与设备路径，↑/↓ 或 Ctrl+R 搜历史，:raw 切入原生 shell，:help 查看帮助）");

    loop {
        let is_multi = target.single_serial().is_none();
        {
            let mut st = state.lock().unwrap();
            st.current_target = current_target.clone();
            st.remote_cwd = remote_cwd.clone();
            st.is_multi = is_multi;
            st.root_mode = root_mode;
        }

        let prompt = if is_multi {
            format!("[@{current_target}] $ ")
        } else {
            let label = store.display_label(&current_target);
            let sym = if root_mode == RootMode::None { '$' } else { '#' };
            format!("[{label}:{remote_cwd}] {sym} ")
        };

        let input = read_input(&mut editor, &prompt, adb_path, (!is_multi).then_some(current_target.as_str()), &interrupt);
        match input {
            Ok(Signal::Success(raw_line)) => {
                let line = raw_line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = saved.add(line);

                if line == ":q" {
                    break;
                }
                if matches!(line, "exit" | "quit") {
                    if !is_multi && matches!(root_mode, RootMode::SuDashC | RootMode::SuZero) {
                        root_mode = RootMode::None;
                        println!("已退出 root 模式，返回普通 Shell ($)");
                        continue;
                    }
                    break;
                }
                if line == ":help" {
                    print_help();
                    continue;
                }
                if line == ":clear" {
                    if std::io::stderr().is_terminal() {
                        eprint!("\x1b[2J\x1b[H");
                        let _ = std::io::stderr().flush();
                    }
                    continue;
                }
                if line == ":raw" {
                    if is_multi {
                        eprintln!("`:raw` 仅支持单机模式，请先使用 `:use <设备别名|序列号>` 切换到单台设备。");
                        continue;
                    }
                    println!("已切换到原生 adb shell（输入 exit 返回 adbx 增强 Shell）");
                    if let Err(e) = adb::run_passthrough(adb_path, Some(&current_target), &["shell".into()]) {
                        eprintln!("启动原生 shell 失败: {e:#}");
                    }
                    continue;
                }
                if line == ":devices" {
                    if let Ok(devs) = adb::list_devices(adb_path) {
                        for d in devs {
                            let alias = store.alias_for(&d.serial).unwrap_or("-");
                            println!("  {:<14} {:<24} {:<10} {}", alias, d.serial, d.state, d.model);
                        }
                    }
                    continue;
                }
                if line == ":history" {
                    let _ = history::list_history(store, 20, None, None, false);
                    continue;
                }
                if let Some(new_target) = line.strip_prefix(":use ") {
                    let clean = new_target.trim().trim_start_matches('@');
                    if clean.is_empty() {
                        println!("用法: :use <别名|序列号|all|分组名>");
                        continue;
                    }
                    let selected = (|| -> Result<DeviceTarget> {
                        let config = Store::load()?;
                        let online: Vec<_> = adb::list_devices(adb_path)?.into_iter().filter(|d| d.state == "device").collect();
                        config.select_target(clean, &online)
                    })();
                    match selected {
                        Ok(next) => {
                            target = next;
                            current_target = target.spec();
                            remote_cwd = "/".into();
                            root_mode = if target.single_serial().is_some() && is_native_root(adb_path, &current_target) {
                                RootMode::NativeRoot
                            } else { RootMode::None };
                            println!("已切换 REPL 目标 -> {current_target}");
                        }
                        Err(e) => eprintln!("切换失败: {e}"),
                    }
                    continue;
                }

                // 单独输入 `su` / `su 0` / `su root` 时不挂起，自动探测并切入 root 提权模式
                if matches!(line, "su" | "su 0" | "su root") {
                    if is_multi {
                        eprintln!("多机模式下不支持交互式切换 `su`，请直接运行如 `su -c id` 或先 `:use <单台设备>`。");
                        continue;
                    }
                    match probe_root_mode(adb_path, &current_target) {
                        RootProbeResult::AlreadyRoot => {
                            root_mode = RootMode::NativeRoot;
                            println!("当前 adb shell 已经是 root (uid=0)");
                        }
                        RootProbeResult::Mode(m) => {
                            root_mode = m;
                            println!("已切换到 root 模式（提示符变为 #，输入 exit 返回普通权限）");
                        }
                        RootProbeResult::Unavailable => {
                            eprintln!("无法非交互切换 root（设备未 root 或需手机端弹窗授权）。可尝试先执行 `adbx root`，或输入 `:raw` 进入原生 shell 后再试。");
                        }
                    }
                    continue;
                }

                // 单机模式下支持 `cd <path>` 维持虚拟工作目录
                if !is_multi && (line == "cd" || line.starts_with("cd ")) {
                    let arg = line.strip_prefix("cd").unwrap_or("").trim();
                    let next_dir = if arg.is_empty() || arg == "~" {
                        "/".to_string()
                    } else {
                        let probe = wrap_with_root(
                            &format!("cd {} && cd {} && pwd", shell_quote(&remote_cwd), shell_quote(arg)),
                            root_mode,
                        );
                        match adb::run_captured_with_timeout(
                            adb_path,
                            Some(&current_target),
                            &["shell".into(), probe],
                            5,
                        ) {
                            Ok(cap) if cap.exit_code == 0 => {
                                let resolved = String::from_utf8_lossy(&cap.stdout).trim().to_string();
                                if resolved.is_empty() { remote_cwd.clone() } else { resolved }
                            }
                            Ok(cap) => {
                                eprintln!("{}", String::from_utf8_lossy(&cap.stderr).trim());
                                continue;
                            }
                            Err(e) => {
                                eprintln!("cd 失败: {e}");
                                continue;
                            }
                        }
                    };
                    remote_cwd = next_dir;
                    continue;
                }

                if !is_multi && adb::is_dangerous_shell_str(line) {
                    let label = store.display_label(&current_target);
                    match selector::confirm_dangerous(&label, &current_target, line) {
                        Ok(true) => {}
                        Ok(false) => {
                            println!("已取消执行。");
                            continue;
                        }
                        Err(e) => {
                            eprintln!("确认失败: {e}");
                            continue;
                        }
                    }
                }

                // 实际执行远端 shell 命令并写入统一 history.jsonl
                let full_cmd = if !is_multi {
                    let base = if remote_cwd != "/" {
                        format!("cd {} && {line}", shell_quote(&remote_cwd))
                    } else {
                        line.to_string()
                    };
                    wrap_with_root(&base, root_mode)
                } else {
                    line.to_string()
                };
                let mut adb_args = vec!["shell".to_string()];
                if !is_multi && needs_remote_pty(line) { adb_args.push("-t".into()); }
                adb_args.push(full_cmd);
                let started = Instant::now();

                let code = if is_multi {
                    let opts = broadcast::BroadcastOptions {
            cwd: None,
                        mode_override: Some(broadcast::OutputMode::Grouped),
                        max_jobs: 8,
                        timeout_secs: 30,
                        assume_yes: false,
                        no_color: false,
                        json_output: false,
                        adb_args: adb_args.clone(),
                    };
                    let result = Store::load().and_then(|cfg| broadcast::run_locked(&target, opts, adb_path, &cfg));
                    match result {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("执行失败: {e:#}");
                            1
                        }
                    }
                } else {
                    let tokens: Vec<String> = line.split_whitespace().map(|s| s.to_string()).collect();
                    let res = if logcat::should_colorize_logcat(&tokens) {
                        logcat::run_colored_stream(adb_path, &current_target, &adb_args, store)
                    } else {
                        adb::run_passthrough(adb_path, Some(&current_target), &adb_args)
                    };
                    match res {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("执行失败: {e:#}");
                            1
                        }
                    }
                };

                if !is_multi {
                    history::record(
                        store, &store.display_label(&current_target), std::slice::from_ref(&current_target),
                        &adb_args, code, started.elapsed().as_millis(),
                    );
                    if code != 0 && disconnected(adb_path, &current_target) {
                        eprintln!("设备 {current_target} 已断开，已退出增强 Shell。");
                        exit_code = 1;
                        break;
                    }
                }
            }
            Ok(Signal::CtrlC) => {
                eprintln!("^C");
                continue;
            }
            Ok(Signal::ExternalBreak(_)) => {
                eprintln!("设备 {current_target} 已断开，已退出增强 Shell。");
                exit_code = 1;
                break;
            }
            Ok(Signal::CtrlD) => {
                break;
            }
            Ok(_) => continue,
            Err(err) => {
                exit_code = 1;
                eprintln!("REPL 读取异常: {err}");
                break;
            }
        }
    }

    let _ = saved.append(&repl_hist_file);
    Ok(exit_code)
}

// A successful server snapshot distinguishes unplugging from a failed remote
// command or a transient error querying adb. Never substitute another serial.
fn disconnected(adb_path: &PathBuf, serial: &str) -> bool {
    let Ok(cap) = adb::run_captured_with_timeout(adb_path, None, &["devices".into()], 2) else {
        return false;
    };
    let output = String::from_utf8_lossy(&cap.stdout);
    let mut lines = output.lines();
    cap.exit_code == 0 && lines.next().is_some_and(|l| l.trim() == "List of devices attached")
        && !lines.any(|l| {
            let mut fields = l.split_whitespace();
            fields.next() == Some(serial) && fields.next() == Some("device")
        })
}

fn read_input(
    editor: &mut Reedline,
    prompt: &str,
    adb_path: &PathBuf,
    serial: Option<&str>,
    interrupt: &Arc<AtomicBool>,
) -> std::io::Result<Signal> {
    if !std::io::stdin().is_terminal() {
        let read_line = || -> std::io::Result<Signal> {
            let mut line = String::new();
            Ok(if std::io::stdin().read_line(&mut line)? == 0 {
                Signal::CtrlD
            } else {
                Signal::Success(line)
            })
        };
        let Some(serial) = serial else {
            return read_line();
        };
        // A pipe may stay open without producing another byte. Only the main
        // thread dispatches commands; an unplug exits the CLI without waiting for EOF.
        let (send, input) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = send.send(read_line());
        });
        loop {
            match input.recv_timeout(Duration::from_secs(1)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) if disconnected(adb_path, serial) => {
                    return Ok(Signal::ExternalBreak(String::new()));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(Signal::CtrlD),
            }
        }
    }
    let mut read = || {
        if std::io::stdout().is_terminal() && std::io::stderr().is_terminal() {
            editor.read_line(&ShellPrompt(prompt))
        } else {
            redirected_line(prompt, interrupt)
        }
    };
    let Some(serial) = serial else {
        return read();
    };
    std::thread::scope(|scope| {
        let (done, stop) = mpsc::channel::<()>();
        let watcher = scope.spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) =
                stop.recv_timeout(Duration::from_secs(1))
            {
                if disconnected(adb_path, serial) {
                    interrupt.store(true, Ordering::Release);
                    return true;
                }
            }
            false
        });
        let result = read();
        drop(done);
        let dropped = watcher.join().expect("device monitor");
        interrupt.store(false, Ordering::Release);
        if dropped {
            Ok(Signal::ExternalBreak(String::new()))
        } else {
            result
        }
    })
}

/// Cursor-position queries go to stdout on some terminals. When stdout is data,
/// keep the prompt and echo on stderr and read keys without a cursor query.
fn redirected_line(prompt: &str, interrupt: &AtomicBool) -> std::io::Result<Signal> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    struct RestoreTerminal;
    impl Drop for RestoreTerminal {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
        }
    }
    enable_raw_mode()?;
    let _restore = RestoreTerminal;
    let mut err = std::io::stderr();
    write!(err, "{prompt}")?;
    err.flush()?;
    let mut line = String::new();
    let result = loop {
        if interrupt.load(Ordering::Acquire) {
            break Signal::ExternalBreak(line);
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Enter => break Signal::Success(line),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                break Signal::CtrlC
            }
            KeyCode::Char('d')
                if line.is_empty() && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                break Signal::CtrlD
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                line.push(c);
                write!(err, "{c}")?;
            }
            KeyCode::Backspace if line.pop().is_some() => write!(err, "\x08 \x08")?,
            _ => {}
        }
        err.flush()?;
    };
    write!(err, "\r\n")?;
    Ok(result)
}

fn wrap_with_root(cmd: &str, mode: RootMode) -> String {
    match mode {
        RootMode::None | RootMode::NativeRoot => cmd.to_string(),
        RootMode::SuDashC => format!("su -c {}", shell_quote(cmd)),
        RootMode::SuZero => format!("su 0 sh -c {}", shell_quote(cmd)),
    }
}

fn is_native_root(adb_path: &PathBuf, serial: &str) -> bool {
    adb::run_captured_with_timeout(adb_path, Some(serial), &["shell".into(), "id".into()], 2)
        .map(|cap| cap.exit_code == 0 && String::from_utf8_lossy(&cap.stdout).contains("uid=0("))
        .unwrap_or(false)
}

fn probe_root_mode(adb_path: &PathBuf, serial: &str) -> RootProbeResult {
    if is_native_root(adb_path, serial) {
        return RootProbeResult::AlreadyRoot;
    }
    if let Ok(cap) = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["shell".into(), "su -c id".into()],
        3,
    ) {
        if cap.exit_code == 0 && String::from_utf8_lossy(&cap.stdout).contains("uid=0(") {
            return RootProbeResult::Mode(RootMode::SuDashC);
        }
    }
    if let Ok(cap) = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["shell".into(), "su 0 id".into()],
        3,
    ) {
        if cap.exit_code == 0 && String::from_utf8_lossy(&cap.stdout).contains("uid=0(") {
            return RootProbeResult::Mode(RootMode::SuZero);
        }
    }
    RootProbeResult::Unavailable
}

fn needs_remote_pty(line: &str) -> bool {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let Some(&first) = tokens.first() else {
        return false;
    };
    let bin = first.rsplit('/').next().unwrap_or(first);
    match bin {
        "vi" | "vim" | "nano" | "less" | "more" | "htop" => true,
        "top" => !tokens.contains(&"-n"),
        _ => false,
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn print_help() {
    println!("adbx 增强 Shell 快捷键与内置指令:");
    println!("  → / Ctrl+E          采纳当前灰色内联命令建议（历史记录优先，其次内置常用命令）");
    println!("  Tab                 补全命令名、远端设备目录/文件（如 cat proc/asound/c）与 :use 目标");
    println!("  ↑ / ↓ 或 Ctrl+R     上下翻阅或反向模糊搜索命令历史");
    println!("  <任意命令>          在目标设备执行 `adb shell <命令>` 并自动记录到 history.jsonl");
    println!("  cd <路径>           单机模式下切换远端工作目录");
    println!("  su                  自动探测并切换为 root 提权模式（提示符变为 #，输入 exit 退回）");
    println!("  :use <别名|all|组>  热切换目标设备或切换为多机并行 REPL");
    println!("  :raw                临时切入原生 `adb shell` 交互式 PTY（`adbx shell --raw` 可直接启动）");
    println!("  :clear              清屏");
    println!("  :devices            列出已连接设备");
    println!("  :history            查看最近 20 条命令历史");
    println!("  exit / Ctrl+D       退出 root 模式或退出 Shell");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggest_matches_builtin_commands() {
        let suggestions = vec![
            "cat /proc/asound/cards".to_string(),
            "getprop ro.product.model".to_string(),
        ];
        assert_eq!(
            suggest_inline_suffix(&suggestions, "cat /proc/a"),
            Some("sound/cards".to_string())
        );
        assert_eq!(
            suggest_inline_suffix(&suggestions, "cat /proc/asound/cards"),
            None
        );
        assert_eq!(suggest_inline_suffix(&suggestions, "unknown"), None);
    }

    #[test]
    fn split_remote_path_prefix_handles_relative_and_absolute() {
        assert_eq!(
            split_remote_path_prefix("proc/asound/c"),
            ("proc/asound/", "c")
        );
        assert_eq!(
            split_remote_path_prefix("/proc/asound/"),
            ("/proc/asound/", "")
        );
        assert_eq!(split_remote_path_prefix("cards"), ("", "cards"));
    }

    #[test]
    fn resolve_remote_list_dir_joins_cwd_correctly() {
        assert_eq!(resolve_remote_list_dir("/", "proc/asound/"), "/proc/asound/");
        assert_eq!(resolve_remote_list_dir("/data/local", "tmp/"), "/data/local/tmp/");
        assert_eq!(resolve_remote_list_dir("/data/local", "/proc/"), "/proc/");
        assert_eq!(resolve_remote_list_dir("/data/local", ""), "/data/local");
    }

    #[test]
    fn detects_interactive_pty_commands() {
        assert!(needs_remote_pty("vi /sdcard/a.txt"));
        assert!(needs_remote_pty("top"));
        assert!(!needs_remote_pty("top -n 1"));
        assert!(!needs_remote_pty("su"));
        assert!(!needs_remote_pty("cat /proc/asound/cards"));
    }

    #[test]
    fn wraps_commands_with_root_mode() {
        assert_eq!(wrap_with_root("id", RootMode::None), "id");
        assert_eq!(wrap_with_root("id", RootMode::NativeRoot), "id");
        assert_eq!(
            wrap_with_root("cat '/data/a'", RootMode::SuDashC),
            "su -c 'cat '\\''/data/a'\\'''"
        );
        assert_eq!(
            wrap_with_root("id", RootMode::SuZero),
            "su 0 sh -c 'id'"
        );
    }
}
