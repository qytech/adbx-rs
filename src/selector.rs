use crate::adb::DeviceInfo;
use crate::store::Store;
use anyhow::{bail, Result};
use dialoguer::{Confirm, FuzzySelect, MultiSelect};
use std::io::IsTerminal;

pub enum PickOutcome {
    Single(DeviceInfo),
    AllOnline,
}

pub fn is_interactive_tty() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub fn pick_device(devices: &[DeviceInfo], store: &Store) -> Result<DeviceInfo> {
    if devices.is_empty() {
        bail!("当前没有可选择的 ADB 设备");
    }

    if !is_interactive_tty() {
        let hints: Vec<String> = devices
            .iter()
            .map(|d| format!("@{} ({} / {})", store.display_label(&d.serial), d.serial, d.model))
            .collect();
        bail!(
            "检测到非交互终端且有 {} 台在线设备，已阻止弹窗挂起。请显式指定目标设备（例如 `adbx --device <别名> ...`、`adbx --all ...` 或设置 ADBX_DEVICE）。在线设备: {}",
            devices.len(),
            hints.join(", ")
        );
    }

    let items: Vec<String> = devices
        .iter()
        .map(|d| {
            let alias = store.alias_for(&d.serial).unwrap_or("-");
            format!("{:<14} | {:<22} | {:<8} | {}", alias, d.serial, d.state, d.model)
        })
        .collect();

    let idx = FuzzySelect::new()
        .with_prompt("检测到多台设备，请选择目标设备（支持输入关键词模糊过滤）")
        .items(&items)
        .default(0)
        .interact()?;

    Ok(devices[idx].clone())
}

/// 普通模式执行带参数的非交互命令（如 `adbx shell <cmd>`）时，首项提供「全部设备并行执行」
pub fn pick_device_or_all(devices: &[DeviceInfo], store: &Store) -> Result<PickOutcome> {
    if devices.is_empty() {
        bail!("当前没有可选择的 ADB 设备");
    }

    if !is_interactive_tty() {
        let hints: Vec<String> = devices
            .iter()
            .map(|d| format!("@{} ({} / {})", store.display_label(&d.serial), d.serial, d.model))
            .collect();
        bail!(
            "检测到非交互终端且有 {} 台在线设备，请使用 `adbx --all ...` 或 `adbx --device <别名> ...`。在线设备: {}",
            devices.len(),
            hints.join(", ")
        );
    }

    let mut items = Vec::with_capacity(devices.len() + 1);
    items.push(format!(
        "[ALL]          | 全部 {} 台在线设备并行执行",
        devices.len()
    ));
    for d in devices {
        let alias = store.alias_for(&d.serial).unwrap_or("-");
        items.push(format!(
            "{:<14} | {:<22} | {:<8} | {}",
            alias, d.serial, d.state, d.model
        ));
    }

    let idx = FuzzySelect::new()
        .with_prompt("检测到多台设备：直接回车可全部并行执行，或选择单台设备")
        .items(&items)
        .default(0)
        .interact()?;

    if idx == 0 {
        Ok(PickOutcome::AllOnline)
    } else {
        Ok(PickOutcome::Single(devices[idx - 1].clone()))
    }
}

pub fn pick_multiple_devices(devices: &[DeviceInfo], store: &Store) -> Result<Vec<DeviceInfo>> {
    if devices.is_empty() {
        bail!("当前没有在线的可选 ADB 设备");
    }

    if !is_interactive_tty() {
        bail!("非交互终端不支持 `--pick` 交互勾选，请改用 `--all` 或 `--device 别名1,别名2`");
    }

    let items: Vec<String> = devices
        .iter()
        .map(|d| {
            let alias = store.alias_for(&d.serial).unwrap_or("-");
            format!("{:<14} | {:<22} | {}", alias, d.serial, d.model)
        })
        .collect();

    let defaults = vec![true; items.len()];
    let indices = MultiSelect::new()
        .with_prompt("请勾选要批量执行的目标设备（空格勾选/取消，回车确认）")
        .items(&items)
        .defaults(&defaults)
        .interact()?;

    if indices.is_empty() {
        bail!("未勾选任何设备，已取消执行");
    }

    Ok(indices.into_iter().map(|i| devices[i].clone()).collect())
}

pub fn confirm_dangerous(label: &str, serial: &str, cmd_preview: &str) -> Result<bool> {
    if !is_interactive_tty() {
        bail!("非交互终端拦截了高危命令 `adb {cmd_preview}`（目标: {label} / {serial}）");
    }
    let prompt = format!(
        "高危操作确认 -> 目标设备 [{label} / {serial}]，命令: adb {cmd_preview}，确认执行？"
    );
    Ok(Confirm::new().with_prompt(prompt).default(false).interact()?)
}

pub fn confirm_batch_dangerous(targets: &[(String, String)], cmd_preview: &str) -> Result<bool> {
    if !is_interactive_tty() {
        bail!("非交互终端拦截了批量高危命令 `adb {cmd_preview}`；如确认自动化执行，请显式传入 `-y` / `--yes`");
    }
    eprintln!("即将对以下 {} 台设备执行高危命令: adb {cmd_preview}", targets.len());
    for (label, serial) in targets {
        eprintln!("  - {label} ({serial})");
    }
    Ok(Confirm::new()
        .with_prompt("确认批量执行以上高危操作？")
        .default(false)
        .interact()?)
}
