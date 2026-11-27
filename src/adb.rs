use anyhow::{bail, Context, Result};
use regex::Regex;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Once, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

static CTRLC_INIT: Once = Once::new();
static DANGEROUS_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
static PKG_REGEX: OnceLock<Regex> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub serial: String,
    pub state: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct CommandCapture {
    pub exit_code: i32,
    pub timed_out: bool,
    pub elapsed_ms: u128,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// 严格校验 Android 包名格式，杜绝 `adb shell` 命令注入
pub fn validate_package_name(pkg: &str) -> Result<&str> {
    let trimmed = pkg.trim();
    let re = PKG_REGEX.get_or_init(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._]*$").unwrap());
    if trimmed.is_empty() || trimmed.len() > 200 || !re.is_match(trimmed) {
        bail!("非法 Android 包名: '{pkg}'（仅允许字母、数字、点与下划线）");
    }
    Ok(trimmed)
}

/// MCP 安全模式只读命令白名单校验（禁止链式符号/重定向，仅放行已知只读诊断前缀）
pub fn is_readonly_mcp_shell(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return false;
    }
    let forbidden = [';', '|', '&', '`', '$', '>', '<', '\n', '\r'];
    if trimmed.chars().any(|c| forbidden.contains(&c)) {
        return false;
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }

    match tokens[0] {
        "getprop" | "dumpsys" | "ls" | "cat" | "ps" | "wm" | "ip" | "df" | "uname" | "id"
        | "uptime" | "date" | "whoami" | "pidof" => true,
        "top" => tokens.contains(&"-n"),
        "pm" => matches!(tokens.get(1).copied(), Some("list" | "path" | "dump")),
        "settings" => matches!(tokens.get(1).copied(), Some("get" | "list")),
        _ => false,
    }
}

/// 跨平台探测本机 adb / adb.exe 路径
pub fn find_adb_binary() -> Result<PathBuf> {
    if let Ok(path) = which::which("adb") {
        return Ok(path);
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(sdk) = std::env::var("ANDROID_HOME").or_else(|_| std::env::var("ANDROID_SDK_ROOT")) {
        let bin = if cfg!(windows) { "adb.exe" } else { "adb" };
        candidates.push(PathBuf::from(sdk).join("platform-tools").join(bin));
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("Library/Android/sdk/platform-tools/adb"));
    }

    #[cfg(target_os = "linux")]
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("Android/Sdk/platform-tools/adb"));
    }

    #[cfg(target_os = "windows")]
    if let Some(local_app_data) = dirs::data_local_dir() {
        candidates.push(local_app_data.join("Android\\Sdk\\platform-tools\\adb.exe"));
    }

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    bail!("未找到 adb 可执行文件，请将其加入 PATH 或设置 ANDROID_HOME 环境变量")
}

/// 解析 `adb devices -l` 输出
pub fn list_devices(adb_path: &PathBuf) -> Result<Vec<DeviceInfo>> {
    let output = Command::new(adb_path)
        .args(["devices", "-l"])
        .output()
        .with_context(|| format!("执行 {} devices -l 失败", adb_path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("adb devices 执行失败: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut devices = Vec::new();

    for line in stdout.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('*') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(serial) = parts.next() else { continue };
        let Some(state) = parts.next() else { continue };

        let mut model = String::from("-");
        for kv in parts {
            if let Some(v) = kv.strip_prefix("model:") {
                model = v.to_string();
            }
        }
        devices.push(DeviceInfo {
            serial: serial.to_string(),
            state: state.to_string(),
            model,
        });
    }

    Ok(devices)
}

/// 带硬超时与并发管道读取的 adb 执行器
pub fn run_captured_with_timeout(
    adb_path: &PathBuf,
    serial: Option<&str>,
    args: &[String],
    timeout_secs: u64,
) -> Result<CommandCapture> {
    let started = Instant::now();
    let mut cmd = Command::new(adb_path);
    if let Some(s) = serial {
        cmd.arg("-s").arg(s);
    }
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("启动 {} 失败", adb_path.display()))?;

    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();

    let out_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut p) = out_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let err_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut p) = err_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Duration::from_secs(timeout_secs.max(1));
    let mut timed_out = false;
    let exit_code = loop {
        match child.try_wait()? {
            Some(status) => break status.code().unwrap_or(1),
            None => {
                if started.elapsed() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break 124;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    };

    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();

    Ok(CommandCapture {
        exit_code,
        timed_out,
        elapsed_ms: started.elapsed().as_millis(),
        stdout,
        stderr,
    })
}

pub fn is_global_or_explicit_adb_command(args: &[String]) -> bool {
    if args.is_empty() {
        return true;
    }
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-s" | "-d" | "-e" | "-t" | "-H" | "-P" | "-L"))
    {
        return true;
    }
    matches!(
        args[0].as_str(),
        "start-server"
            | "kill-server"
            | "connect"
            | "disconnect"
            | "pair"
            | "version"
            | "help"
            | "--version"
            | "--help"
            | "mdns"
    )
}

pub fn is_dangerous_command(args: &[String]) -> bool {
    if args.is_empty() {
        return false;
    }
    match args[0].as_str() {
        "uninstall" | "reboot" => true,
        "shell" => is_dangerous_shell_str(&args[1..].join(" ")),
        _ => false,
    }
}

pub fn is_dangerous_shell_str(shell_cmd: &str) -> bool {
    let patterns = DANGEROUS_PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)(?:^|[;&|/]\s*|\s)(?:rm|reboot|factory_reset)\b").unwrap(),
            Regex::new(r"(?i)\b(?:pm|cmd\s+package)\s+(?:clear|uninstall)\b").unwrap(),
            Regex::new(r"(?i)\bdd\s+if=").unwrap(),
        ]
    });
    patterns.iter().any(|re| re.is_match(shell_cmd))
}

pub fn ensure_ctrlc_ignored() {
    CTRLC_INIT.call_once(|| {
        let _ = ctrlc::set_handler(|| {});
    });
}

pub fn run_passthrough(adb_path: &PathBuf, serial: Option<&str>, args: &[String]) -> Result<u8> {
    ensure_ctrlc_ignored();

    let mut cmd = Command::new(adb_path);
    if let Some(s) = serial {
        cmd.arg("-s").arg(s);
    }
    cmd.args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = cmd
        .status()
        .with_context(|| format!("启动 {} 失败", adb_path.display()))?;

    Ok(status.code().unwrap_or(1) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_package_name_against_injection() {
        assert!(validate_package_name("com.example.app_1").is_ok());
        assert!(validate_package_name("com.foo; rm -rf /sdcard").is_err());
        assert!(validate_package_name("com.foo && reboot").is_err());
        assert!(validate_package_name("$(whoami)").is_err());
    }

    #[test]
    fn readonly_mcp_shell_blocks_writes_and_chains() {
        assert!(is_readonly_mcp_shell("getprop ro.product.model"));
        assert!(is_readonly_mcp_shell("pm list packages -3"));
        assert!(is_readonly_mcp_shell("dumpsys activity top"));
        assert!(!is_readonly_mcp_shell("getprop; rm -rf /sdcard"));
        assert!(!is_readonly_mcp_shell("/system/bin/rm /sdcard/a"));
        assert!(!is_readonly_mcp_shell("pm clear com.example.app"));
        assert!(!is_readonly_mcp_shell("settings put global airplane_mode_on 1"));
    }

    #[test]
    fn dangerous_shell_avoids_false_positives_and_catches_bypasses() {
        assert!(!is_dangerous_shell_str("echo confirm ok"));
        assert!(!is_dangerous_shell_str("dumpsys alarm"));
        assert!(is_dangerous_shell_str("rm /sdcard/test.txt"));
        assert!(is_dangerous_shell_str("rm\t-rf /sdcard/tmp"));
        assert!(is_dangerous_shell_str("/system/bin/rm /sdcard/a"));
        assert!(is_dangerous_shell_str("cmd package uninstall com.foo"));
    }

    #[test]
    fn detects_global_adb_and_dangerous_commands() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<String>>();
        assert!(is_global_or_explicit_adb_command(&s(&["start-server"])));
        assert!(is_global_or_explicit_adb_command(&s(&["-s", "ABC", "shell", "id"])));
        assert!(!is_global_or_explicit_adb_command(&s(&["shell", "id"])));

        assert!(is_dangerous_command(&s(&["uninstall", "com.foo"])));
        assert!(is_dangerous_command(&s(&["reboot"])));
        assert!(is_dangerous_command(&s(&["shell", "rm", "-rf", "/sdcard/x"])));
        assert!(!is_dangerous_command(&s(&["shell", "ls", "/sdcard"])));
    }
}
