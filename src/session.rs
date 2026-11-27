/// 生成跨平台终端会话 ID（实现不同终端标签页各自锁定不同手机，互不串机）
pub fn current_session_id() -> String {
    // 1. Windows Terminal / macOS Terminal / iTerm2 / tmux / WezTerm / IDE 终端环境变量
    for env_key in [
        "WT_SESSION",
        "TERM_SESSION_ID",
        "TMUX_PANE",
        "WEZTERM_PANE",
        "VSCODE_IPC_HOOK_CLI",
        "CONEMUPID",
        "TERMINAL_EMULATOR",
    ] {
        if let Ok(val) = std::env::var(env_key) {
            if !val.trim().is_empty() {
                return format!("{env_key}:{}", val.trim());
            }
        }
    }

    // 2. Unix/macOS：读取 stdin 关联的 TTY，其次回退到父 Shell 进程 PID
    #[cfg(unix)]
    {
        if let Ok(tty_path) = std::fs::read_link("/dev/fd/0") {
            let s = tty_path.to_string_lossy();
            if s.starts_with("/dev/") {
                return format!("tty:{s}");
            }
        }
        format!("ppid:{}", std::os::unix::process::parent_id())
    }

    // 3. Windows 非 Windows Terminal 环境：结合 TERM_PROGRAM / SESSIONNAME 区分来源
    #[cfg(windows)]
    {
        let prog = std::env::var("TERM_PROGRAM")
            .or_else(|_| std::env::var("SESSIONNAME"))
            .unwrap_or_else(|_| "console".to_string());
        format!("win:{prog}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_session_id_is_non_empty() {
        assert!(!current_session_id().trim().is_empty());
    }
}
