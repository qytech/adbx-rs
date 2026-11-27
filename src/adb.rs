use anyhow::{bail, Context, Result};
use regex::Regex;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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

/// MCP 安全模式只接受字面量参数；同时具备读写能力的命令仅放行明确的查询形式。
pub fn is_readonly_mcp_shell(cmd: &str) -> bool {
    if !cmd
        .chars()
        .all(|c| c.is_alphanumeric() || " \t_-/.:=,+%@".contains(c))
    {
        return false;
    }
    let tokens: Vec<_> = cmd.split_whitespace().collect();
    match tokens.as_slice() {
        ["getprop" | "ls" | "cat" | "ps" | "df" | "uname" | "id" | "uptime" | "whoami" | "pidof", ..]
        | ["pm", "list", ..]
        | ["settings", "get", "system" | "secure" | "global", _]
        | ["settings", "list", "system" | "secure" | "global"]
        | ["wm", "size" | "density"]
        | ["ip", "addr" | "link" | "route"]
        | ["ip", "addr" | "link" | "route", "show", ..]
        | ["date"]
        | ["date", "-u"]
        | ["dumpsys"]
        | ["dumpsys", "-l" | "activity" | "battery" | "meminfo" | "package" | "window"]
        | ["dumpsys", "activity", "top" | "activities" | "processes" | "services"]
        | ["dumpsys", "window", "windows" | "displays"] => true,
        ["pm", "path" | "dump", pkg] => validate_package_name(pkg).is_ok(),
        ["dumpsys", "meminfo" | "package", pkg] => {
            (*pkg == "android" || pkg.contains('.')) && validate_package_name(pkg).is_ok()
        }
        ["date", fmt] | ["date", "-u", fmt] => fmt.starts_with('+'),
        ["top", "-n", n] | ["top", "-b", "-n", n] => n.parse::<u32>().is_ok_and(|n| n > 0),
        _ => false,
    }
}

pub const MANAGED_TOOLS_MARKER: &str = ".adbx-managed";

/// 返回 adbx 托管下载的官方 Android Platform-Tools 中 `adb` 可执行文件路径
pub fn managed_adb_path() -> Option<PathBuf> {
    let base = match std::env::var("ADBX_DATA_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
        _ => dirs::data_local_dir()?.join("adbx"),
    };
    let bin = if cfg!(windows) { "adb.exe" } else { "adb" };
    Some(base.join("platform-tools").join(bin))
}

/// 返回当前平台对应的 Google 官方 Android Platform-Tools 下载地址
pub fn platform_tools_url() -> Result<&'static str> {
    #[cfg(target_os = "macos")]
    {
        Ok("https://dl.google.com/android/repository/platform-tools-latest-darwin.zip")
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Ok("https://dl.google.com/android/repository/platform-tools-latest-linux.zip")
    }
    #[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
    {
        bail!("官方 Linux Platform-Tools 归档为 x86_64。请通过发行版包管理器安装当前架构的 adb，再运行 adbx。")
    }
    #[cfg(target_os = "windows")]
    {
        Ok("https://dl.google.com/android/repository/platform-tools-latest-windows.zip")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        bail!("当前操作系统暂不支持自动下载 Android Platform-Tools，请手动安装 adb 并加入 PATH")
    }
}

/// 跨平台探测本机 adb / adb.exe 路径（含 adbx 自动托管目录）
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

    if let Some(managed) = managed_adb_path() {
        candidates.push(managed);
    }

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    bail!("未找到 adb 可执行文件，请运行 `adbx setup-adb` 自动下载官方 Platform-Tools，或将其加入 PATH / 设置 ANDROID_HOME")
}

/// 下载并安装 Google 官方 Android Platform-Tools 到 adbx 本地托管目录
pub fn install_managed_platform_tools(force: bool) -> Result<PathBuf> {
    let target_adb = managed_adb_path().context("无法确定 adbx 本地数据目录")?;
    let tools_dir = target_adb.parent().context("无效的 platform-tools 路径")?;
    let install_root = target_adb
        .parent()
        .and_then(|p| p.parent())
        .context("无效的托管 adb 路径")?
        .to_path_buf();

    if !force && target_adb.is_file() {
        verify_managed_adb(&target_adb)?;
        println!("已检测到托管的 adb: {}", target_adb.display());
        return Ok(target_adb);
    }

    let url = platform_tools_url()?;
    std::fs::create_dir_all(&install_root)?;
    // 只给 adbx 新建的子目录加标记，已有 SDK 即使被复用也不取得其所有权。
    match std::fs::create_dir(tools_dir) {
        Ok(()) => std::fs::write(tools_dir.join(MANAGED_TOOLS_MARKER), b"adbx\n")?,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && tools_dir.is_dir() => {}
        Err(err) => return Err(err).context("无法创建托管 platform-tools 目录"),
    }
    let zip_path = install_root.join("platform-tools-download.zip");
    println!("正在从 Google 官方源下载 Android Platform-Tools...");
    println!("  {url}");

    #[cfg(unix)]
    {
        let dl = Command::new("curl")
            .args(["-fSL", url, "-o"])
            .arg(&zip_path)
            .status()
            .context("执行 curl 下载失败，请确认本机已安装 curl")?;
        if !dl.success() {
            let _ = std::fs::remove_file(&zip_path);
            bail!("下载 Android Platform-Tools 失败");
        }
        let ex = Command::new("unzip")
            .args(["-oq"])
            .arg(&zip_path)
            .arg("-d")
            .arg(&install_root)
            .status()
            .context("执行 unzip 解压失败，请确认本机已安装 unzip")?;
        let _ = std::fs::remove_file(&zip_path);
        if !ex.success() {
            bail!("解压 Android Platform-Tools 失败");
        }
        if target_adb.is_file() {
            let mut perms = std::fs::metadata(&target_adb)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&target_adb, perms)?;
        }
    }

    #[cfg(windows)]
    {
        let ps_cmd = format!(
            "Invoke-WebRequest -Uri '{}' -OutFile '{}'; Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
            url,
            zip_path.display().to_string().replace('\'', "''"),
            zip_path.display().to_string().replace('\'', "''"),
            install_root.display().to_string().replace('\'', "''"),
        );
        let status = Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps_cmd])
            .status()
            .context("调用 PowerShell 下载并解压 Platform-Tools 失败")?;
        let _ = std::fs::remove_file(&zip_path);
        if !status.success() {
            bail!("下载或解压 Android Platform-Tools 失败");
        }
    }

    if !target_adb.is_file() {
        bail!("未在解压目录中找到 {}", target_adb.display());
    }
    verify_managed_adb(&target_adb)?;
    println!("已安装官方 adb 到: {}", target_adb.display());
    Ok(target_adb)
}

fn verify_managed_adb(path: &PathBuf) -> Result<()> {
    let cap = run_captured_with_timeout(path, None, &["version".into()], 5).with_context(|| {
        format!(
            "运行 {} version 失败，请检查平台架构或使用系统 adb",
            path.display()
        )
    })?;
    if cap.exit_code != 0 || !String::from_utf8_lossy(&cap.stdout).contains("Android Debug Bridge")
    {
        bail!(
            "{} 未通过 adb version 验证 (exit={})",
            path.display(),
            cap.exit_code
        );
    }
    Ok(())
}

/// 查找 adb，若交互终端下缺失则提示一键自动下载安装官方 Platform-Tools
pub fn find_or_prompt_adb() -> Result<PathBuf> {
    match find_adb_binary() {
        Ok(path) => Ok(path),
        Err(err) => {
            if crate::selector::is_interactive_tty() {
                let confirm = dialoguer::Confirm::new()
                    .with_prompt("未检测到本机 adb，是否立即自动下载安装官方 Android Platform-Tools？")
                    .default(true)
                    .interact()?;
                if confirm {
                    return install_managed_platform_tools(false);
                }
            }
            Err(err)
        }
    }
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
    run_captured_in(adb_path, serial, args, timeout_secs, None)
}

pub fn run_captured_in(
    adb_path: &PathBuf,
    serial: Option<&str>,
    args: &[String],
    timeout_secs: u64,
    cwd: Option<&Path>,
) -> Result<CommandCapture> {
    let started = Instant::now();
    let mut cmd = Command::new(adb_path);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
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

// ADB 客户端的顶层命令，包括 help 中省略的命令；设备上的命令由 shell 自行解析。
pub const NATIVE_COMMANDS: &[&str] = &[
    "abb", "attach", "backup", "bugreport", "connect", "detach", "devices", "disable-verity",
    "disconnect", "emu", "enable-verity", "exec-in", "exec-out", "features", "fork-server",
    "forward", "get-devpath", "get-serialno", "get-state", "help", "host-features", "inc-server",
    "install", "install-multi-package", "install-multiple", "jdwp", "keygen", "kill-server",
    "logcat", "lolcat", "longcat", "ls", "mdns", "nodaemon", "pair", "pubkey", "pull", "push",
    "raw", "reboot", "reboot-bootloader", "reboot-fastboot", "reconnect", "remount", "rescue",
    "restore", "reverse", "root", "server", "server-status", "shell", "sideload", "start-server",
    "sync", "tcpip", "track-app", "track-devices", "track-jdwp", "transport-id", "uninstall",
    "unroot", "usb", "version",
];

pub fn is_native_command(name: &str) -> bool {
    if let Some(spec) = name.strip_prefix("wait-for-") {
        let mut parts = spec.split('-').peekable();
        if matches!(parts.peek(), Some(&("usb" | "local" | "any"))) {
            parts.next();
        }
        return parts.peek().is_some() && parts.all(|state| {
            matches!(state, "device" | "recovery" | "rescue" | "sideload" | "bootloader" | "disconnect" | "any")
        });
    }
    NATIVE_COMMANDS.contains(&name) || matches!(name, "--help" | "--version" | "/?")
}

pub fn is_global_or_explicit_adb_command(args: &[String]) -> bool {
    if args.is_empty() {
        return true;
    }
    matches!(
        args[0].as_str(),
        "-s" | "-d"
            | "-e"
            | "-t"
            | "-H"
            | "-P"
            | "-L"
            | "start-server"
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

/// Native adb accepts a wait-for-* prefix followed by one ordinary command.
pub fn after_wait_prefix(args: &[String]) -> &[String] {
    if args.len() > 1 && args[0].starts_with("wait-for-") { &args[1..] } else { args }
}

pub fn is_dangerous_command(args: &[String]) -> bool {
    let args = after_wait_prefix(args);
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
    run_passthrough_in(adb_path, serial, args, None)
}

pub fn run_passthrough_in(
    adb_path: &PathBuf,
    serial: Option<&str>,
    args: &[String],
    cwd: Option<&Path>,
) -> Result<u8> {
    ensure_ctrlc_ignored();

    let mut cmd = Command::new(adb_path);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
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
    fn recognizes_native_wait_commands() {
        for name in ["wait-for-device", "wait-for-usb-recovery", "wait-for-any-device-disconnect"] {
            assert!(is_native_command(name), "{name}");
        }
        for name in ["wait-for-", "wait-for-usb", "wait-for-any", "wait-for-usb-typo"] {
            assert!(!is_native_command(name), "{name}");
        }
    }

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
        assert!(is_readonly_mcp_shell("wm size"));
        assert!(is_readonly_mcp_shell("date +%F"));
        assert!(!is_readonly_mcp_shell("wm size 800x600"));
        assert!(!is_readonly_mcp_shell("date 010100002030"));
        assert!(!is_readonly_mcp_shell("getprop\n"));
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

    #[test]
    fn platform_tools_url_and_managed_path_are_valid() {
        if cfg!(all(target_os = "linux", not(target_arch = "x86_64"))) {
            assert!(platform_tools_url().is_err());
        } else {
            let url = platform_tools_url().unwrap();
            assert!(url.starts_with("https://dl.google.com/android/repository/platform-tools-latest-"));
            assert!(url.ends_with(".zip"));
        }

        let path = managed_adb_path().unwrap();
        let expected_name = if cfg!(windows) { "adb.exe" } else { "adb" };
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some(expected_name));
        assert_eq!(
            path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()),
            Some("platform-tools")
        );
    }
}
