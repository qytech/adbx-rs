use crate::selector;
use crate::store::{self, Store};
use anyhow::{bail, Context, Result};
use dialoguer::Confirm;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn show_paths(dir: &Path) -> Result<u8> {
    let cache_dir = match std::env::var("ADBX_CACHE_DIR") {
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v.trim())),
        _ => dirs::cache_dir().map(|d| d.join("adbx")),
    };
    let cache_file = cache_dir
        .map(|d| d.join("completion_cache.json"))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "-".to_string());

    println!("配置目录:       {}", dir.display());
    println!("设备配置文件:   {}", dir.join("devices.toml").display());
    println!("会话状态文件:   {}", dir.join("sessions.json").display());
    println!("命令历史文件:   {}", dir.join("history.jsonl").display());
    println!("补全缓存文件:   {}", cache_file);
    Ok(0)
}

pub fn show_config(dir: &Path) -> Result<u8> {
    let path = dir.join("devices.toml");
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

pub fn edit_config(dir: &Path) -> Result<u8> {
    let path = dir.join("devices.toml");
    let original = store::read_optional(&path)?;
    let draft = tempfile::Builder::new()
        .prefix("adbx-edit-")
        .suffix(".toml")
        .tempfile_in(dir)?
        .into_temp_path();
    let initial = original.as_deref().unwrap_or("# adbx devices.toml\n");
    fs::write(&draft, initial)?;

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|s| !s.trim().is_empty());

    let status = if let Some(ed) = editor {
        let Some((prog, args)) = parse_editor_command(&ed) else {
            bail!("无效的编辑器命令: {ed}");
        };
        Command::new(prog).args(args).arg(&draft).status()
    } else if cfg!(windows) {
        Command::new("notepad").arg(&draft).status()
    } else {
        Command::new("vi").arg(&draft).status()
    }
    .with_context(|| format!("无法打开编辑器编辑 {}", draft.display()))?;

    let edited = fs::read_to_string(&draft)?;
    if !status.success() {
        if edited != initial {
            let saved = draft.keep()?;
            bail!("编辑器退出失败，草稿保存在 {}", saved.display());
        }
        return Ok(status.code().unwrap_or(1) as u8);
    }
    if let Err(err) = Store::validate_devices_toml(&edited) {
        let saved = draft.keep()?;
        bail!("{err:#}；草稿保存在 {}", saved.display());
    }
    if edited == initial && original.is_some() {
        return Ok(0);
    }
    let committed = store::with_store_lock(dir, || {
        let latest = store::read_optional(&path)?;
        if latest.as_deref() == Some(&edited) {
            return Ok(true);
        }
        if latest != original {
            return Ok(edited == initial);
        }
        store::atomic_write(&path, &edited)?;
        Ok(true)
    });
    if !matches!(committed, Ok(true)) {
        let saved = draft.keep()?;
        committed.with_context(|| format!("提交配置失败，草稿保存在 {}", saved.display()))?;
        bail!(
            "编辑期间配置已被其他进程修改，已保留最新配置；你的草稿保存在 {}",
            saved.display()
        );
    }
    Ok(0)
}

pub fn export_config(dir: &Path, out_file: &str) -> Result<u8> {
    let path = dir.join("devices.toml");
    let raw = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        "# adbx devices.toml\n".to_string()
    };
    store::atomic_write(Path::new(out_file), &raw)?;
    println!("已导出配置到: {out_file}");
    Ok(0)
}

pub fn import_config(dir: &Path, in_file: &str) -> Result<u8> {
    let raw = fs::read_to_string(in_file)
        .with_context(|| format!("读取待导入配置文件失败: {in_file}"))?;
    Store::validate_devices_toml(&raw)?;
    store::with_store_lock(dir, || store::atomic_write(&dir.join("devices.toml"), &raw))?;
    println!("校验通过并已导入配置到: {}", dir.join("devices.toml").display());
    Ok(0)
}

pub fn reset_config(dir: &Path, assume_yes: bool) -> Result<u8> {
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
    store::with_store_lock(dir, || {
        for name in ["devices.toml", "sessions.json"] {
            if let Err(err) = fs::remove_file(dir.join(name)) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    return Err(err).with_context(|| format!("重置 {name} 失败"));
                }
            }
        }
        Ok(())
    })?;
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
