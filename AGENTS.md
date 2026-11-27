# AGENTS.md

This file guides AI coding agents working on `adbx-rs` (`adbx`), a cross-platform (macOS, Linux, Windows) enhanced ADB CLI, TUI, enhanced REPL, and MCP server written in Rust (edition 2021).

## Build, Lint & Test

Always run the full verification suite after making code changes:

```bash
cargo clippy --all-targets -- -D warnings
cargo test
sh -n scripts/install.sh
```

- **Zero-warning policy**: `clippy --all-targets -- -D warnings` must pass cleanly.
- **Hermetic tests**: All 32 unit tests (`src/*.rs`) and 11 end-to-end integration tests ([tests/cli_integration.rs](tests/cli_integration.rs)) run against mock `adb` scripts/PowerShell shims and isolated temp directories—never require a real Android device or touch real user config files.

## Architecture & Module Map

- [src/main.rs](src/main.rs): `clap` CLI definition (`Cli`, `Commands`), `CompleteEnv` dynamic completion entrypoint, leading-target extraction (`--all`, `--pick`, `--device`, `--group`, `@...`), native `adb` passthrough (`--` escape hatch and `-s`/`-a`/etc.), and command dispatch.
- [src/adb.rs](src/adb.rs): Cross-platform `adb` discovery (PATH, `ANDROID_HOME`/`ANDROID_SDK_ROOT`, macOS/Linux/Windows default SDK paths), `run_captured_with_timeout`, `run_passthrough`, `validate_package_name`, `is_readonly_mcp_shell`, and `is_dangerous_command`/`is_dangerous_shell_str`.
- [src/store.rs](src/store.rs): Persistent TOML/JSON config (`devices.toml`, `sessions.json`, `history.jsonl`, `repl_history`), atomic writes via `atomic_write` (PID + `AtomicUsize`), 7-day session expiration, alias/group resolution, and `ADBX_CONFIG_DIR` override.
- [src/session.rs](src/session.rs): Per-terminal session ID (`WT_SESSION`, `TERM_SESSION_ID`, `TMUX_PANE`, `WEZTERM_PANE`, `VSCODE_IPC_HOOK_CLI`, Unix TTY/PPID fallback).
- [src/selector.rs](src/selector.rs): TTY detection (`is_interactive_tty`), `FuzzySelect`/`MultiSelect` device pickers, and dangerous command confirmations (fail closed in non-TTY).
- [src/broadcast.rs](src/broadcast.rs): Parallel multi-device execution (`Grouped`, `Stream`, `--json`), hard timeouts, dangerous-command batch prompt, and automatic aligned `[label]` per-line prefixes when stdout is piped (`!stdout.is_terminal()`).
- [src/repl.rs](src/repl.rs): Enhanced interactive `adbx shell` / `adbx repl` built on `rustyline` (`ReplHelper` ghost-text `Hinter`, remote `ls` `Completer` with 3s cache, `Highlighter`), virtual `cd`, `:use`/`:raw`/`:clear`, `su` `#` elevation (`RootMode`), dangerous command confirmation, and PTY auto-allocation (`vi`/`vim`/`top` without `-n`).
- [src/completion.rs](src/completion.rs): `clap_complete` dynamic `ArgValueCompleter`s (`complete_devices`, `complete_packages`, `complete_shell_args` for remote paths like `adbx --device pixel shell cat /proc/asound/`).
- [src/cache.rs](src/cache.rs): Fast 5s-TTL completion cache (`ADBX_CACHE_DIR` override).
- [src/logcat.rs](src/logcat.rs), [src/logcat_snapshot.rs](src/logcat_snapshot.rs), [src/highlight.rs](src/highlight.rs): Shared `threadtime` parser (`parse_threadtime_raw`), Ratatui auto-reconnecting Logcat TUI, real-time ANSI colorized `adbx logcat` streaming, non-interactive `adbx log --tail` snapshots, and VS Code-style + `[log_highlights]` semantic coloring.
- [src/watch.rs](src/watch.rs): Ratatui multi-device monitor connecting to `127.0.0.1:${ANDROID_ADB_SERVER_PORT:-5037}` (`host:track-devices`).
- [src/reboot.rs](src/reboot.rs): Smart `adbx reboot` (`rr`) restoring `root`/`remount` state and launching enhanced shell.
- [src/wifi.rs](src/wifi.rs), [src/port.rs](src/port.rs), [src/history.rs](src/history.rs), [src/palette.rs](src/palette.rs), [src/config_cmd.rs](src/config_cmd.rs): Wi-Fi ADB, port presets, JSONL history + `.sh`/`.json` export, fuzzy command palette, and config lifecycle (`parse_editor_command`).
- [src/mcp.rs](src/mcp.rs): Native stdio JSON-RPC 2.0 MCP server (`adbx mcp`, safe read-only allowlist by default, `--allow-destructive` opt-in).

## Critical Invariants & Conventions

1. **CLI Flag Sync**: `extract_leading_target()` in `src/main.rs` parses `--all`, `--pick`, `--device`, `--group`, `--target`, and `@...` before `Cli::parse()`. Keep the top-level `struct Cli` flags in sync so `adbx --help` and `CompleteEnv` (`clap_complete`) stay accurate.
2. **TTY vs Pipe Discipline**:
   - Interactive pickers and confirmations must guard on `selector::is_interactive_tty()` and fail with actionable guidance in non-TTY environments.
   - Piped multi-device broadcast output (`!stdout.is_terminal()`) must emit clean uncolored `[label] ...` prefixes on stdout and send summaries/errors to stderr.
   - `adbx logcat` colorization must only activate when stdout is a TTY and non-streaming flags (`-c`, `-g`, `-S`, `-f`, `-B`, `--no-color`) are absent.
3. **Security Boundaries**:
   - Always validate package names via `adb::validate_package_name()` before interpolating into `adb shell` commands.
   - Keep `adb::is_readonly_mcp_shell()` as a strict allowlist (never replace with a denylist) and reject shell chaining characters (`; | & $ \` > < \n \r`).
   - Quote remote shell arguments with single quotes (`'...'` escaping `'`).
4. **Cross-Platform Behavior**:
   - Support macOS, Linux, and Windows (`#cfg(unix)` / `#cfg(windows)`). Use `std::env::join_paths` for `PATH`, `ADBX_CONFIG_DIR`/`ADBX_CACHE_DIR` in tests, and avoid Unix-only assumptions in core modules.
5. **Preserve Existing Comments & Docs**: Keep existing Chinese/English comments and docstrings intact unless directly modifying the documented logic.

## Packaging & Release

- CI workflows: [.github/workflows/ci.yml](.github/workflows/ci.yml) (macOS, Ubuntu, Windows) and [.github/workflows/release.yml](.github/workflows/release.yml) (5-target archives, `.deb` packages, `SHA256SUMS.txt`, Homebrew `adbx.rb`, Scoop `adbx.json`, Arch `PKGBUILD`).
- Distribution templates & installers: [Formula/adbx.rb](Formula/adbx.rb), [bucket/adbx.json](bucket/adbx.json), [packaging/aur/PKGBUILD](packaging/aur/PKGBUILD), [scripts/install.sh](scripts/install.sh), [scripts/install.ps1](scripts/install.ps1).
- When cutting a release: bump `version` in `Cargo.toml`, verify `clippy` + `test`, push `git tag -a vX.Y.Z -m "Release vX.Y.Z"` + `git push origin vX.Y.Z`, and sync the generated SHA256 manifests from the GitHub Release back into the repo.
