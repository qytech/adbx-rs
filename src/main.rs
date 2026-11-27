mod adb;
mod broadcast;
mod cache;
mod completion;
mod config_cmd;
mod highlight;
mod history;
mod logcat;
mod logcat_snapshot;
mod mcp;
mod palette;
mod port;
mod pull;
mod reboot;
mod repl;
mod selector;
mod session;
mod store;
mod update;
mod watch;
mod wifi;

use anyhow::{bail, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{engine::ArgValueCompleter, env::CompleteEnv, generate, Shell};
use std::io;
use store::DeviceTarget;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = "adbx",
    version,
    about = "增强型 ADB（macOS & Windows）：防串机、设备分组、多机并行、断线重连 Logcat、无线调试与安全加固 MCP Server",
    subcommand_negates_reqs = true,
    allow_external_subcommands = true
)]
#[allow(dead_code)]
struct Cli {
    /// 对全部在线设备并行执行后续命令
    #[arg(long)]
    all: bool,
    /// 交互式多选设备并行执行后续命令
    #[arg(long)]
    pick: bool,
    /// 指定目标设备别名、序列号或逗号分隔列表（例如 `pixel` 或 `a,b`）
    #[arg(long, value_name = "TARGET", add = ArgValueCompleter::new(completion::complete_devices))]
    device: Option<String>,
    /// 指定目标设备分组并行执行
    #[arg(long, value_name = "GROUP", add = ArgValueCompleter::new(completion::complete_devices))]
    group: Option<String>,
    #[arg(long, value_name = "TARGET", hide = true, add = ArgValueCompleter::new(completion::complete_devices))]
    target: Option<String>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// 列出所有已连接设备（兼容 `-l` 原生详细输出，支持 `--json` 供 AI Agent 解析）
    #[command(visible_alias = "list")]
    Devices {
        #[arg(long)]
        json: bool,
        /// 兼容原生 `adb devices -l` 详细输出
        #[arg(short = 'l', long)]
        long: bool,
    },
    /// 为当前终端窗口锁定目标（支持单台别名、分组名、`all` 或逗号多台 `a,b`）
    Use {
        /// 按分组名动态锁定（也可用于分组与设备同名时）
        #[arg(long, conflicts_with = "target")]
        group: Option<String>,
        #[arg(add = ArgValueCompleter::new(completion::complete_devices))]
        target: Option<String>,
    },
    /// 清除当前终端窗口的设备锁定
    Unuse,
    /// 为设备序列号绑定人类可读别名
    Alias {
        #[arg(add = ArgValueCompleter::new(completion::complete_devices))]
        serial: String,
        name: String,
    },
    /// 管理设备分组（如 `adbx group set qa pixel mi`）
    Group {
        #[command(subcommand)]
        action: GroupCmd,
    },
    /// 无线调试管理（mDNS 发现、一键切无线并记忆 IP、批量重连）
    Wifi {
        #[command(subcommand)]
        action: WifiCmd,
    },
    /// 端口转发管理（reverse / forward 列表、预设保存与一键批量同步）
    Port {
        #[command(subcommand)]
        action: PortCmd,
    },
    /// 智能重启：若之前执行过 root/remount，重启后自动等待开机完成、恢复 root & remount 并进入 shell
    #[command(visible_alias = "rr")]
    Reboot {
        /// 重启目标模式（如 bootloader / recovery；省略为正常重启）
        mode: Option<String>,
        /// 最长等待开机秒数（检测到 sys.boot_completed=1 后会提前继续）
        #[arg(short = 'w', long, default_value_t = 60)]
        wait: u64,
        /// 重启并恢复完成后强制自动进入 adb shell
        #[arg(long)]
        shell: bool,
        /// 仅重启，不自动进入 shell
        #[arg(long)]
        no_shell: bool,
        /// 强制在重启后执行 adb root
        #[arg(long)]
        root: bool,
        /// 强制在重启后执行 adb remount（会自动先执行 root）
        #[arg(long)]
        remount: bool,
        /// 忽略此前记忆的 root/remount 状态
        #[arg(long)]
        no_restore: bool,
        /// 跳过重启二次确认
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// 增强装包（自动附加 -r -d -g 允许降级与自动授权，支持多机并行及装完自动拉起）
    #[command(name = "install+")]
    InstallPlus {
        /// APK 文件路径
        apk: String,
        /// 装完后自动启动的应用包名
        #[arg(short, long, add = ArgValueCompleter::new(completion::complete_packages))]
        launch: Option<String>,
        /// 目标设备（支持 all / 分组名 / 别名 / 逗号多台；省略则沿用当前会话）
        #[arg(short, long, add = ArgValueCompleter::new(completion::complete_devices))]
        target: Option<String>,
        /// 单台安装超时秒数
        #[arg(long, default_value_t = 90)]
        timeout: u64,
    },
    /// 输出当前终端锁定的设备（可用于 Zsh / PowerShell 提示符）
    Current {
        #[arg(long)]
        prompt: bool,
    },
    /// 交互式 Logcat TUI（支持断线重连与崩溃跳转），或 `--tail N` 拉取非交互快照
    Log {
        #[arg(short, long, add = ArgValueCompleter::new(completion::complete_packages))]
        pkg: Option<String>,
        #[arg(short, long, default_value = "V")]
        level: String,
        #[arg(short, long)]
        regex: Option<String>,
        #[arg(short = 'x', long)]
        exclude: Option<String>,
        #[arg(long, default_value_t = 100_000)]
        buffer: usize,
        #[arg(long)]
        tail: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// 启动原生 MCP stdio 服务器（默认只读白名单模式）
    Mcp {
        #[arg(long)]
        allow_destructive: bool,
    },
    /// 实时多机监控面板（直连本地 adb-server host:track-devices，实时查看热插拔/电量/温度）
    Watch,
    /// 管理自定义快捷宏（保存于 devices.toml [macros]，自动同步到 `adbx menu`）
    Macro {
        #[command(subcommand)]
        action: MacroCmd,
    },
    /// 配置文件管理（查看路径/内容、编辑、导入导出、重置）
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// 命令历史管理（列表、关键词搜索、模糊重放、导出与清空）
    #[command(visible_alias = "h")]
    History {
        #[command(subcommand)]
        action: Option<HistoryCmd>,
    },
    /// 交互式宿主机 Shell REPL（支持上下键历史、Ctrl+R、cd 跟踪、自动写入 history.jsonl 与多机并行）
    Repl,
    /// 打开模糊搜索命令面板
    Menu,
    /// 直接执行快捷宏（screenshot / top-activity / clear-restart / meminfo / wifi-adb）
    Quick {
        action: String,
    },
    /// 自动下载并安装 Google 官方 Android Platform-Tools（无需手动配置 adb）
    SetupAdb {
        /// 强制重新下载覆盖
        #[arg(long)]
        force: bool,
    },
    /// 检查并自动升级 adbx 到最新 GitHub Release 版本（校验 SHA256）
    #[command(visible_alias = "upgrade")]
    Update {
        /// 仅检查是否有新版本，不执行下载替换
        #[arg(long)]
        check: bool,
        /// 指定要安装的版本号（如 v0.0.2）
        #[arg(long)]
        version: Option<String>,
        /// 强制覆盖当前版本（包括同版本或包管理器目录）
        #[arg(long)]
        force: bool,
    },
    /// 卸载本机已安装的 adbx（需传 `--self` 或使用 `adbx self-uninstall`；卸载手机应用请传包名 `adbx uninstall <包名>`）
    #[command(visible_alias = "self-uninstall")]
    Uninstall {
        /// 确认要卸载宿主机本机安装的 adbx（防止与 `adb uninstall` 混淆）
        #[arg(long = "self")]
        self_uninstall: bool,
        /// 同时清除本地配置、历史记录、缓存与托管的 platform-tools
        #[arg(long)]
        purge: bool,
        /// 仅预览检测到的安装位置与待清理目录，不执行删除
        #[arg(long)]
        dry_run: bool,
        /// 跳过交互确认（非交互终端必须显式传入）
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// 生成静态 Shell 补全脚本
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// 在目标设备执行 shell 命令或进入增强交互 Shell（支持宿主机 Tab 补全远端路径）
    #[command(disable_help_flag = true)]
    Shell {
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            add = ArgValueCompleter::new(completion::complete_shell_args)
        )]
        args: Vec<String>,
    },
    /// 在选定设备执行原生 adb 子命令（使用 `adbx -- <args>` 可直接透传）
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Subcommand, Debug)]
enum GroupCmd {
    /// 列出所有已配置设备分组
    List,
    /// 创建或更新设备分组
    Set {
        name: String,
        #[arg(required = true)]
        members: Vec<String>,
    },
    /// 删除设备分组
    Rm { name: String },
}

#[derive(Subcommand, Debug)]
enum WifiCmd {
    /// 列出已记忆的无线设备与局域网 mDNS 服务
    List,
    /// 将当前 USB 设备切为 5555 无线调试并记忆 IP
    Enable,
    /// 一键批量重连所有已记忆的无线设备
    Reconnect,
}

#[derive(Subcommand, Debug)]
enum PortCmd {
    /// 列出当前设备的 reverse/forward 映射及本地预设
    List,
    /// 添加并保存端口映射（如 `adbx port add reverse 8081 8081`）
    Add {
        /// reverse 或 forward
        direction: String,
        local: String,
        remote: String,
    },
    /// 将已保存的端口预设同步到当前设备（或通过 --target all 同步全部在线设备）
    Sync {
        #[arg(short, long, add = ArgValueCompleter::new(completion::complete_devices))]
        target: Option<String>,
    },
    /// 从本地预设删除指定端口映射（如 `adbx port rm reverse 8081 8081`）
    Rm {
        direction: String,
        local: String,
        remote: String,
    },
    /// 清空当前设备上的全部 reverse/forward 映射
    Clear,
}

#[derive(Subcommand, Debug)]
enum MacroCmd {
    /// 列出所有自定义快捷宏
    List,
    /// 新增或更新自定义快捷宏（如 `adbx macro set net-off shell svc wifi disable`）
    Set {
        name: String,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// 删除自定义快捷宏
    Rm { name: String },
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// 打印所有配置/状态/历史/缓存文件的系统路径
    Path,
    /// 查看当前 devices.toml 内容
    Show,
    /// 用系统编辑器打开 devices.toml（保存退出后自动校验 TOML 语法）
    Edit,
    /// 导出 devices.toml 备份或分享给团队
    Export { file: String },
    /// 校验并导入 devices.toml
    Import { file: String },
    /// 重置配置文件与会话状态
    Reset {
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum HistoryCmd {
    /// 查看最近执行的命令历史
    List {
        #[arg(short = 'n', long, default_value_t = 30)]
        limit: usize,
        #[arg(short, long)]
        device: Option<String>,
        #[arg(short, long)]
        grep: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// 模糊搜索历史命令并回车直接重放
    Replay {
        #[arg(short = 'n', long, default_value_t = 100)]
        limit: usize,
        /// 显式指定完整设备序列号，可重复；默认沿用历史记录中的原设备
        #[arg(long = "serial", value_name = "SERIAL")]
        serials: Vec<String>,
        /// 本地相对路径的工作目录（默认采用历史记录的目录）
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        /// 重放导出的命令，须与 --serial 一起使用
        #[arg(last = true, value_name = "ADB_ARGS")]
        args: Vec<String>,
    },
    /// 导出历史记录（支持 .jsonl / .json / .sh 脚本）
    Export { file: String },
    /// 清空全部命令历史
    Clear {
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

fn main() -> ExitCode {
    CompleteEnv::with_factory(Cli::command).complete();

    match run() {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("adbx error: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn run() -> Result<u8> {
    let raw_args: Vec<String> = std::env::args().collect();

    // 原生逃生舱：`adbx -- <任意原生参数>` 跳过全部增强逻辑，100% 原样转发给本机 adb
    if raw_args.len() >= 2 && raw_args[1] == "--" {
        let adb_path = adb::find_or_prompt_adb()?;
        return adb::run_passthrough(&adb_path, None, &raw_args[2..]);
    }

    // 原生 adb 全局参数直通
    if raw_args.len() >= 2
        && matches!(
            raw_args[1].as_str(),
            "-a" | "-s" | "-d" | "-e" | "-t" | "-H" | "-P" | "-L" | "--one-device"
        )
    {
        let adb_path = adb::find_or_prompt_adb()?;
        return adb::run_passthrough(&adb_path, None, &raw_args[1..]);
    }

    if raw_args.len() == 2 && raw_args[1] == "?" {
        return run_menu(None);
    }

    // 若用户输入 `adbx uninstall <包名>` 或带 `-k`/`--user`，按原生 `adb uninstall` 处理手机应用卸载
    if raw_args.len() >= 2
        && raw_args[1] == "uninstall"
        && update::is_adb_package_uninstall_args(&raw_args[2..])
    {
        return exec_with_target(None, raw_args[1..].to_vec());
    }

    if let Some((target_spec, rest)) = extract_leading_target(&raw_args)? {
        if rest.is_empty() {
            bail!("指定目标设备或分组时需要在后面跟上要执行的命令，例如: adbx --all shell getprop ro.product.model");
        }

        let opts = broadcast::parse_broadcast_args(&rest)?;
        if matches!(opts.adb_args[0].as_str(), "devices" | "list") {
            // 设备清单是宿主机查询，即使有目标前缀也只执行一次。
            let mut args = vec![raw_args[0].clone()];
            args.extend(opts.adb_args);
            if opts.json_output && !args.iter().any(|a| a == "--json") {
                args.push("--json".into());
            }
            return run_cli(&args);
        }

        let cfg_store = store::Store::load()?;
        let selector_flag = raw_args[1].split('=').next().unwrap_or_default();
        if selector_flag == "--group" && cfg_store.group_members(&target_spec).is_none() {
            bail!("未定义设备分组 '{target_spec}'，请用 adbx group list 查看分组");
        }
        if !matches!(rest[0].as_str(), "repl" | "reboot" | "rr" | "install+" | "log" | "menu" | "?" | "uninstall") {
            validate_native_command(&opts.adb_args[0])?;
        }
        let adb_path = adb::find_or_prompt_adb()?;
        let target = match selector_flag {
            "--group" => DeviceTarget::Group(target_spec),
            "--all" => DeviceTarget::All,
            _ => {
                let online: Vec<_> = adb::list_devices(&adb_path)?.into_iter().filter(|d| d.state == "device").collect();
                if selector_flag == "--device" {
                    cfg_store.select_devices(&target_spec, &online)?
                } else if target_spec == "pick" {
                    DeviceTarget::Fixed(selector::pick_multiple_devices(&online, &cfg_store)?.into_iter().map(|d| d.serial).collect())
                } else if target_spec.contains(',') {
                    DeviceTarget::Fixed(resolve_target_devices(&target_spec, &online, &cfg_store)?.into_iter().map(|d| d.serial).collect())
                } else {
                    cfg_store.select_target(&target_spec, &online)?
                }
            }
        };
        if rest[0] == "repl" {
            return repl::run_repl(&adb_path, target, &cfg_store);
        }
        if rest[0] == "reboot" || rest[0] == "rr" {
            let mut synth = vec![raw_args[0].clone()];
            synth.extend(rest.iter().cloned());
            let cli = Cli::try_parse_from(synth)?;
            if let Some(Commands::Reboot {
                mode,
                wait,
                shell,
                no_shell,
                root,
                remount,
                no_restore,
                yes,
            }) = cli.command
            {
                let serials = serials_for_target(&adb_path, &cfg_store, &target)?;
                if let [serial] = serials.as_slice() {
                    return dispatch_reboot(&adb_path, &cfg_store, serial, mode, wait, shell, no_shell,
                        root, remount, no_restore, yes);
                }
                if shell && !no_shell { bail!("多机重启不能同时进入交互 Shell，请使用 --no-shell 或选择单台设备"); }
                let labels: Vec<_> = serials.iter().map(|s| (cfg_store.display_label(s), s.clone())).collect();
                let preview = format!("reboot {}", mode.as_deref().unwrap_or("")).trim_end().to_owned();
                if !yes && !selector::confirm_batch_dangerous(&labels, &preview)? { return Ok(0); }
                let mut failed = false;
                for batch in serials.chunks(8) {
                    std::thread::scope(|scope| {
                        let jobs: Vec<_> = batch.iter().map(|serial| (serial, scope.spawn(|| {
                            dispatch_reboot(&adb_path, &cfg_store, serial, mode.clone(), wait, false, true,
                                root, remount, no_restore, true)
                        }))).collect();
                        for (serial, job) in jobs {
                            match job.join().expect("reboot worker") {
                                Ok(0) => {},
                                Ok(code) => { eprintln!("{serial}: 重启失败 (exit={code})"); failed = true; }
                                Err(err) => { eprintln!("{serial}: {err:#}"); failed = true; }
                            }
                        }
                    });
                }
                return Ok(u8::from(failed));
            }
        }
        if rest[0] == "install+" {
            let mut synth = vec![raw_args[0].clone()];
            synth.extend(rest.iter().cloned());
            let cli = Cli::try_parse_from(synth)?;
            if let Some(Commands::InstallPlus { apk, launch, timeout, .. }) = cli.command {
                let serials = serials_for_target(&adb_path, &cfg_store, &target)?;
                return install_on_serials(&adb_path, &cfg_store, &serials, apk, launch, timeout);
            }
        }

        if rest[0] == "log" {
            let mut synth = vec![raw_args[0].clone()];
            synth.extend(rest.iter().cloned());
            let cli = Cli::try_parse_from(synth)?;
            if let Some(Commands::Log {
                pkg,
                level,
                regex,
                exclude,
                buffer,
                tail,
                json,
            }) = cli.command
            {
                return run_logcat(Some(target), pkg, level, regex, exclude, buffer, tail, json);
            }
        }
        if rest[0] == "menu" || rest[0] == "?" {
            return run_menu(Some(target));
        }
        if rest.len() == 1 && rest[0] == "uninstall" {
            return run_interactive_apk_uninstall(Some(target));
        }
        if target.single_serial().is_none() || raw_args[1] == "--pick" || raw_args[1] == "@pick" || opts.adb_args != rest {
            return broadcast::run_locked(&target, opts, &adb_path, &cfg_store);
        }
        validate_native_command(&rest[0])?;
        return exec_with_target(Some(target), rest);
    }

    run_cli(&raw_args)
}

fn validate_native_command(name: &str) -> Result<()> {
    if adb::is_native_command(name) {
        return Ok(());
    }
    let cli = Cli::command();
    let mut suggestions: Vec<_> = cli.get_subcommands()
        .flat_map(|sub| sub.get_name_and_visible_aliases())
        .chain(adb::NATIVE_COMMANDS.iter().copied())
        .filter(|candidate| !name.is_empty() && candidate.starts_with(name))
        .collect();
    suggestions.sort_unstable();
    suggestions.dedup();
    let hint = if suggestions.is_empty() {
        String::new()
    } else {
        format!("你可能想运行：adbx {}。", suggestions.join("、adbx "))
    };
    bail!("无法识别子命令 `{name}`。{hint}运行 `adbx --help` 查看用法；自定义 adb 子命令可用 `adbx -- {name}` 透传。")
}

fn run_cli(raw_args: &[String]) -> Result<u8> {
    let cli = Cli::parse_from(raw_args);
    if let Some(Commands::External(args)) = &cli.command {
        validate_native_command(&args[0])?;
    }
    if let Some(Commands::SetupAdb { force }) = cli.command {
        adb::install_managed_platform_tools(force)?;
        return Ok(0);
    }
    if let Some(Commands::Update {
        check,
        version,
        force,
    }) = cli.command
    {
        return update::run_update(check, version, force);
    }
    if let Some(Commands::Uninstall {
        self_uninstall,
        purge,
        dry_run,
        yes,
    }) = cli.command
    {
        let invoked_self_alias = raw_args.get(1).is_some_and(|s| s == "self-uninstall");
        if self_uninstall || invoked_self_alias {
            return update::run_uninstall(purge, dry_run, yes);
        }
        if purge || dry_run || yes {
            eprintln!(
                "提示：卸载本机 adbx 请显式加上 --self（例如 `adbx uninstall --self --dry-run` 或 `adbx self-uninstall`）。"
            );
            return Ok(2);
        }
        return run_interactive_apk_uninstall(None);
    }
    if let Some(Commands::Config { action }) = cli.command {
        let dir = store::config_dir()?;
        return match action {
            ConfigCmd::Path => config_cmd::show_paths(&dir),
            ConfigCmd::Show => config_cmd::show_config(&dir),
            ConfigCmd::Edit => config_cmd::edit_config(&dir),
            ConfigCmd::Export { file } => config_cmd::export_config(&dir, &file),
            ConfigCmd::Import { file } => config_cmd::import_config(&dir, &file),
            ConfigCmd::Reset { yes } => config_cmd::reset_config(&dir, yes),
        };
    }
    let adb_path = adb::find_or_prompt_adb()?;
    let mut cfg_store = store::Store::load()?;
    let session_id = session::current_session_id();

    match cli.command {
        None | Some(Commands::Menu) => run_menu(None),
        Some(Commands::Repl) => {
            let target = if let Some(spec) = non_empty_env("ADBX_DEVICE") {
                let online: Vec<_> = adb::list_devices(&adb_path)?.into_iter().filter(|d| d.state == "device").collect();
                cfg_store.select_target(&spec, &online)?
            } else if let Some(locked) = cfg_store.session_target(&session_id)? {
                locked
            } else {
                DeviceTarget::single(&resolve_single_target_serial(&adb_path, &cfg_store, &session_id, None)?)
            };
            repl::run_repl(&adb_path, target, &cfg_store)
        }
        Some(Commands::Watch) => {
            watch::run_watch_tui(adb_path)?;
            Ok(0)
        }
        Some(Commands::Macro { action }) => match action {
            MacroCmd::List => {
                if cfg_store.list_macros().is_empty() {
                    println!("暂无自定义快捷宏，可用 `adbx macro set net-off shell svc wifi disable` 创建。");
                } else {
                    for (name, args) in cfg_store.list_macros() {
                        println!("{:<16} -> adb {}", name, args.join(" "));
                    }
                }
                Ok(0)
            }
            MacroCmd::Set { name, args } => {
                cfg_store.set_macro(&name, args.clone())?;
                println!("已保存自定义宏 '{name}' -> adb {}", args.join(" "));
                Ok(0)
            }
            MacroCmd::Rm { name } => {
                if cfg_store.remove_macro(&name)? {
                    println!("已删除自定义宏 '{name}'");
                } else {
                    println!("未找到自定义宏 '{name}'");
                }
                Ok(0)
            }
        },
        Some(Commands::History { action }) => match action {
            None => history::replay_history(&adb_path, &cfg_store, 100, &[], None),
            Some(HistoryCmd::List { limit, device, grep, json }) => {
                history::list_history(&cfg_store, limit, device.as_deref(), grep.as_deref(), json)
            }
            Some(HistoryCmd::Replay { limit, serials, args, cwd }) => {
                if args.is_empty() {
                    history::replay_history(&adb_path, &cfg_store, limit, &serials, cwd.as_deref())
                } else {
                    history::replay_command(&adb_path, &cfg_store, &serials, &args, cwd.as_deref())
                }
            }
            Some(HistoryCmd::Export { file }) => history::export_history(&cfg_store, &file),
            Some(HistoryCmd::Clear { yes }) => history::clear_history(&cfg_store, yes),
        },
        Some(Commands::Mcp { allow_destructive }) => {
            mcp::run_stdio_server(allow_destructive)?;
            Ok(0)
        }
        Some(Commands::Quick { action }) => {
            let serial = resolve_single_target_serial(&adb_path, &cfg_store, &session_id, None)?;
            palette::execute_macro(&action, &adb_path, &serial, &cfg_store)
        }
        Some(Commands::Group { action }) => match action {
            GroupCmd::List => {
                if cfg_store.list_groups().is_empty() {
                    println!("尚未定义设备分组，可通过 `adbx group set qa pixel mi` 创建。");
                } else {
                    for (name, members) in cfg_store.list_groups() {
                        println!("@{:<14} -> {}", name, members.join(", "));
                    }
                }
                Ok(0)
            }
            GroupCmd::Set { name, members } => {
                if matches!(name.as_str(), "all" | "pick") || name.contains(',') || name.starts_with('@') {
                    bail!("分组名不能为 all/pick 或包含逗号/@");
                }
                cfg_store.set_group(&name, members.clone())?;
                println!("已保存分组 @{name} -> {}", members.join(", "));
                Ok(0)
            }
            GroupCmd::Rm { name } => {
                if cfg_store.remove_group(&name)? {
                    println!("已删除分组 @{name}");
                } else {
                    println!("未找到分组 @{name}");
                }
                Ok(0)
            }
        },
        Some(Commands::Wifi { action }) => match action {
            WifiCmd::List => wifi::list_wifi(&adb_path, &cfg_store),
            WifiCmd::Enable => {
                let serial = resolve_single_target_serial(&adb_path, &cfg_store, &session_id, None)?;
                wifi::enable_wifi(&adb_path, &serial, &mut cfg_store)
            }
            WifiCmd::Reconnect => wifi::reconnect_all(&adb_path, &cfg_store),
        },
        Some(Commands::Port { action }) => match action {
            PortCmd::List => {
                let serial = resolve_single_target_serial(&adb_path, &cfg_store, &session_id, None)?;
                port::list_ports(&adb_path, &serial, &cfg_store)
            }
            PortCmd::Add { direction, local, remote } => {
                let serial = resolve_single_target_serial(&adb_path, &cfg_store, &session_id, None)?;
                port::add_port(&adb_path, &serial, &direction, &local, &remote, &mut cfg_store)
            }
            PortCmd::Sync { target } => {
                let serials = resolve_many_target_serials(&adb_path, &cfg_store, &session_id, target)?;
                port::sync_ports(&adb_path, &serials, &cfg_store)
            }
            PortCmd::Rm { direction, local, remote } => {
                port::remove_port(&direction, &local, &remote, &mut cfg_store)
            }
            PortCmd::Clear => {
                let serial = resolve_single_target_serial(&adb_path, &cfg_store, &session_id, None)?;
                port::clear_ports(&adb_path, &serial)
            }
        },
        Some(Commands::Reboot {
            mode,
            wait,
            shell,
            no_shell,
            root,
            remount,
            no_restore,
            yes,
        }) => {
            let serial = resolve_single_target_serial(&adb_path, &cfg_store, &session_id, None)?;
            dispatch_reboot(
                &adb_path, &cfg_store, &serial, mode, wait, shell, no_shell, root, remount, no_restore, yes,
            )
        }
        Some(Commands::InstallPlus { apk, launch, target, timeout }) => {
            run_install_plus(&adb_path, &cfg_store, &session_id, apk, launch, target, timeout)
        },
        Some(Commands::Devices { json, long }) => {
            if long && !json {
                return adb::run_passthrough(&adb_path, None, &["devices".into(), "-l".into()]);
            }
            let devices = adb::list_devices(&adb_path)?;
            let locked = cfg_store.session_target(&session_id)?;
            if json {
                let items: Vec<_> = devices
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "alias": cfg_store.alias_for(&d.serial),
                            "serial": d.serial,
                            "state": d.state,
                            "model": d.model,
                            "locked": is_device_locked_in(&locked, &d.serial, &cfg_store, &devices)
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(0);
            }
            if devices.is_empty() {
                println!("未检测到已连接的 ADB 设备。");
                return Ok(0);
            }
            println!(
                "{:<2} {:<16} {:<24} {:<10} MODEL",
                "", "ALIAS", "SERIAL", "STATE"
            );
            for d in &devices {
                let mark = if is_device_locked_in(&locked, &d.serial, &cfg_store, &devices) {
                    "*"
                } else {
                    " "
                };
                let alias = cfg_store.alias_for(&d.serial).unwrap_or("-");
                println!(
                    "{:<2} {:<16} {:<24} {:<10} {}",
                    mark, alias, d.serial, d.state, d.model
                );
            }
            if let Some(l) = &locked {
                if l.single_serial().is_none() {
                    println!("(当前终端已锁定为并行模式: {})", format_session_target(l, &cfg_store));
                }
            }
            Ok(0)
        }
        Some(Commands::Use { target, group }) => {
            let devices = adb::list_devices(&adb_path)?;
            let online: Vec<_> = devices.into_iter().filter(|d| d.state == "device").collect();
            let chosen_spec = if let Some(name) = group {
                if cfg_store.group_members(&name).is_none() { bail!("未定义设备分组 '{name}'"); }
                DeviceTarget::Group(name)
            } else if let Some(t) = target {
                cfg_store.select_target(&t, &online)?
            } else {
                match selector::pick_device_or_all(&online, &cfg_store)? {
                    selector::PickOutcome::AllOnline => DeviceTarget::All,
                    selector::PickOutcome::Single(d) => DeviceTarget::single(&d.serial),
                }
            };
            cfg_store.set_session_target(&session_id, &chosen_spec)?;
            println!(
                "已锁定当前终端会话 -> {}",
                format_session_target(&chosen_spec, &cfg_store)
            );
            Ok(0)
        }
        Some(Commands::Unuse) => {
            cfg_store.clear_session_device(&session_id)?;
            println!("已清除当前终端会话的设备锁定。");
            Ok(0)
        }
        Some(Commands::Alias { serial, name }) => {
            if name.starts_with('@') || name.contains(char::is_whitespace) || name.contains(',') {
                bail!("别名不能包含空格、逗号或以 @ 开头");
            }
            if matches!(name.as_str(), "all" | "pick") {
                bail!("'all' 和 'pick' 为多机广播保留字，请换一个别名");
            }
            let devices = adb::list_devices(&adb_path).unwrap_or_default();
            let resolved = cfg_store
                .resolve_input_to_serial(&serial, &devices)
                .unwrap_or(serial);
            cfg_store.set_alias(&resolved, &name)?;
            println!("已绑定别名: {name} -> {resolved}");
            Ok(0)
        }
        Some(Commands::Current { prompt }) => {
            if let Some(spec) = cfg_store.session_target(&session_id)? {
                let label = format_session_target(&spec, &cfg_store);
                if prompt {
                    print!("[adb:{label}]");
                } else {
                    println!("{label}");
                }
            } else if !prompt {
                println!("当前终端会话未锁定设备");
            }
            Ok(0)
        }
        Some(Commands::Log {
            pkg,
            level,
            regex,
            exclude,
            buffer,
            tail,
            json,
        }) => run_logcat(None, pkg, level, regex, exclude, buffer, tail, json),
        Some(Commands::Config { .. })
        | Some(Commands::SetupAdb { .. })
        | Some(Commands::Update { .. })
        | Some(Commands::Uninstall { .. }) => Ok(0),
        Some(Commands::Completions { shell }) => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "adbx", &mut io::stdout());
            Ok(0)
        }
        Some(Commands::Shell { args }) => {
            let mut full = Vec::with_capacity(args.len() + 1);
            full.push("shell".to_string());
            full.extend(args);
            exec_with_target(None, full)
        }
        Some(Commands::External(args)) => exec_with_target(None, args),
    }
}

fn is_device_locked_in(
    locked: &Option<DeviceTarget>,
    serial: &str,
    store: &store::Store,
    all_devs: &[adb::DeviceInfo],
) -> bool {
    match locked {
        Some(DeviceTarget::All) => true,
        Some(DeviceTarget::Fixed(list)) => list.iter().any(|s| s == serial),
        Some(DeviceTarget::Group(name)) => store
            .expand_group_online(name, all_devs)
            .is_ok_and(|list| list.iter().any(|d| d.serial == serial)),
        None => false,
    }
}

fn format_session_target(target: &DeviceTarget, store: &store::Store) -> String {
    match target {
        DeviceTarget::All => "ALL".into(),
        DeviceTarget::Group(name) => format!("@{name}"),
        DeviceTarget::Fixed(list) => match list.as_slice() {
            [serial] => format!("{} ({serial})", store.display_label(serial)),
            _ => list
                .iter()
                .map(|s| store.display_label(s))
                .collect::<Vec<_>>()
                .join(","),
        },
    }
}

fn run_menu(explicit_target: Option<DeviceTarget>) -> Result<u8> {
    let adb_path = adb::find_or_prompt_adb()?;
    let cfg_store = store::Store::load()?;
    let session_id = session::current_session_id();
    let serial = resolve_single_target_serial(&adb_path, &cfg_store, &session_id, explicit_target)?;
    palette::open_palette(&adb_path, &serial, &cfg_store)
}

fn parse_third_party_packages(output: &str) -> Vec<String> {
    let mut pkgs: Vec<String> = output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("package:")
                .map(|p| p.trim().to_string())
        })
        .filter(|p| !p.is_empty())
        .collect();
    pkgs.sort();
    pkgs.dedup();
    pkgs
}

fn run_interactive_apk_uninstall(explicit_target: Option<DeviceTarget>) -> Result<u8> {
    if !selector::is_interactive_tty() {
        eprintln!(
            "非交互终端下请指定应用包名（`adbx uninstall <包名>`）；卸载本机 adbx 请运行 `adbx uninstall --self`（或 `adbx self-uninstall`）。"
        );
        return Ok(2);
    }

    let adb_path = adb::find_or_prompt_adb()?;
    let cfg_store = store::Store::load()?;
    let session_id = session::current_session_id();
    let serial = resolve_single_target_serial(&adb_path, &cfg_store, &session_id, explicit_target)?;
    let label = cfg_store.display_label(&serial);

    let cap = adb::run_captured_with_timeout(
        &adb_path,
        Some(&serial),
        &[
            "shell".into(),
            "pm".into(),
            "list".into(),
            "packages".into(),
            "-3".into(),
        ],
        5,
    )?;
    if cap.exit_code != 0 {
        let err = String::from_utf8_lossy(&cap.stderr);
        bail!("获取第三方应用列表失败: {}", err.trim());
    }

    let pkgs = parse_third_party_packages(&String::from_utf8_lossy(&cap.stdout));
    if pkgs.is_empty() {
        println!("设备 [{label} / {serial}] 上未检测到已安装的第三方应用。");
        return Ok(0);
    }

    let prompt = format!(
        "勾选要卸载的第三方应用 [目标: {label}，共 {} 个]（空格勾选/取消，回车确认，Esc 取消）",
        pkgs.len()
    );
    let Some(indices) = dialoguer::MultiSelect::new()
        .with_prompt(prompt)
        .items(&pkgs)
        .max_length(15)
        .interact_opt()?
    else {
        println!("未勾选任何应用，已取消卸载。");
        return Ok(0);
    };

    if indices.is_empty() {
        println!("未勾选任何应用，已取消卸载。");
        return Ok(0);
    }

    let selected: Vec<String> = indices.into_iter().map(|i| pkgs[i].clone()).collect();
    for pkg in &selected {
        adb::validate_package_name(pkg)?;
    }

    let confirmed = if selected.len() == 1 {
        let cmd_preview = format!("uninstall {}", selected[0]);
        selector::confirm_dangerous(&label, &serial, &cmd_preview)?
    } else {
        eprintln!("待从 [{label} / {serial}] 卸载以下 {} 个应用：", selected.len());
        for pkg in &selected {
            eprintln!("  - {pkg}");
        }
        dialoguer::Confirm::new()
            .with_prompt(format!(
                "高危操作确认 -> 将从 [{label} / {serial}] 卸载以上 {} 个应用，确认执行？",
                selected.len()
            ))
            .default(false)
            .interact()?
    };

    if !confirmed {
        println!("已取消卸载。");
        return Ok(0);
    }

    let mut ok_count = 0usize;
    let mut fail_count = 0usize;
    for pkg in &selected {
        println!("[adbx] -> adb -s {serial} uninstall {pkg}");
        let args = vec!["uninstall".to_string(), pkg.clone()];
        let started = std::time::Instant::now();
        let code = adb::run_passthrough(&adb_path, Some(&serial), &args)?;
        history::record(&cfg_store, &label, std::slice::from_ref(&serial), &args, code, started.elapsed().as_millis());
        if code == 0 {
            ok_count += 1;
        } else {
            fail_count += 1;
        }
    }

    if selected.len() > 1 {
        println!("批量卸载完成: 成功 {ok_count} 个，失败 {fail_count} 个。");
    }
    Ok(if fail_count == 0 { 0 } else { 1 })
}

#[allow(clippy::too_many_arguments)]
fn run_logcat(
    explicit_target: Option<DeviceTarget>,
    pkg: Option<String>,
    level: String,
    regex: Option<String>,
    exclude: Option<String>,
    buffer: usize,
    tail: Option<usize>,
    json: bool,
) -> Result<u8> {
    let adb_path = adb::find_or_prompt_adb()?;
    let cfg_store = store::Store::load()?;
    let session_id = session::current_session_id();
    let target_serial =
        resolve_single_target_serial(&adb_path, &cfg_store, &session_id, explicit_target)?;
    let min_level = logcat::LogLevel::from_str_lossy(&level);

    if tail.is_some() || json || !selector::is_interactive_tty() {
        let max_lines = tail.unwrap_or(150);
        let snap = logcat_snapshot::fetch_snapshot(
            &adb_path,
            &target_serial,
            pkg.as_deref(),
            min_level,
            regex.as_deref().unwrap_or(""),
            exclude.as_deref().unwrap_or(""),
            max_lines,
        )?;
        if json {
            println!("{}", serde_json::to_string_pretty(&snap)?);
        } else {
            use std::io::IsTerminal;
            let is_tty = std::io::stdout().is_terminal();
            let custom_rules = highlight::compile_custom_rules(cfg_store.list_log_highlights());
            for line in &snap.lines {
                if is_tty {
                    let lvl = logcat::LogLevel::from_str_lossy(&line.level);
                    println!(
                        "{}",
                        logcat::format_ansi_fields(
                            &line.timestamp,
                            &line.pid,
                            None,
                            lvl,
                            &line.tag,
                            &line.message,
                            &custom_rules,
                        )
                    );
                } else {
                    println!(
                        "{} {:>5} {} {:<18} {}",
                        line.timestamp, line.pid, line.level, line.tag, line.message
                    );
                }
            }
            if snap.truncated {
                eprintln!(
                    "... [已截断，仅显示最近 {} / 共 {} 条匹配日志]",
                    snap.returned, snap.total_matched
                );
            }
        }
        return Ok(0);
    }

    let label = cfg_store.display_label(&target_serial);
    let opts = logcat::LogcatOptions {
        adb_path,
        serial: target_serial,
        device_label: label,
        package: pkg,
        min_level,
        include_pattern: regex.unwrap_or_default(),
        exclude_pattern: exclude.unwrap_or_default(),
        max_buffer: buffer.max(1_000),
    };
    logcat::run_tui(opts)?;
    Ok(0)
}

fn exec_with_target(explicit_target: Option<DeviceTarget>, args: Vec<String>) -> Result<u8> {
    let adb_path = adb::find_or_prompt_adb()?;
    let mut cfg_store = store::Store::load()?;
    let session_id = session::current_session_id();

    if adb::is_global_or_explicit_adb_command(&args) {
        return adb::run_passthrough(&adb_path, None, &args);
    }

    let is_raw_shell = args.len() == 2 && args[0] == "shell" && args[1] == "--raw";
    let is_interactive_shell = (args.len() == 1 && args[0] == "shell") || is_raw_shell;
    let env_target = non_empty_env("ADBX_DEVICE");
    let locked_target = if explicit_target.is_none() && env_target.is_none() { cfg_store.session_target(&session_id)? } else { None };

    // 修复：空 ADBX_DEVICE 不再遮蔽当前会话锁定
    if explicit_target.is_none() && !is_interactive_shell {
        if let Some(ref env_t) = env_target {
            let clean = env_t.trim_start_matches('@');
            if clean.eq_ignore_ascii_case("all")
                || clean.contains(',')
                || cfg_store.group_members(clean).is_some()
            {
                let opts = broadcast::parse_broadcast_args(&args)?;
                let normalized = if clean.eq_ignore_ascii_case("all") { "all" } else { clean };
                return broadcast::run_broadcast(normalized, opts, &adb_path, &cfg_store);
            }
        } else if let Some(ref locked) = locked_target {
            if locked.single_serial().is_none() {
                let opts = broadcast::parse_broadcast_args(&args)?;
                return broadcast::run_locked(locked, opts, &adb_path, &cfg_store);
            }
        }
    }

    let target_serial = if explicit_target.is_none()
        && !is_interactive_shell
        && env_target.is_none()
        && locked_target.is_none()
    {
        let online: Vec<_> = adb::list_devices(&adb_path)?
            .into_iter()
            .filter(|d| d.state == "device")
            .collect();
        match online.len() {
            0 => bail!("没有在线的 ADB 设备（请检查 USB 调试或执行 adbx devices）"),
            1 => online[0].serial.clone(),
            _ => match selector::pick_device_or_all(&online, &cfg_store)? {
                selector::PickOutcome::AllOnline => {
                    let opts = broadcast::parse_broadcast_args(&args)?;
                    return broadcast::run_broadcast("all", opts, &adb_path, &cfg_store);
                }
                selector::PickOutcome::Single(d) => d.serial,
            },
        }
    } else {
        resolve_single_target_serial(&adb_path, &cfg_store, &session_id, explicit_target)?
    };

    // 交互终端下 `adbx shell` 默认进入内置增强 Shell（支持灰色内联建议、Tab 路径/命令补全与历史搜索）；
    // 传入 `adbx shell --raw` 或在增强 Shell 中执行 `:raw` 可随时进入原生 adb shell PTY。
    if args.len() == 1 && args[0] == "shell" && selector::is_interactive_tty() {
        return repl::run_repl(&adb_path, DeviceTarget::single(&target_serial), &cfg_store);
    }

    let mut args = if is_raw_shell {
        vec!["shell".to_string()]
    } else {
        args
    };

    if adb::is_dangerous_command(&args) {
        let label = cfg_store.display_label(&target_serial);
        let cmd_preview = args.join(" ");
        if !selector::confirm_dangerous(&label, &target_serial, &cmd_preview)? {
            println!("已取消执行。");
            return Ok(0);
        }
    }

    let started = std::time::Instant::now();
    let code = if args.first().map(|s| s.as_str()) == Some("logcat") {
        if args.iter().any(|a| a == "--no-color") {
            args.retain(|a| a != "--no-color");
            adb::run_passthrough(&adb_path, Some(&target_serial), &args)?
        } else if logcat::should_colorize_logcat(&args) {
            logcat::run_colored_stream(&adb_path, &target_serial, &args, &cfg_store)?
        } else {
            adb::run_passthrough(&adb_path, Some(&target_serial), &args)?
        }
    } else {
        adb::run_passthrough(&adb_path, Some(&target_serial), &args)?
    };
    let label = cfg_store.display_label(&target_serial);
    history::record(&cfg_store, &label, std::slice::from_ref(&target_serial), &args, code, started.elapsed().as_millis());

    // 自动记忆该设备是否执行过 root / remount，供后续 `adbx reboot` 自动恢复
    if code == 0 {
        match args.first().map(String::as_str) {
            Some("root") => cfg_store.update_device_flags(&target_serial, |f| f.wants_root = true)?,
            Some("remount") => cfg_store.update_device_flags(&target_serial, |f| {
                f.wants_root = true;
                f.wants_remount = true;
            })?,
            Some("unroot") => cfg_store.update_device_flags(&target_serial, |f| *f = Default::default())?,
            _ => {}
        }
    }

    Ok(code)
}

fn resolve_single_target_serial(
    adb_path: &std::path::PathBuf,
    cfg_store: &store::Store,
    session_id: &str,
    explicit_target: Option<DeviceTarget>,
) -> Result<String> {
    let devices = adb::list_devices(adb_path)?;
    let online: Vec<_> = devices.into_iter().filter(|d| d.state == "device").collect();

    let online = if let Some(target) = explicit_target {
        cfg_store.locked_devices(&target, &online)?
    } else if let Some(spec) = non_empty_env("ADBX_DEVICE") {
        resolve_target_devices(&spec, &online, cfg_store)?
    } else if let Some(locked) = cfg_store.session_target(session_id)? {
        cfg_store.locked_devices(&locked, &online)?
    } else {
        online
    };

    match online.len() {
        0 => bail!("没有在线的 ADB 设备（请检查 USB 调试或执行 adbx devices）"),
        1 => Ok(online[0].serial.clone()),
        _ => Ok(selector::pick_device(&online, cfg_store)?.serial),
    }
}

fn resolve_target_devices(spec: &str, online: &[adb::DeviceInfo], store: &store::Store) -> Result<Vec<adb::DeviceInfo>> {
    let spec = spec.trim_start_matches('@');
    if spec.eq_ignore_ascii_case("all") { return Ok(online.to_vec()); }
    if spec == "pick" { return selector::pick_multiple_devices(online, store); }
    let mut selected = Vec::new();
    for part in spec.split(',').map(str::trim) {
        if store.group_members(part).is_some() {
            for device in store.expand_group_online(part, online)? {
                if !selected.iter().any(|d: &adb::DeviceInfo| d.serial == device.serial) { selected.push(device); }
            }
        } else {
            let serial = store.resolve_input_to_serial(part, online)?;
            let device = online.iter().find(|d| d.serial == serial).ok_or_else(|| anyhow::anyhow!("设备 {serial} 未在线"))?;
            if !selected.iter().any(|d| d.serial == serial) { selected.push(device.clone()); }
        }
    }
    Ok(selected)
}

fn resolve_many_target_serials(
    adb_path: &std::path::PathBuf,
    cfg_store: &store::Store,
    session_id: &str,
    explicit: Option<String>,
) -> Result<Vec<String>> {
    let online: Vec<_> = adb::list_devices(adb_path)?
        .into_iter()
        .filter(|d| d.state == "device")
        .collect();
    if online.is_empty() {
        bail!("没有在线的 ADB 设备");
    }

    let spec = explicit.or_else(|| non_empty_env("ADBX_DEVICE"));
    if spec.is_none() {
        if let Some(locked) = cfg_store.session_target(session_id)? {
            return Ok(cfg_store.locked_devices(&locked, &online)?.into_iter().map(|d| d.serial).collect());
        }
    }

    match spec {
        Some(raw) => Ok(resolve_target_devices(&raw, &online, cfg_store)?.into_iter().map(|d| d.serial).collect()),
        None => match online.len() {
            1 => Ok(vec![online[0].serial.clone()]),
            _ => match selector::pick_device_or_all(&online, cfg_store)? {
                selector::PickOutcome::AllOnline => Ok(online.into_iter().map(|d| d.serial).collect()),
                selector::PickOutcome::Single(d) => Ok(vec![d.serial]),
            },
        },
    }
}

fn run_install_plus(
    adb_path: &std::path::PathBuf,
    cfg_store: &store::Store,
    session_id: &str,
    apk: String,
    launch: Option<String>,
    target: Option<String>,
    timeout: u64,
) -> Result<u8> {
    let serials = resolve_many_target_serials(adb_path, cfg_store, session_id, target)?;
    install_on_serials(adb_path, cfg_store, &serials, apk, launch, timeout)
}

fn serials_for_target(
    adb_path: &std::path::PathBuf,
    store: &store::Store,
    target: &DeviceTarget,
) -> Result<Vec<String>> {
    let online: Vec<_> = adb::list_devices(adb_path)?
        .into_iter()
        .filter(|d| d.state == "device")
        .collect();
    let devices = store.locked_devices(target, &online)?;
    if devices.is_empty() {
        bail!("没有在线的 ADB 设备");
    }
    Ok(devices.into_iter().map(|d| d.serial).collect())
}

fn install_on_serials(
    adb_path: &std::path::PathBuf,
    cfg_store: &store::Store,
    serials: &[String],
    apk: String,
    launch: Option<String>,
    timeout: u64,
) -> Result<u8> {
    if !std::path::Path::new(&apk).is_file() {
        bail!("APK 文件不存在或不是普通文件: {apk}");
    }
    if let Some(ref pkg) = launch {
        adb::validate_package_name(pkg)?;
    }
    let opts = broadcast::BroadcastOptions {
        cwd: None,
        mode_override: Some(broadcast::OutputMode::Stream),
        max_jobs: 8,
        timeout_secs: timeout,
        assume_yes: true,
        no_color: false,
        json_output: false,
        adb_args: vec!["install".into(), "-r".into(), "-d".into(), "-g".into(), apk],
    };
    let code = broadcast::run_on_serials(serials, opts, adb_path, cfg_store, None)?;
    if code == 0 {
        if let Some(pkg) = launch {
            let launch_opts = broadcast::BroadcastOptions {
                cwd: None,
                mode_override: Some(broadcast::OutputMode::Grouped),
                max_jobs: 8,
                timeout_secs: 15,
                assume_yes: true,
                no_color: false,
                json_output: false,
                adb_args: vec![
                    "shell".into(),
                    "monkey".into(),
                    "-p".into(),
                    pkg,
                    "-c".into(),
                    "android.intent.category.LAUNCHER".into(),
                    "1".into(),
                ],
            };
            return broadcast::run_on_serials(serials, launch_opts, adb_path, cfg_store, None);
        }
    }
    Ok(code)
}

#[allow(clippy::too_many_arguments)]
fn dispatch_reboot(
    adb_path: &std::path::PathBuf,
    cfg_store: &store::Store,
    serial: &str,
    mode: Option<String>,
    wait: u64,
    shell: bool,
    no_shell: bool,
    root: bool,
    remount: bool,
    no_restore: bool,
    yes: bool,
) -> Result<u8> {
    let remembered = cfg_store.get_device_flags(serial);
    let restore_root = root || (!no_restore && remembered.wants_root);
    let restore_remount = remount || (!no_restore && remembered.wants_remount);
    // 若用户此前做过 root/remount（或显式传了 --shell/--root/--remount），重启完成后默认自动进入 shell
    let enter_shell = !no_shell && (shell || restore_root || restore_remount);

    reboot::run_smart_reboot(
        adb_path,
        serial,
        cfg_store,
        reboot::RebootOptions {
            wait_secs: wait,
            enter_shell,
            restore_root,
            restore_remount,
            assume_yes: yes,
            target_mode: mode,
        },
    )
}

fn extract_leading_target(raw_args: &[String]) -> Result<Option<(String, Vec<String>)>> {
    let Some(arg1) = raw_args.get(1).map(|s| s.as_str()) else {
        return Ok(None);
    };

    if arg1 == "--all" {
        return Ok(Some(("all".to_string(), raw_args[2..].to_vec())));
    }
    if arg1 == "--pick" {
        return Ok(Some(("pick".to_string(), raw_args[2..].to_vec())));
    }
    if matches!(arg1, "--device" | "--group" | "--target") {
        let Some(val) = raw_args.get(2).map(|s| s.trim().trim_start_matches('@')) else {
            bail!("参数 {arg1} 缺少目标值，例如: adbx {arg1} pixel shell getprop ro.product.model");
        };
        if val.is_empty() {
            bail!("参数 {arg1} 的目标值不能为空");
        }
        return Ok(Some((val.to_string(), raw_args[3..].to_vec())));
    }
    if let Some(val) = arg1
        .strip_prefix("--device=")
        .or_else(|| arg1.strip_prefix("--group="))
        .or_else(|| arg1.strip_prefix("--target="))
    {
        let clean = val.trim().trim_start_matches('@');
        if clean.is_empty() {
            bail!("目标参数不能为空，例如: adbx --device=pixel shell getprop ro.product.model");
        }
        return Ok(Some((clean.to_string(), raw_args[2..].to_vec())));
    }
    if let Some(val) = arg1.strip_prefix('@') {
        if val.trim().is_empty() {
            bail!("使用 `@` 简写时请指定设备别名或分组，例如: adbx --all shell ...");
        }
        return Ok(Some((val.trim().to_string(), raw_args[2..].to_vec())));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn extracts_standard_flags_and_legacy_at_syntax() {
        assert_eq!(
            extract_leading_target(&s(&["adbx", "--all", "shell", "getprop"])).unwrap(),
            Some(("all".into(), s(&["shell", "getprop"])))
        );
        assert_eq!(
            extract_leading_target(&s(&["adbx", "--pick", "reboot"])).unwrap(),
            Some(("pick".into(), s(&["reboot"])))
        );
        assert_eq!(
            extract_leading_target(&s(&["adbx", "--device", "pixel", "shell", "ls"])).unwrap(),
            Some(("pixel".into(), s(&["shell", "ls"])))
        );
        assert_eq!(
            extract_leading_target(&s(&["adbx", "--group=qa", "shell", "id"])).unwrap(),
            Some(("qa".into(), s(&["shell", "id"])))
        );
        assert_eq!(
            extract_leading_target(&s(&["adbx", "@all", "shell", "getprop"])).unwrap(),
            Some(("all".into(), s(&["shell", "getprop"])))
        );
        assert_eq!(
            extract_leading_target(&s(&["adbx", "shell", "getprop"])).unwrap(),
            None
        );
        assert!(extract_leading_target(&s(&["adbx", "--device"])).is_err());
    }

    #[test]
    fn parses_and_sorts_third_party_packages() {
        let raw = "\npackage:com.zeta.app\r\npackage:com.alpha.app\npackage:com.alpha.app\nignored-line\npackage:  \n";
        assert_eq!(
            parse_third_party_packages(raw),
            vec!["com.alpha.app".to_string(), "com.zeta.app".to_string()]
        );
    }
}
