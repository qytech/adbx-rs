# adbx (Rust 跨平台增强型 ADB)

面向 **macOS & Windows & Linux** 平台的增强型 ADB：防串机会话锁、设备分组、多机并行广播、断线重连 Ratatui Logcat / 彩色流式 `logcat`、内置补全增强 Shell、Wi-Fi 与端口管理、`install+` 以及安全加固 MCP Server。

## 安装（全平台，类似 `fd` / `rg`）

### macOS / Linux

- **Homebrew**（macOS Apple Silicon / Intel & Linux x86_64 / arm64）：
  ```bash
  brew tap Jax-Lifu/adbx-rs https://github.com/Jax-Lifu/adbx-rs
  brew install adbx
  ```
- **Debian / Ubuntu**（从 [GitHub Releases](https://github.com/Jax-Lifu/adbx-rs/releases) 下载 `.deb` 包，支持 `amd64` 与 `arm64`）：
  ```bash
  sudo apt install ./adbx_0.0.1_amd64.deb   # 或 arm64.deb
  ```
- **Arch Linux**（使用 [packaging/aur/PKGBUILD](packaging/aur/PKGBUILD) 或 Release 附带的 `PKGBUILD`）：
  ```bash
  makepkg -si
  ```
- **一键脚本安装**（支持 macOS arm64/x86_64 与 Linux static musl x86_64 / arm64，自动校验 SHA256）：
  ```bash
  curl -fsSL https://raw.githubusercontent.com/Jax-Lifu/adbx-rs/main/scripts/install.sh | sh
  ```

### Windows

- **Scoop**（本项目自带 [bucket/adbx.json](bucket/adbx.json)）：
  ```powershell
  scoop bucket add adbx https://github.com/Jax-Lifu/adbx-rs
  scoop install adbx/adbx
  ```
- **PowerShell 一键安装**（自动校验 SHA256 并加入用户 `PATH`）：
  ```powershell
  irm https://raw.githubusercontent.com/Jax-Lifu/adbx-rs/main/scripts/install.ps1 | iex
  ```

### Cargo / Rust 生态

- **从 GitHub Release 直接下载二进制**（[cargo-binstall](https://github.com/cargo-bins/cargo-binstall)）：
  ```bash
  cargo binstall adbx
  ```
- **从源码编译安装**：
  ```bash
  cargo install --path .
  ```

---

## 核心特性

- **防串机与多机并行**
  - `adbx use <设备|分组|all>` 为当前终端标签页独立锁定目标；`adbx unuse` 解除锁定。
  - 已锁单机后仍可随时通过前置参数临时切到多机：`adbx --all shell getprop`、`adbx --pick ...`、`adbx --device <别名|分组|a,b> ...`（向下兼容 `@all` / `@pick` / `@<目标>`）。
  - **管道输出自动标识设备**：多机结果通过 `| grep` 等管道输出时，每行自动附加对齐的 `[设备名]` 前缀。
- **增强型交互 Shell (`adbx shell` / `adbx repl`)**
  - 实时灰色内联命令建议（`→` / `Ctrl+E` 一键采纳历史或常用 Android 调试命令）。
  - `Tab` 补全命令名、远端设备文件路径（如 `cat proc/asound/c`）与 `:use` 目标；`↑`/`↓` 与 `Ctrl+R` 检索历史。
  - 原生接管 `su` 提权模式（`#` 提示符）、自动为 `vi`/`top` 分配 PTY、危险命令二次确认，以及 `:raw` / `adbx shell --raw` 随时切回原生 PTY。
- **彩色流式 `adbx logcat` 与交互式 `adbx log` TUI**
  - `adbx logcat` 在终端下自动按日志等级（`V`/`D`/`I`/`W`/`E`/`F`）显示实心色块徽章、Tag 哈希配色与 VS Code 风格语义高亮（支持 `--no-color` 关闭）。
  - `adbx log` 提供支持断线自动重连、包名追 PID、正则过滤、崩溃跳转与 10 万行缓冲的 Ratatui TUI。
- **智能重启 / 监控 / 无线 / 端口 / 装包 / MCP**
  - `adbx reboot` (`rr`)、`adbx watch`、`adbx wifi`、`adbx port`、`adbx install+`、`adbx mcp`。

## 宿主机动态 Tab 补全配置

安装后配置动态补全（支持补全设备别名、`adbx shell cat /proc/asound/` 等远端路径）：

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

## 发布新版本（GitHub Releases + Brew + Scoop）

1. 更新 `Cargo.toml` 版本号并提交代码。
2. 推送版本标签触发 [.github/workflows/release.yml](.github/workflows/release.yml)：
   ```bash
   git tag -a v0.0.1 -m "Release v0.0.1"
   git push origin v0.0.1
   ```
   CI 会自动构建 5 个平台的 `.tar.gz`/`.zip`/`.deb` 包、`SHA256SUMS.txt`、已填入真实 SHA256 的 `adbx.rb`、`adbx.json` 与 `PKGBUILD` 并发布到 GitHub Releases。
3. 将 Release 中生成的 `adbx.rb`、`adbx.json`、`PKGBUILD` 同步提交到仓库对应目录，即可让用户通过 `brew`、`scoop` 和 AUR 安装更新。

## 构建与测试

```bash
cargo clippy --all-targets -- -D warnings
cargo test
```
