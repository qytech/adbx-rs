use crate::selector;
use anyhow::{bail, Context, Result};
use dialoguer::{Confirm, MultiSelect};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_REPO: &str = "qytech/adbx-rs";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum InstallSource {
    Homebrew,
    Scoop,
    Chocolatey,
    Winget,
    Cargo,
    SystemPkg,
    DirectBinary,
}

impl InstallSource {
    fn label(&self) -> &'static str {
        match self {
            InstallSource::Homebrew => "Homebrew",
            InstallSource::Scoop => "Scoop",
            InstallSource::Chocolatey => "Chocolatey",
            InstallSource::Winget => "Winget",
            InstallSource::Cargo => "Cargo",
            InstallSource::SystemPkg => "系统包管理器",
            InstallSource::DirectBinary => "独立二进制 / 安装脚本",
        }
    }
}

#[derive(Debug, Clone)]
struct DetectedInstall {
    display_path: PathBuf,
    canonical_path: PathBuf,
    source: InstallSource,
}

/// 判断 `adbx uninstall ...` 参数是否是在卸载手机端 APK（而非卸载宿主机 `adbx` 自身）
pub fn is_adb_package_uninstall_args(args: &[String]) -> bool {
    args.iter().any(|arg| {
        let a = arg.as_str();
        if a == "-k" || a == "--user" || a.starts_with("--user=") {
            return true;
        }
        !matches!(
            a,
            "--self" | "--purge" | "--dry-run" | "-y" | "--yes" | "-h" | "--help"
        )
    })
}

fn classify_install_source(path: &Path) -> InstallSource {
    let s = path.to_string_lossy().to_ascii_lowercase().replace('\\', "/");
    if s.contains("/cellar/adbx/") || s.contains("/homebrew/") || s.contains("/linuxbrew/") {
        InstallSource::Homebrew
    } else if s.contains("/scoop/apps/adbx/") || s.contains("/scoop/shims/") {
        InstallSource::Scoop
    } else if s.contains("/chocolatey/lib/adbx/") || s.contains("/chocolatey/bin/") {
        InstallSource::Chocolatey
    } else if s.contains("/winget/packages/") || s.contains("/microsoft/winget/links/") {
        InstallSource::Winget
    } else if s.contains("/.cargo/bin/") {
        InstallSource::Cargo
    } else if s == "/usr/bin/adbx" {
        InstallSource::SystemPkg
    } else {
        InstallSource::DirectBinary
    }
}

fn windows_package_root(path: &Path, source: &InstallSource) -> Option<PathBuf> {
    let markers: &[&str] = match source {
        InstallSource::Scoop => &["/scoop/apps/adbx/", "/scoop/shims/"],
        InstallSource::Chocolatey => &["/chocolatey/lib/adbx/", "/chocolatey/bin/"],
        InstallSource::Winget => &["/winget/packages/", "/winget/links/"],
        _ => return None,
    };
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let normalized = normalized.strip_prefix("//?/").unwrap_or(&normalized);
    markers.iter().find_map(|marker| {
        let (prefix, _) = normalized.split_once(marker)?;
        Some(PathBuf::from(format!(
            "{prefix}/{}",
            marker.split('/').nth(1)?
        )))
    })
}

fn discover_installations() -> Vec<DetectedInstall> {
    let bin_name = if cfg!(windows) { "adbx.exe" } else { "adbx" };
    let mut candidates = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe);
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            candidates.push(dir.join(bin_name));
        }
    }

    #[cfg(unix)]
    {
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join(".local/bin/adbx"));
            candidates.push(home.join(".cargo/bin/adbx"));
        }
        candidates.push(PathBuf::from("/opt/homebrew/bin/adbx"));
        candidates.push(PathBuf::from("/usr/local/bin/adbx"));
        candidates.push(PathBuf::from("/usr/bin/adbx"));
    }

    #[cfg(windows)]
    {
        if let Some(local_app_data) = dirs::data_local_dir() {
            candidates.push(local_app_data.join("adbx").join("bin").join("adbx.exe"));
            candidates.push(
                local_app_data
                    .join("Microsoft")
                    .join("WinGet")
                    .join("Links")
                    .join("adbx.exe"),
            );
        }
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join(".cargo").join("bin").join("adbx.exe"));
            candidates.push(home.join("scoop").join("shims").join("adbx.exe"));
        }
        if let Some(program_data) = std::env::var_os("ProgramData") {
            candidates.push(
                PathBuf::from(program_data)
                    .join("chocolatey")
                    .join("bin")
                    .join("adbx.exe"),
            );
        }
    }

    collect_installations(candidates)
}

fn collect_installations(candidates: Vec<PathBuf>) -> Vec<DetectedInstall> {
    let mut seen_packages = HashSet::new();
    let mut seen_canonical = HashSet::new();
    let mut found = Vec::new();
    for raw in candidates {
        if !raw.is_file() {
            continue;
        }
        let canonical = fs::canonicalize(&raw).unwrap_or_else(|_| raw.clone());
        if !seen_canonical.insert(canonical.clone()) {
            continue;
        }
        let source = match classify_install_source(&canonical) {
            InstallSource::DirectBinary => classify_install_source(&raw),
            other => other,
        };
        // Scoop and Chocolatey shims are distinct executable files, not symlinks.
        if let Some(root) = windows_package_root(&canonical, &source)
            .or_else(|| windows_package_root(&raw, &source))
        {
            if !seen_packages.insert((source.clone(), root)) {
                continue;
            }
        }
        found.push(DetectedInstall {
            display_path: raw,
            canonical_path: canonical,
            source,
        });
    }
    found
}

enum PurgeItem {
    File(PathBuf),
    ManagedTools(PathBuf),
}

impl PurgeItem {
    fn path(&self) -> &Path {
        match self {
            Self::File(p) | Self::ManagedTools(p) => p,
        }
    }

    fn remove(&self) -> Result<()> {
        let result = match self {
            Self::File(p) => fs::remove_file(p),
            Self::ManagedTools(p) => fs::remove_dir_all(p),
        };
        match result {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result.with_context(|| format!("清理 {} 失败", self.path().display())),
        }
    }
}

fn discover_purge_items() -> Vec<PurgeItem> {
    let config_dir = match std::env::var("ADBX_CONFIG_DIR") {
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v.trim())),
        _ => dirs::config_dir().map(|d| d.join("adbx")),
    };
    let cache_dir = match std::env::var("ADBX_CACHE_DIR") {
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v.trim())),
        _ => dirs::cache_dir().map(|d| d.join("adbx")),
    };
    let data_dir = match std::env::var("ADBX_DATA_DIR") {
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v.trim())),
        _ => dirs::data_local_dir().map(|d| d.join("adbx")),
    };

    let default_data = dirs::data_local_dir().map(|d| d.join("adbx"));
    purge_items_in(
        config_dir.as_deref(),
        cache_dir.as_deref(),
        data_dir.as_deref(),
        default_data.as_deref(),
    )
}

fn purge_items_in(
    config: Option<&Path>,
    cache: Option<&Path>,
    data: Option<&Path>,
    default_data: Option<&Path>,
) -> Vec<PurgeItem> {
    let mut items = Vec::new();
    for (root, names) in [
        (
            config,
            &[
                "devices.toml",
                "sessions.json",
                "history.jsonl",
                "repl_history.txt",
            ][..],
        ),
        (cache, &["completion_cache.json"][..]),
    ] {
        if let Some(root) = root {
            for name in names {
                let file = root.join(name);
                if file.is_file() || file.is_symlink() {
                    items.push(PurgeItem::File(file));
                }
            }
        }
    }
    if let Some(root) = data {
        let tools = root.join("platform-tools");
        let binary = tools.join(if cfg!(windows) { "adb.exe" } else { "adb" });
        // 旧版本未写归属标记，仅在默认专用数据目录识别此类旧安装。
        let legacy_install = Some(root) == default_data && !root.is_symlink() && binary.is_file();
        if tools.is_dir()
            && !tools.is_symlink()
            && (legacy_install
                || fs::read(tools.join(crate::adb::MANAGED_TOOLS_MARKER))
                    .is_ok_and(|s| s == b"adbx\n"))
        {
            items.push(PurgeItem::ManagedTools(tools));
        }
    }
    items
}

pub fn run_uninstall(purge: bool, dry_run: bool, yes: bool) -> Result<u8> {
    let installs = discover_installations();
    let purge_items = if purge {
        discover_purge_items()
    } else {
        Vec::new()
    };

    if installs.is_empty() && purge_items.is_empty() {
        println!("未检测到本机安装的 adbx（也无待清理的数据目录）。");
        return Ok(0);
    }

    println!("检测到以下 adbx 安装：");
    for item in &installs {
        if item.canonical_path != item.display_path {
            println!(
                "  - [{}] {} -> {}",
                item.source.label(),
                item.display_path.display(),
                item.canonical_path.display()
            );
        } else {
            println!("  - [{}] {}", item.source.label(), item.display_path.display());
        }
    }
    if purge && !purge_items.is_empty() {
        println!("待清理文件与托管工具 (--purge)：");
        for item in &purge_items {
            println!("  - {}", item.path().display());
        }
    }

    if dry_run {
        println!("--dry-run 仅预览，未执行任何删除。");
        return Ok(0);
    }

    let selected_indices: Vec<usize> = if yes {
        (0..installs.len()).collect()
    } else {
        if !selector::is_interactive_tty() {
            bail!("非交互终端下拒绝执行卸载，请显式追加 --yes 确认（或先用 --dry-run 预览）");
        }
        let chosen = if installs.len() > 1 {
            let labels: Vec<String> = installs
                .iter()
                .map(|item| format!("[{}] {}", item.source.label(), item.display_path.display()))
                .collect();
            let defaults = vec![true; labels.len()];
            let picked = MultiSelect::new()
                .with_prompt("检测到多份安装，请选择要卸载的目标（空格切换，回车确认）")
                .items(&labels)
                .defaults(&defaults)
                .interact()?;
            if picked.is_empty() && !purge {
                println!("未选择任何卸载项，已取消。");
                return Ok(0);
            }
            picked
        } else {
            (0..installs.len()).collect()
        };

        let confirmed = Confirm::new()
            .with_prompt("确认执行以上卸载/清理操作？")
            .default(false)
            .interact()?;
        if !confirmed {
            println!("已取消卸载。");
            return Ok(0);
        }
        chosen
    };

    let current_exe = std::env::current_exe()
        .ok()
        .map(|p| fs::canonicalize(&p).unwrap_or(p));
    let mut had_manual_step = false;

    for idx in selected_indices {
        let item = &installs[idx];
        had_manual_step |= uninstall_one(item, current_exe.as_deref())?;
    }

    for item in purge_items {
        item.remove()?;
        println!("已清理: {}", item.path().display());
    }

    if !had_manual_step {
        println!("adbx 卸载完成。");
    }
    #[cfg(unix)]
    println!("提示：如当前终端缓存了旧路径，可执行 `hash -r`；若此前配置过补全或 PATH，可按需从 ~/.zshrc / ~/.bashrc 中移除。");
    #[cfg(windows)]
    println!("提示：如此前在 $PROFILE 中启用过补全，可按需移除。");

    Ok(0)
}

fn uninstall_one(item: &DetectedInstall, current_exe: Option<&Path>) -> Result<bool> {
    match item.source {
        InstallSource::Homebrew => {
            let brew = owning_brew(item)?;
            run_pkg_uninstall(&brew, &["uninstall", "adbx"])?;
        }
        InstallSource::Scoop | InstallSource::Chocolatey => {
            let root = windows_package_root(&item.canonical_path, &item.source)
                .or_else(|| windows_package_root(&item.display_path, &item.source))
                .context("无法确定包管理器安装根目录")?;
            if item.source == InstallSource::Scoop {
                run_pkg_uninstall(&root.join("shims/scoop.cmd"), &["uninstall", "adbx"])?;
            } else {
                run_pkg_uninstall(&root.join("bin/choco.exe"), &["uninstall", "adbx", "-y"])?;
            }
        }
        InstallSource::Winget => {
            run_pkg_uninstall(Path::new("winget"), &["uninstall", "--id", "qytech.adbx"])?
        }
        InstallSource::SystemPkg => {
            println!(
                    "请使用系统包管理器手动卸载 {}（例如 `sudo apt remove adbx` 或 `sudo pacman -R adbx`）。",
                    item.display_path.display()
                );
            return Ok(true);
        }
        InstallSource::Cargo | InstallSource::DirectBinary => {
            remove_binary_file(&item.display_path, current_exe)?;
        }
    }
    Ok(false)
}

// A machine can have multiple Homebrew prefixes. Verify the selected file
// belongs to this brew's formula cellar before asking it to uninstall anything.
fn owning_brew(item: &DetectedInstall) -> Result<PathBuf> {
    let cellar_brew = item
        .canonical_path
        .ancestors()
        .find(|p| p.file_name().is_some_and(|name| name == "Cellar"))
        .and_then(Path::parent)
        .map(|prefix| prefix.join("bin/brew"));
    let linked_brew = item.display_path.parent().map(|bin| bin.join("brew"));
    for brew in [cellar_brew, linked_brew, which::which("brew").ok()]
        .into_iter()
        .flatten()
    {
        if !brew.is_file() {
            continue;
        }
        let Ok(out) = Command::new(&brew).args(["--cellar", "adbx"]).output() else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let cellar = String::from_utf8_lossy(&out.stdout);
        if !cellar.trim().is_empty()
            && fs::canonicalize(cellar.trim())
                .is_ok_and(|path| item.canonical_path.starts_with(path))
        {
            return Ok(brew);
        }
    }
    bail!(
        "无法确认管理 {} 的 Homebrew 安装，请使用对应前缀的 brew 卸载",
        item.display_path.display()
    )
}

fn run_pkg_uninstall(cmd: &Path, args: &[&str]) -> Result<()> {
    println!("执行: {} {}", cmd.display(), args.join(" "));
    let status = Command::new(cmd)
        .args(args)
        .status()
        .with_context(|| format!("调用 {} 失败", cmd.display()))?;
    if !status.success() {
        bail!(
            "`{} {}` 执行失败（退出码 {:?}）",
            cmd.display(),
            args.join(" "),
            status.code()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn remove_binary_file(path: &Path, _current_exe: Option<&Path>) -> Result<()> {
    fs::remove_file(path)
        .with_context(|| format!("删除 {} 失败（可能需要 sudo 权限）", path.display()))?;
    println!("已删除: {}", path.display());
    Ok(())
}

#[cfg(windows)]
fn remove_binary_file(path: &Path, current_exe: Option<&Path>) -> Result<()> {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if current_exe.is_some_and(|cur| cur == canonical) {
        Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "Wait-Process -Id ([int]$env:ADBX_UNINSTALL_PID) -ErrorAction SilentlyContinue; Remove-Item -LiteralPath $env:ADBX_UNINSTALL_FILE -Force -ErrorAction Stop"])
            .env("ADBX_UNINSTALL_PID", std::process::id().to_string())
            .env("ADBX_UNINSTALL_FILE", path)
            .spawn()
            .with_context(|| format!("计划延迟删除 {} 失败", path.display()))?;
        println!("已安排在进程退出后删除当前运行的 {}", path.display());
        return Ok(());
    }
    fs::remove_file(path).with_context(|| format!("删除 {} 失败", path.display()))?;
    println!("已删除: {}", path.display());
    Ok(())
}

pub fn run_update(check_only: bool, target_version: Option<String>, force: bool) -> Result<u8> {
    let repo = std::env::var("ADBX_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_string());

    let current_exe = std::env::current_exe().context("无法获取当前 adbx 可执行文件路径")?;
    if !check_only && !force {
        if let Some(hint) = detect_package_manager_hint(&current_exe) {
            println!("检测到当前 adbx 由包管理器或系统目录托管（{}）。", current_exe.display());
            println!("建议直接使用: {hint}");
            println!("若仍希望就地覆盖更新，可运行: adbx update --force");
            return Ok(0);
        }
    }

    let tag = match target_version {
        Some(v) => normalize_tag(&v),
        None => fetch_latest_tag(&repo)?,
    };

    if check_only {
        if is_newer_version(CURRENT_VERSION, &tag) {
            println!("发现新版本: v{CURRENT_VERSION} -> {tag}");
            println!("运行 `adbx update` 即可自动升级。");
        } else {
            println!("当前已是最新版本: v{CURRENT_VERSION} (远端最新: {tag})");
        }
        return Ok(0);
    }

    if !force && !is_newer_version(CURRENT_VERSION, &tag) {
        println!("当前已是最新版本 v{CURRENT_VERSION}（目标: {tag}），无需更新（如需覆盖可加 `--force`）。");
        return Ok(0);
    }

    let (triple, ext) = current_target_triple()?;
    let archive = format!("adbx-{tag}-{triple}.{ext}");
    let base_url = format!("https://github.com/{repo}/releases/download/{tag}");

    let tmp_dir = std::env::temp_dir().join(format!("adbx-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir)?;
    let _guard = TempDirGuard(tmp_dir.clone());

    let archive_path = tmp_dir.join(&archive);
    let sums_path = tmp_dir.join("SHA256SUMS.txt");

    println!("正在从 {repo} 下载 {archive} ({tag})...");
    download_file(&format!("{base_url}/{archive}"), &archive_path)?;
    download_file(&format!("{base_url}/SHA256SUMS.txt"), &sums_path)?;

    let sums_content = fs::read_to_string(&sums_path).context("读取 SHA256SUMS.txt 失败")?;
    let expected = parse_sha256_for_asset(&sums_content, &archive)
        .with_context(|| format!("SHA256SUMS.txt 中未找到 {archive} 的校验条目"))?;
    let actual = compute_file_sha256(&archive_path)?;
    if !expected.eq_ignore_ascii_case(&actual) {
        bail!("SHA256 校验失败！expected={expected} actual={actual}");
    }
    println!("SHA256 校验通过 ({actual})");

    let extract_dir = tmp_dir.join("extracted");
    fs::create_dir_all(&extract_dir)?;
    extract_archive(&archive_path, &extract_dir, ext)?;

    let bin_name = if cfg!(windows) { "adbx.exe" } else { "adbx" };
    let new_bin = extract_dir.join(bin_name);
    if !new_bin.is_file() {
        bail!("解压后的安装包中未找到 {bin_name}");
    }

    replace_executable(&current_exe, &new_bin)?;
    println!("已成功更新 adbx: v{CURRENT_VERSION} -> {tag} ({})", current_exe.display());
    Ok(0)
}

fn normalize_tag(raw: &str) -> String {
    let t = raw.trim();
    if t.starts_with('v') {
        t.to_string()
    } else {
        format!("v{t}")
    }
}

fn current_target_triple() -> Result<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok(("aarch64-apple-darwin", "tar.gz")),
        ("macos", "x86_64") => Ok(("x86_64-apple-darwin", "tar.gz")),
        ("linux", "x86_64") => Ok(("x86_64-unknown-linux-musl", "tar.gz")),
        ("linux", "aarch64") => Ok(("aarch64-unknown-linux-gnu", "tar.gz")),
        ("windows", "x86_64") => Ok(("x86_64-pc-windows-msvc", "zip")),
        (os, arch) => bail!("暂不支持当前平台自动更新: {os}/{arch}，请使用 cargo install 或手动下载"),
    }
}

fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let clean = v.trim().trim_start_matches('v');
    let core = clean.split('-').next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    Some((major, minor, patch))
}

fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_semver(current), parse_semver(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => current.trim().trim_start_matches('v') != latest.trim().trim_start_matches('v'),
    }
}

fn detect_package_manager_hint(exe_path: &Path) -> Option<&'static str> {
    match classify_install_source(exe_path) {
        InstallSource::Homebrew => Some("brew upgrade adbx"),
        InstallSource::Scoop => Some("scoop update adbx"),
        InstallSource::Chocolatey => Some("choco upgrade adbx"),
        InstallSource::Winget => Some("winget upgrade qytech.adbx"),
        InstallSource::SystemPkg => Some("系统包管理器（apt / pacman）升级，或 `adbx update --force`"),
        InstallSource::Cargo | InstallSource::DirectBinary => None,
    }
}

fn parse_sha256_for_asset(sums_content: &str, filename: &str) -> Option<String> {
    for line in sums_content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        if name == filename {
            return Some(hash.to_string());
        }
    }
    None
}

fn extract_tag_from_release_url(url: &str) -> Option<String> {
    let (_, tail) = url.trim().split_once("/releases/tag/")?;
    let tag = tail.split(['/', '?', '#']).next()?.trim();
    if tag.is_empty() {
        None
    } else {
        Some(tag.to_string())
    }
}

fn fetch_latest_tag(repo: &str) -> Result<String> {
    if let Ok(tag) = fetch_latest_tag_via_redirect(repo) {
        return Ok(tag);
    }
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let body = fetch_text(&url).context("请求 GitHub Releases API 失败")?;
    let parsed: Value = serde_json::from_str(&body).context("解析 GitHub Release 响应失败")?;
    let tag = parsed
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .context("未在 GitHub Release 响应中找到 tag_name")?;
    Ok(tag)
}

#[cfg(unix)]
fn fetch_latest_tag_via_redirect(repo: &str) -> Result<String> {
    let url = format!("https://github.com/{repo}/releases/latest");
    let out = Command::new("curl")
        .args(["-fsSL", "-o", "/dev/null", "-w", "%{url_effective}", &url])
        .output()
        .context("执行 curl 查询最新版本失败")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let effective = String::from_utf8_lossy(&out.stdout);
    extract_tag_from_release_url(&effective).context("未能从跳转链接解析版本号")
}

#[cfg(windows)]
fn fetch_latest_tag_via_redirect(repo: &str) -> Result<String> {
    let url = format!("https://github.com/{repo}/releases/latest");
    let ps = format!(
        "$r = Invoke-WebRequest -UseBasicParsing -Uri '{}'; $u = $r.BaseResponse.ResponseUri.AbsoluteUri; if (-not $u) {{ $u = $r.BaseResponse.RequestMessage.RequestUri.AbsoluteUri }}; Write-Output $u",
        url.replace('\'', "''")
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
        .output()
        .context("调用 PowerShell 查询最新版本失败")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let effective = String::from_utf8_lossy(&out.stdout);
    extract_tag_from_release_url(&effective).context("未能从跳转链接解析版本号")
}

#[cfg(unix)]
fn fetch_text(url: &str) -> Result<String> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: adbx-updater",
            url,
        ])
        .output()
        .context("执行 curl 失败")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(windows)]
fn fetch_text(url: &str) -> Result<String> {
    let ps = format!(
        "(Invoke-WebRequest -UseBasicParsing -Headers @{{'Accept'='application/vnd.github+json';'User-Agent'='adbx-updater'}} -Uri '{}').Content",
        url.replace('\'', "''")
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
        .output()
        .context("调用 PowerShell 失败")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(unix)]
fn download_file(url: &str, dest: &Path) -> Result<()> {
    let status = Command::new("curl")
        .args(["-fSL", url, "-o"])
        .arg(dest)
        .status()
        .context("执行 curl 下载失败")?;
    if !status.success() {
        bail!("下载失败: {url}");
    }
    Ok(())
}

#[cfg(windows)]
fn download_file(url: &str, dest: &Path) -> Result<()> {
    let ps = format!(
        "Invoke-WebRequest -Uri '{}' -OutFile '{}'",
        url.replace('\'', "''"),
        dest.display().to_string().replace('\'', "''")
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
        .status()
        .context("调用 PowerShell 下载失败")?;
    if !status.success() {
        bail!("下载失败: {url}");
    }
    Ok(())
}

#[cfg(unix)]
fn compute_file_sha256(path: &Path) -> Result<String> {
    if let Ok(out) = Command::new("shasum").args(["-a", "256"]).arg(path).output() {
        if out.status.success() {
            if let Some(hash) = String::from_utf8_lossy(&out.stdout).split_whitespace().next() {
                return Ok(hash.to_string());
            }
        }
    }
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .context("未找到 shasum 或 sha256sum，无法校验安装包")?;
    if !out.status.success() {
        bail!("计算 SHA256 失败");
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(|s| s.to_string())
        .context("解析 sha256sum 输出失败")
}

#[cfg(windows)]
fn compute_file_sha256(path: &Path) -> Result<String> {
    let ps = format!(
        "(Get-FileHash -Path '{}' -Algorithm SHA256).Hash",
        path.display().to_string().replace('\'', "''")
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
        .output()
        .context("调用 PowerShell 计算 SHA256 失败")?;
    if !out.status.success() {
        bail!("计算 SHA256 失败");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_ascii_lowercase())
}

#[cfg(unix)]
fn extract_archive(archive: &Path, dest_dir: &Path, _ext: &str) -> Result<()> {
    let status = Command::new("tar")
        .args(["-xzf"])
        .arg(archive)
        .args(["-C"])
        .arg(dest_dir)
        .status()
        .context("执行 tar 解压失败")?;
    if !status.success() {
        bail!("解压更新包失败");
    }
    Ok(())
}

#[cfg(windows)]
fn extract_archive(archive: &Path, dest_dir: &Path, _ext: &str) -> Result<()> {
    let ps = format!(
        "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
        archive.display().to_string().replace('\'', "''"),
        dest_dir.display().to_string().replace('\'', "''")
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
        .status()
        .context("执行 Expand-Archive 解压失败")?;
    if !status.success() {
        bail!("解压更新包失败");
    }
    Ok(())
}

#[cfg(unix)]
fn replace_executable(current_exe: &Path, new_bin: &Path) -> Result<()> {
    let parent = current_exe
        .parent()
        .context("无效的可执行文件路径")?;
    let staged = parent.join(format!(".adbx.update.tmp.{}", std::process::id()));
    fs::copy(new_bin, &staged)
        .with_context(|| format!("写入临时文件 {} 失败（可能需要 sudo 权限）", staged.display()))?;
    let mut perms = fs::metadata(&staged)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&staged, perms)?;
    if let Err(e) = fs::rename(&staged, current_exe) {
        let _ = fs::remove_file(&staged);
        return Err(e).with_context(|| format!("替换 {} 失败", current_exe.display()));
    }
    Ok(())
}

#[cfg(windows)]
fn replace_executable(current_exe: &Path, new_bin: &Path) -> Result<()> {
    let old_exe = current_exe.with_extension("exe.old");
    let _ = fs::remove_file(&old_exe);
    fs::rename(current_exe, &old_exe)
        .with_context(|| format!("无法重命名运行中的 {}（请检查文件权限）", current_exe.display()))?;
    if let Err(e) = fs::copy(new_bin, current_exe) {
        let _ = fs::rename(&old_exe, current_exe);
        return Err(e).with_context(|| format!("写入新版 {} 失败", current_exe.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_shim_and_payload_share_one_uninstall_action() {
        let tmp = tempfile::tempdir().unwrap();
        for (manager, shim, payload) in [
            ("scoop", "shims/adbx.exe", "apps/adbx/current/adbx.exe"),
            ("chocolatey", "bin/adbx.exe", "lib/adbx/tools/adbx.exe"),
        ] {
            let dir = tmp.path().join(manager);
            let paths = [dir.join(shim), dir.join(payload)];
            for p in &paths {
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(p, b"fixture").unwrap();
            }
            assert_eq!(collect_installations(paths.to_vec()).len(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn uninstalling_a_direct_link_preserves_its_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("shared-tool");
        fs::write(&target, b"fixture").unwrap();
        let link = tmp.path().join("adbx");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let installs = collect_installations(vec![link.clone()]);
        assert!(!uninstall_one(&installs[0], None).unwrap());
        assert!(!link.exists());
        assert_eq!(fs::read(&target).unwrap(), b"fixture");
    }

    #[test]
    fn purge_removes_owned_paths_in_a_shared_directory() {
        let root = std::env::temp_dir().join(format!("adbx-purge-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let _cleanup = TempDirGuard(root.clone());
        let tools = root.join("platform-tools");
        fs::create_dir(&tools).unwrap();
        fs::write(
            tools.join(if cfg!(windows) { "adb.exe" } else { "adb" }),
            "sdk",
        )
        .unwrap();
        fs::write(root.join("devices.toml"), "").unwrap();
        fs::write(root.join("completion_cache.json"), "{}").unwrap();
        fs::write(root.join("other-app.txt"), "keep").unwrap();
        fs::create_dir(root.join("history.jsonl")).unwrap();

        let discover = || purge_items_in(Some(&root), Some(&root), Some(&root), None);
        for item in discover() {
            item.remove().unwrap();
        }
        assert!(!root.join("devices.toml").exists());
        assert!(!root.join("completion_cache.json").exists());
        assert!(root.join("history.jsonl").is_dir());
        assert!(tools.is_dir(), "用户自备的 SDK 应保留");

        fs::write(tools.join(crate::adb::MANAGED_TOOLS_MARKER), b"adbx\n").unwrap();
        for item in discover() {
            item.remove().unwrap();
        }
        assert!(!tools.exists());
        assert_eq!(
            fs::read_to_string(root.join("other-app.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn compares_semver_versions_correctly() {
        assert!(is_newer_version("0.0.1", "v0.0.2"));
        assert!(is_newer_version("0.1.9", "v0.2.0"));
        assert!(!is_newer_version("0.0.2", "v0.0.2"));
        assert!(!is_newer_version("0.1.0", "v0.0.9"));
    }

    #[test]
    fn parses_sha256sums_lines() {
        let sums = "deadbeef  adbx-v0.0.1-aarch64-apple-darwin.tar.gz\ncafebabe *adbx-v0.0.1-x86_64-pc-windows-msvc.zip\n";
        assert_eq!(
            parse_sha256_for_asset(sums, "adbx-v0.0.1-aarch64-apple-darwin.tar.gz").as_deref(),
            Some("deadbeef")
        );
        assert_eq!(
            parse_sha256_for_asset(sums, "adbx-v0.0.1-x86_64-pc-windows-msvc.zip").as_deref(),
            Some("cafebabe")
        );
        assert_eq!(parse_sha256_for_asset(sums, "missing.tar.gz"), None);
    }

    #[test]
    fn detects_package_manager_install_paths() {
        assert_eq!(
            detect_package_manager_hint(Path::new("/opt/homebrew/Cellar/adbx/0.0.1/bin/adbx")),
            Some("brew upgrade adbx")
        );
        assert_eq!(
            detect_package_manager_hint(Path::new(r"C:\Users\dev\scoop\apps\adbx\0.0.1\adbx.exe")),
            Some("scoop update adbx")
        );
        assert_eq!(
            detect_package_manager_hint(Path::new(
                r"C:\ProgramData\chocolatey\lib\adbx\tools\adbx.exe"
            )),
            Some("choco upgrade adbx")
        );
        assert_eq!(
            detect_package_manager_hint(Path::new(
                r"C:\Users\dev\AppData\Local\Microsoft\WinGet\Packages\qytech.adbx_Microsoft.Winget.Source_8wekyb3d8bbwe\adbx.exe"
            )),
            Some("winget upgrade qytech.adbx")
        );
        assert_eq!(
            detect_package_manager_hint(Path::new("/Users/dev/.local/bin/adbx")),
            None
        );
    }

    #[test]
    fn extracts_tag_from_latest_redirect_url() {
        assert_eq!(
            extract_tag_from_release_url("https://github.com/qytech/adbx-rs/releases/tag/v0.0.1")
                .as_deref(),
            Some("v0.0.1")
        );
        assert_eq!(
            extract_tag_from_release_url("https://github.com/qytech/adbx-rs/releases"),
            None
        );
    }

    #[test]
    fn distinguishes_self_uninstall_vs_adb_apk_uninstall() {
        let s = |vals: &[&str]| vals.iter().map(|v| (*v).to_string()).collect::<Vec<_>>();
        assert!(!is_adb_package_uninstall_args(&s(&[])));
        assert!(!is_adb_package_uninstall_args(&s(&["--self"])));
        assert!(!is_adb_package_uninstall_args(&s(&["--self", "--dry-run"])));
        assert!(!is_adb_package_uninstall_args(&s(&["--purge", "-y"])));
        assert!(!is_adb_package_uninstall_args(&s(&["--help"])));

        assert!(is_adb_package_uninstall_args(&s(&["com.example.app"])));
        assert!(is_adb_package_uninstall_args(&s(&["-k", "com.example.app"])));
        assert!(is_adb_package_uninstall_args(&s(&["--user", "0", "com.example.app"])));
    }

    #[test]
    fn classifies_install_sources_correctly() {
        assert_eq!(
            classify_install_source(Path::new("/opt/homebrew/Cellar/adbx/0.0.2/bin/adbx")),
            InstallSource::Homebrew
        );
        assert_eq!(
            classify_install_source(Path::new(r"C:\Users\dev\scoop\shims\adbx.exe")),
            InstallSource::Scoop
        );
        assert_eq!(
            classify_install_source(Path::new(r"C:\ProgramData\chocolatey\bin\adbx.exe")),
            InstallSource::Chocolatey
        );
        assert_eq!(
            classify_install_source(Path::new("/Users/dev/.cargo/bin/adbx")),
            InstallSource::Cargo
        );
        assert_eq!(
            classify_install_source(Path::new("/Users/dev/.local/bin/adbx")),
            InstallSource::DirectBinary
        );
    }
}
