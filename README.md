# adbx

[![CI](https://github.com/Jax-Lifu/adbx-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/Jax-Lifu/adbx-rs/actions/workflows/ci.yml)
[![Release](https://github.com/Jax-Lifu/adbx-rs/actions/workflows/release.yml/badge.svg)](https://github.com/Jax-Lifu/adbx-rs/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

面向 **macOS、Linux 与 Windows** 的 Rust 增强型 ADB 工具：解决多机研发中的**串机误操作、批量执行、无提示 Shell、无高亮 Logcat 与 AI Agent 接入**问题，同时保持与原生 `adb` 参数 100% 兼容。

---

## 为什么用 `adbx`？

| 痛点 | 原生 `adb` | `adbx` |
| --- | --- | --- |
| 多台设备防串机 | 每次手敲 `-s <长序列号>`，极易操作错手机 | `adbx use <别名>` 按终端标签页独立锁定，互不干扰 |
| 多机并行与管道过滤 | 不支持，需写 `for` 循环 | `adbx --all` / `--pick` / `--group` 并发执行，`| grep` 时每行自动附带 `[设备名]` 前缀 |
| 交互式 `adb shell` | 无历史建议、无自定义提示、切目录麻烦 | 内置灰色实时命令建议（`→` 采纳）、`Tab` 补全命令与远端路径、原生接管 `su`（`#`） |
| `adb logcat` 可读性 | 纯黑白文本流，难快速定位报错 | 实心 `V/D/I/W/E/F` 等级徽章 + Tag 着色 + VS Code 语义高亮；另有 `adbx log` TUI |
| 重启后调试环境 | `reboot` 后需手动等开机并重敲 `root` / `remount` | `adbx reboot` (`rr`) 自动等待开机、恢复 `root & remount` 并进入 Shell |
| 危险命令保护 | `rm` / `pm clear` / `uninstall` 直接执行 | 交互式二次确认，非交互终端下自动拦截 |
| AI Agent 接入 | 需额外封装脚本且易被注入 | 内置原生 `adbx mcp` stdio 服务器（默认只读白名单 + 多模态截图/UI 树/操作） |

---

## 安装

### macOS / Linux

- **Homebrew**（一行直接安装，支持 macOS Apple Silicon / Intel & Linux x86_64 / arm64）：
  ```bash
  brew install Jax-Lifu/tap/adbx
  ```
- **Debian / Ubuntu**（从 [GitHub Releases](https://github.com/Jax-Lifu/adbx-rs/releases) 下载 `.deb`，支持 `amd64` 与 `arm64`）：
  ```bash
  sudo apt install ./adbx_0.0.2_amd64.deb   # 或 adbx_0.0.2_arm64.deb
  ```
- **Arch Linux**（使用 [packaging/aur/PKGBUILD](packaging/aur/PKGBUILD) 或 Release 附带的 `PKGBUILD`）：
  ```bash
  makepkg -si
  ```
- **一键脚本安装**（自动校验 SHA256，支持 macOS arm64/x86_64 与 Linux static musl x86_64 / arm64）：
  ```bash
  curl -fsSL https://raw.githubusercontent.com/Jax-Lifu/adbx-rs/main/scripts/install.sh | sh
  ```

### Windows

- **Winget**（推荐，官方提交后支持直接安装；清单位于 [packaging/winget/](packaging/winget/)）：
  ```powershell
  winget install Jax-Lifu.adbx
  ```
- **Chocolatey**（自动安装 `adb` 依赖；包定义位于 [packaging/chocolatey/](packaging/chocolatey/)）：
  ```powershell
  choco install adbx
  ```
- **Scoop**（一行直接安装，仓库自带 [bucket/adbx.json](bucket/adbx.json)）：
  ```powershell
  scoop install https://raw.githubusercontent.com/Jax-Lifu/adbx-rs/main/bucket/adbx.json
  # 如需通过 bucket 追踪更新，也可使用:
  # scoop bucket add adbx https://github.com/Jax-Lifu/adbx-rs && scoop install adbx/adbx
  ```
- **PowerShell 一键安装**（自动校验 SHA256、解压至 `%LOCALAPPDATA%\adbx\bin` 并加入用户 `PATH`）：
  ```powershell
  irm https://raw.githubusercontent.com/Jax-Lifu/adbx-rs/main/scripts/install.ps1 | iex
  ```

### Cargo / Rust

```bash
# 通过 crates.io 编译安装（或使用 cargo-binstall 直接拉取预编译二进制）
cargo install adbx
cargo binstall adbx

# 或从本地源码安装
cargo install --path .
```

### 关于 `adb` 依赖（开箱即用）

- 通过 **Chocolatey**（Windows）、**Scoop**（Windows）、**AUR**（Arch Linux）或 **`.deb`** 安装时，会自动关联/推荐安装系统 `adb`。
- 若通过一键脚本、Winget、Homebrew、Cargo 或源码安装且本机尚未装 `adb`，首次在交互终端运行 `adbx` 时会提示一键自动下载安装 Google 官方 Android Platform-Tools，也可随时手动执行：
  ```bash
  adbx setup-adb
  ```

### 在线自升级

一键安装或源码编译的用户可直接检查并升级到最新 Release（自动校验 SHA256）：

```bash
adbx update --check     # 仅检查新版本
adbx update             # 或 adbx upgrade，自动下载并更新
```

*(若通过 Homebrew / Winget / Chocolatey / Scoop / apt / pacman 安装，建议优先使用对应包管理器升级，如 `brew upgrade adbx` / `winget upgrade Jax-Lifu.adbx` / `choco upgrade adbx` / `scoop update adbx`。)*

---

## 快速上手与常用命令

### 1. 设备别名、分组与会话防串机

```bash
adbx devices                         # 列出设备（* 标记当前终端锁定的目标，支持 -l / --json）
adbx alias 7c9349cc5027c4c7 cayin    # 为序列号绑定人类可读别名
adbx group set qa cayin pixel        # 创建设备分组 qa

adbx use cayin                       # 仅在当前终端窗口锁定 cayin（其他窗口不受影响）
adbx current --prompt                # 输出 [adb:cayin (7c9349cc...)]，可集成到终端提示符
adbx unuse                           # 取消当前窗口锁定
```

### 2. 多机并行（锁定单机后也能随时覆盖）

```bash
adbx --all shell getprop ro.product.model      # 对全部在线设备并行执行
adbx --pick reboot                             # 交互式多选设备并行执行
adbx --group qa shell id                       # 仅对分组 qa 并行执行
adbx --device cayin,pixel install+ app.apk     # 指定多台设备并行执行

# 通过管道过滤时自动为每行附加对齐的 [设备名] 前缀，一眼区分归属设备：
adbx --all shell getprop | grep adb
```

> 所有前置参数均向下兼容旧简写 `@all` / `@pick` / `@<别名或分组>`；若要 100% 原样透传原生 `adb`（跳过增强逻辑），使用 `adbx -- <args>`。

### 3. 增强型 Shell（`adbx shell` / `adbx repl`）

```bash
adbx shell          # 进入内置增强 Shell
adbx shell --raw    # 直接启动原生 adb shell PTY
```

- **实时灰色命令建议**：输入时优先提示历史命令，其次匹配内置常用 Android 调试命令与自定义宏，按 `→` 或 `Ctrl+E` 一键采纳。
- **`Tab` 智能补全**：补全命令名、远端设备路径（如 `cat proc/asound/c`，含 3 秒目录缓存）以及 `:use` 目标。
- **权限与交互支持**：输入 `su` 自动探测并切换为 `#` 提权模式（支持 `su -c` / `su 0` 与原生 `uid=0`）；输入 `vi` / `top` 自动分配 PTY；输入 `:raw` 临时切入原生 `adb shell`，退出后回到增强 Shell。
- **快捷内建指令**：`:use <别名|all|组>`、`:devices`、`:history`、`:clear`、`:help`。

### 4. 彩色 `logcat` 与交互式 `adbx log` TUI

```bash
adbx logcat                      # 终端实时按 V/D/I/W/E/F 等级高亮 + Tag 配色 + 语义高亮（--no-color 关闭）
adbx log --pkg com.example.app   # 启动 Ratatui 交互式 Logcat TUI（断线重连、包名追 PID、/正则、!排除、n/N 崩溃跳转）
adbx log --tail 100 --level W    # 非交互拉取最近 100 条 Warn+ 日志（支持 --json 供脚本/AI 解析）
```

可在 `devices.toml`（执行 `adbx config edit`）中自定义日志语义高亮规则：

```toml
[log_highlights]
"ORDER_PAY|CHECKOUT" = "magenta"
"TIMEOUT|RETRY" = "red_bg"
"SYNC_DONE" = "green"
```

### 5. 其他高频效率工具

- **智能重启**：`adbx reboot`（别名 `adbx rr`）——记住是否做过 `root`/`remount`，重启等待开机完成后自动恢复并进入增强 Shell。
- **增强装包**：`adbx install+ app.apk --launch com.example.app -t all`——自动附加 `-r -d -g` 支持降级与自动授权。
- **实时多机面板**：`adbx watch`——直连本地 `adb-server`（支持 `ANDROID_ADB_SERVER_PORT`）监控热插拔、电量与温度。
- **无线与端口管理**：`adbx wifi enable` / `list` / `reconnect`；`adbx port add reverse 8081 8081` / `sync -t all`。
- **命令历史与宏**：`adbx h`（交互重放）、`adbx history export out.sh`、`adbx macro set`、`adbx menu`（或 `adbx ?`）。

---

## 宿主机动态 Tab 补全配置

安装后挂载动态补全，即可直接在宿主机终端按 `Tab` 补全设备别名及远端路径（如 `adbx --device cayin shell cat /proc/asound/<Tab>`）：

- **Zsh**（写入 `~/.zshrc`）：
  ```zsh
  source <(COMPLETE=zsh adbx)
  ```
- **Bash**（写入 `~/.bashrc`）：
  ```bash
  source <(COMPLETE=bash adbx)
  ```
- **Windows PowerShell**（写入 `$PROFILE`）：
  ```powershell
  $env:COMPLETE = "powershell"
  adbx | Out-String | Invoke-Expression
  Remove-Item Env:\COMPLETE
  ```

---

## 原生 MCP Server（AI Agent 集成）

`adbx mcp` 通过 stdio 提供原生 JSON-RPC 2.0 MCP 服务，默认启用**只读白名单安全模式**（仅放行 `getprop`、`dumpsys`、`pm list/path`、`ls`、`cat`、`ps` 等只读命令，禁止 shell 链式注入与 `pm clear`）；需要完全权限时可显式传入 `--allow-destructive`。

```json
{
  "mcpServers": {
    "adbx": {
      "command": "adbx",
      "args": ["mcp"]
    }
  }
}
```

可用工具：`list_devices`、`get_logcat`、`take_screenshot`、`dump_ui_tree`、`ui_tap`、`ui_swipe`、`ui_input`、`ui_keyevent`、`run_shell`、`app_action`。

---

## 开发、测试与发版

```bash
cargo clippy --all-targets -- -D warnings
cargo test
```

推送版本标签（例如 `git tag -a v0.0.2 -m "Release v0.0.2" && git push origin v0.0.2`）后，[.github/workflows/release.yml](.github/workflows/release.yml) 会自动构建 macOS（arm64/x86_64）、Linux（musl x86_64/arm64 + `.deb`）、Windows（x86_64 `.zip`）归档，生成 `SHA256SUMS.txt`、Homebrew `adbx.rb`、Scoop `adbx.json`、Arch `PKGBUILD`、Chocolatey 与 Winget 清单并发布至 GitHub Releases。

---

## 开源协议

本项目基于 [MIT License](LICENSE) 开源。
