use crate::selector;
use crate::store::{self, Store};
use anyhow::{bail, Context, Result};
use dialoguer::Confirm;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn show_paths(store: &Store) -> Result<u8> {
    let cache_dir = match std::env::var("ADBX_CACHE_DIR") {
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v.trim())),
        _ => dirs::cache_dir().map(|d| d.join("adbx")),
    };
    let cache_file = cache_dir
        .map(|d| d.join("completion_cache.json"))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "-".to_string());

    println!("配置目录:       {}", store.config_dir().display());
    println!("设备配置文件:   {}", store.devices_config_path().display());
    println!("会话状态文件:   {}", store.sessions_path().display());
    println!("命令历史文件:   {}", store.history_path().display());
    println!("补全缓存文件:   {}", cache_file);
    Ok(0)
}

pub fn show_config(store: &Store) -> Result<u8> {
    let path = store.devices_config_path();
    if !path.exists() {
        println!("(配置文件尚未生成: {}，可通过 `adbx alias` / `adbx group` / `adbx macro` 自动创建)", path.display());
        return Ok(0);
    }
    let raw = fs::read_to_string(&path)?;
    print!("{raw}");
    if !raw.ends_with('\n') {
        println!();
    }
    Ok(0)
}

pub fn edit_config(store: &Store) -> Result<u8> {
    let path = store.devices_config_path();
    if !path.exists() {
        store::atomic_write(&path, "# adbx devices.toml\n")?;
    }

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|s| !s.trim().is_empty());

    let status = if let Some(ed) = editor {
        let Some((prog, args)) = parse_editor_command(&ed) else {
            bail!("无效的编辑器命令: {ed}");
        };
        Command::new(prog).args(args).arg(&path).status()
    } else if cfg!(windows) {
        Command::new("notepad").arg(&path).status()
    } else {
        Command::new("vi").arg(&path).status()
    }
    .with_context(|| format!("无法打开编辑器编辑 {}", path.display()))?;

    // 编辑结束后自动校验 TOML 语法，避免写坏配置
    if let Ok(raw) = fs::read_to_string(&path) {
        if let Err(e) = Store::validate_devices_toml(&raw) {
            eprintln!("警告：编辑后的配置文件存在语法错误: {e:#}");
            return Ok(1);
        }
    }

    Ok(status.code().unwrap_or(0) as u8)
}

pub fn export_config(store: &Store, out_file: &str) -> Result<u8> {
    let path = store.devices_config_path();
    let raw = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        "# adbx devices.toml\n".to_string()
    };
    store::atomic_write(Path::new(out_file), &raw)?;
    println!("已导出配置到: {out_file}");
    Ok(0)
}

pub fn import_config(store: &Store, in_file: &str) -> Result<u8> {
    let raw = fs::read_to_string(in_file)
        .with_context(|| format!("读取待导入配置文件失败: {in_file}"))?;
    Store::validate_devices_toml(&raw)?;
    store::atomic_write(&store.devices_config_path(), &raw)?;
    println!("校验通过并已导入配置到: {}", store.devices_config_path().display());
    Ok(0)
}

pub fn reset_config(store: &Store, assume_yes: bool) -> Result<u8> {
    if !assume_yes {
        if !selector::is_interactive_tty() {
            bail!("非交互终端重置配置需显式传入 `-y` / `--yes`");
        }
        let ok = Confirm::new()
            .with_prompt("确认重置 devices.toml 与 sessions.json（所有别名/分组/宏/端口预设将被清空）？")
            .default(false)
            .interact()?;
        if !ok {
            println!("已取消重置。");
            return Ok(0);
        }
    }
    let _ = fs::remove_file(store.devices_config_path());
    let _ = fs::remove_file(store.sessions_path());
    println!("已重置 adbx 配置文件与会话状态。");
    Ok(0)
}

fn parse_editor_command(raw: &str) -> Option<(String, Vec<String>)> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;

    for ch in raw.trim().chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => cur.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    parts.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }

    let mut it = parts.into_iter();
    let prog = it.next()?;
    Some((prog, it.collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_editor_with_arguments() {
        assert_eq!(
            parse_editor_command("code --wait"),
            Some(("code".into(), vec!["--wait".into()]))
        );
        assert_eq!(
            parse_editor_command("\"/Applications/Sublime Text\" -w"),
            Some(("/Applications/Sublime Text".into(), vec!["-w".into()]))
        );
        assert_eq!(parse_editor_command("   "), None);
    }
}
