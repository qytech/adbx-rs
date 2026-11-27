use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_REPO: &str = "Jax-Lifu/adbx-rs";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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
    let s = exe_path.to_string_lossy().to_ascii_lowercase().replace('\\', "/");
    if s.contains("/cellar/adbx/") || s.contains("/homebrew/") || s.contains("/linuxbrew/") {
        Some("brew upgrade adbx")
    } else if s.contains("/scoop/apps/adbx/") {
        Some("scoop update adbx")
    } else if s.contains("/chocolatey/lib/adbx/") || s.contains("/chocolatey/bin/") {
        Some("choco upgrade adbx")
    } else if s.contains("/winget/packages/") || s.contains("/microsoft/winget/links/") {
        Some("winget upgrade Jax-Lifu.adbx")
    } else if s == "/usr/bin/adbx" {
        Some("系统包管理器（apt / pacman）升级，或 `adbx update --force`")
    } else {
        None
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
                r"C:\Users\dev\AppData\Local\Microsoft\WinGet\Packages\Jax-Lifu.adbx_Microsoft.Winget.Source_8wekyb3d8bbwe\adbx.exe"
            )),
            Some("winget upgrade Jax-Lifu.adbx")
        );
        assert_eq!(
            detect_package_manager_hint(Path::new("/Users/dev/.local/bin/adbx")),
            None
        );
    }

    #[test]
    fn extracts_tag_from_latest_redirect_url() {
        assert_eq!(
            extract_tag_from_release_url("https://github.com/Jax-Lifu/adbx-rs/releases/tag/v0.0.1")
                .as_deref(),
            Some("v0.0.1")
        );
        assert_eq!(
            extract_tag_from_release_url("https://github.com/Jax-Lifu/adbx-rs/releases"),
            None
        );
    }
}
