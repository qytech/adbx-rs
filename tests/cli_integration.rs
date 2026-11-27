use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

struct TestHarness {
    root: PathBuf,
    bin_dir: PathBuf,
    home_dir: PathBuf,
    xdg_config: PathBuf,
    xdg_cache: PathBuf,
    adb_log: PathBuf,
}

impl TestHarness {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("adbx-e2e-{}-{id}", std::process::id()));
        let bin_dir = root.join("bin");
        let home_dir = root.join("home");
        let xdg_config = root.join("xdg_config");
        let xdg_cache = root.join("xdg_cache");
        let adb_log = root.join("adb.log");

        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(home_dir.join("Library/Application Support")).unwrap();
        fs::create_dir_all(home_dir.join("Library/Caches")).unwrap();
        fs::create_dir_all(&xdg_config).unwrap();
        fs::create_dir_all(&xdg_cache).unwrap();

        let adb_name = if cfg!(windows) { "adb.cmd" } else { "adb" };
        let adb_script = bin_dir.join(adb_name);
        write_mock_adb(&adb_script, &adb_log);

        Self {
            root,
            bin_dir,
            home_dir,
            xdg_config,
            xdg_cache,
            adb_log,
        }
    }

    fn cmd(&self) -> Command {
        self.cmd_with_session("test-session-a")
    }

    fn cmd_with_session(&self, session_id: &str) -> Command {
        let mut paths = vec![self.bin_dir.clone()];
        if let Some(orig) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&orig));
        }
        let joined_path = std::env::join_paths(paths).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_adbx"));
        cmd.env("HOME", &self.home_dir)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_CACHE_HOME", &self.xdg_cache)
            .env("ADBX_CONFIG_DIR", self.xdg_config.join("adbx"))
            .env("ADBX_CACHE_DIR", self.xdg_cache.join("adbx"))
            .env("ADBX_DATA_DIR", self.root.join("data"))
            .env_remove("ADBX_TEST_ONLY_SECOND")
            .env_remove("ADBX_TEST_DEVICES_FILE")
            .env_remove("ADBX_TEST_FORWARDS")
            .env("APPDATA", &self.xdg_config)
            .env("LOCALAPPDATA", &self.xdg_cache)
            .env("PATH", joined_path)
            .env("WT_SESSION", session_id)
            .env("TERM", "xterm-256color")
            .env_remove("ADBX_DEVICE")
            .env_remove("TERM_SESSION_ID")
            .env_remove("TMUX_PANE")
            .env_remove("WEZTERM_PANE")
            .env_remove("VSCODE_IPC_HOOK_CLI")
            .env_remove("COMPLETE");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }

    fn read_adb_log(&self) -> String {
        fs::read_to_string(&self.adb_log).unwrap_or_default()
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
fn write_mock_adb(path: &Path, log_path: &Path) {
    let script = format!(
        r#"#!/bin/sh
LOG="{}"
printf '%s\n' "$*" >> "$LOG"

SERIAL=""
if [ "$1" = "-s" ]; then
    SERIAL="$2"
    shift 2
fi

SUB="$1"
shift 2>/dev/null || true

case "$SUB" in
    devices)
        printf 'List of devices attached\n'
        if [ -n "$ADBX_TEST_DEVICES_FILE" ]; then cat "$ADBX_TEST_DEVICES_FILE"; exit $?; fi
        if [ -z "$ADBX_TEST_ONLY_SECOND" ]; then
            printf 'DEV001                 device usb:1-1 product:pixel model:Pixel_8 device:shiba\n'
        fi
        printf 'DEV002                 device usb:1-2 product:cayin model:Cayin_DMP device:cayin_dmp\n'
        exit 0
        ;;
    version)
        printf 'Android Debug Bridge version 1.0.41\n'
        exit 0
        ;;
    mdns)
        printf 'List of discovered mdns services\n'
        printf 'adb-DEV001\t_adb-tls-connect._tcp\t192.168.1.50:5555\n'
        exit 0
        ;;
    connect)
        printf 'connected to %s\n' "$1"
        exit 0
        ;;
    tcpip)
        printf 'restarting in TCP mode port: %s\n' "$1"
        exit 0
        ;;
    reverse)
        if [ "$1" = "--list" ]; then
            printf 'UsbFfs tcp:8081 tcp:8081\n'
        fi
        exit 0
        ;;
    forward)
        if [ "$1" = "--list" ] && [ -n "$ADBX_TEST_FORWARDS" ]; then
            printf '%s\n' "$ADBX_TEST_FORWARDS"
        elif [ "$2" = "tcp:0" ]; then
            case "$SERIAL" in DEV001) echo 41001 ;; DEV002) echo 41002 ;; esac
        fi
        exit 0
        ;;
    push) cat "$1"; exit $? ;;
    pull)
        # The test fixture transfers run.log and writes device-specific contents.
        for dest do :; done
        if [ -d "$dest" ]; then dest="$dest/run.log"; fi
        printf '%s' "$SERIAL" > "$dest"
        exit $?
        ;;
    install)
        printf 'Performing Streamed Install\nSuccess\n'
        exit 0
        ;;
    reboot)
        exit 0
        ;;
    logcat)
        printf '11-26 22:15:01.100  4321  4355 V Trace   : verbose noise line\n'
        printf '11-26 22:15:01.110  4321  4355 D Net     : debug state=ready\n'
        printf '11-26 22:15:01.120  4321  4355 I App     : started\n'
        printf '11-26 22:15:01.130  4321  4355 W Audio   : underrun\n'
        printf '11-26 22:15:01.140  4321  4355 E OkHttp  : timeout on /api/v1\n'
        printf '11-26 22:15:01.150  9999  9999 E Other   : other process error\n'
        exit 0
        ;;
    shell)
        CMD="$*"
        case "$CMD" in
            "-tt uiautomator dump /dev/tty")
                printf '<hierarchy rotation="0"><node/></hierarchy>\nUI hierchary dumped to: /dev/tty\n'
                ;;
            *pidof*com.example.app*)
                printf '4321\n'
                ;;
            id|id\ *)
                printf 'uid=2000(shell) gid=2000(shell)\n'
                ;;
            *getprop*)
                printf '[ro.product.model]: [Model_%s]\n' "$SERIAL"
                ;;
            *ls*/proc/asound/*)
                printf 'card0/\ncard1/\ncards\n'
                ;;
            *ip*-f*inet*addr*)
                printf '12: wlan0: inet 192.168.1.88/24 brd 192.168.1.255\n'
                ;;
            *dumpsys*activity*top*)
                printf '  ACTIVITY com.example.app/.MainActivity 1234 pid=4321\n'
                ;;
            *monkey*)
                printf 'Events injected: 1\n'
                ;;
            *pm*clear*)
                printf 'Success\n'
                ;;
            *)
                printf 'mock:%s:%s\n' "$SERIAL" "$CMD"
                ;;
        esac
        exit 0
        ;;
    *)
        printf 'mock:%s:%s %s\n' "$SERIAL" "$SUB" "$*"
        exit 0
        ;;
esac
"#,
        log_path.display()
    );

    fs::write(path, script).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[cfg(windows)]
fn write_mock_adb(path: &Path, log_path: &Path) {
    let ps1_path = path.with_file_name("mock_adb.ps1");
    let ps1_script = format!(
        r#"$logPath = '{}'
Add-Content -Path $logPath -Value ($args -join ' ') -Encoding UTF8

$i = 0
$serial = ''
if ($args.Count -ge 2 -and $args[0] -eq '-s') {{
    $serial = $args[1]
    $i = 2
}}
if ($i -ge $args.Count) {{ exit 0 }}
$sub = $args[$i]
$rest = if ($i + 1 -lt $args.Count) {{ $args[($i + 1)..($args.Count - 1)] }} else {{ @() }}

switch ($sub) {{
    'devices' {{
        Write-Output 'List of devices attached'
        if ($env:ADBX_TEST_DEVICES_FILE) {{ Get-Content -LiteralPath $env:ADBX_TEST_DEVICES_FILE; exit 0 }}
        if (-not $env:ADBX_TEST_ONLY_SECOND) {{
            Write-Output 'DEV001                 device usb:1-1 product:pixel model:Pixel_8 device:shiba'
        }}
        Write-Output 'DEV002                 device usb:1-2 product:cayin model:Cayin_DMP device:cayin_dmp'
        exit 0
    }}
    'version' {{
        Write-Output 'Android Debug Bridge version 1.0.41'
        exit 0
    }}
    'mdns' {{
        Write-Output 'List of discovered mdns services'
        Write-Output "adb-DEV001`t_adb-tls-connect._tcp`t192.168.1.50:5555"
        exit 0
    }}
    'connect' {{
        Write-Output ("connected to " + ($rest -join ' '))
        exit 0
    }}
    'tcpip' {{
        Write-Output ("restarting in TCP mode port: " + ($rest -join ' '))
        exit 0
    }}
    'reverse' {{
        if ($rest.Count -ge 1 -and $rest[0] -eq '--list') {{
            Write-Output 'UsbFfs tcp:8081 tcp:8081'
        }}
        exit 0
    }}
    'forward' {{
        if ($args -contains '--list' -and $env:ADBX_TEST_FORWARDS) {{ Write-Output $env:ADBX_TEST_FORWARDS }}
        elseif ($args -contains 'tcp:0') {{
            if ($serial -eq 'DEV001') {{ Write-Output '41001' }} else {{ Write-Output '41002' }}
        }}
        exit 0
    }}
    'push' {{ Get-Content -LiteralPath $rest[0]; exit 0 }}
    'pull' {{
        $dest = $rest[-1]
        if (Test-Path -LiteralPath $dest -PathType Container) {{ $dest = Join-Path $dest 'run.log' }}
        [System.IO.File]::WriteAllText([System.IO.Path]::GetFullPath($dest), $serial)
        exit 0
    }}
    'install' {{
        Write-Output 'Performing Streamed Install'
        Write-Output 'Success'
        exit 0
    }}
    'reboot' {{ exit 0 }}
    'logcat' {{
        Write-Output '11-26 22:15:01.100  4321  4355 V Trace   : verbose noise line'
        Write-Output '11-26 22:15:01.110  4321  4355 D Net     : debug state=ready'
        Write-Output '11-26 22:15:01.120  4321  4355 I App     : started'
        Write-Output '11-26 22:15:01.130  4321  4355 W Audio   : underrun'
        Write-Output '11-26 22:15:01.140  4321  4355 E OkHttp  : timeout on /api/v1'
        Write-Output '11-26 22:15:01.150  9999  9999 E Other   : other process error'
        exit 0
    }}
    'shell' {{
        $cmd = $rest -join ' '
        if ($cmd -eq '-tt uiautomator dump /dev/tty') {{ Write-Output '<hierarchy rotation="0"><node/></hierarchy>' }}
        elseif ($cmd -like '*pidof*com.example.app*') {{ Write-Output '4321' }}
        elseif ($cmd -match '^id(\s|$)') {{ Write-Output 'uid=2000(shell) gid=2000(shell)' }}
        elseif ($cmd -like '*getprop*') {{ Write-Output "[ro.product.model]: [Model_$serial]" }}
        elseif ($cmd -like '*ls*/proc/asound/*') {{
            Write-Output 'card0/'
            Write-Output 'card1/'
            Write-Output 'cards'
        }}
        elseif ($cmd -like '*ip*-f*inet*addr*') {{ Write-Output '12: wlan0: inet 192.168.1.88/24 brd 192.168.1.255' }}
        elseif ($cmd -like '*dumpsys*activity*top*') {{ Write-Output '  ACTIVITY com.example.app/.MainActivity 1234 pid=4321' }}
        elseif ($cmd -like '*monkey*') {{ Write-Output 'Events injected: 1' }}
        elseif ($cmd -like '*pm*clear*') {{ Write-Output 'Success' }}
        else {{ Write-Output "mock:${{serial}}:${{cmd}}" }}
        exit 0
    }}
    default {{
        Write-Output ("mock:${{serial}}:${{sub}} " + ($rest -join ' '))
        exit 0
    }}
}}
"#,
        log_path.display().to_string().replace('\'', "''")
    );
    fs::write(&ps1_path, ps1_script).unwrap();

    let cmd_shim = "@echo off\r\npowershell -NoProfile -ExecutionPolicy Bypass -File \"%~dp0mock_adb.ps1\" %*\r\n";
    fs::write(path, cmd_shim).unwrap();
}

#[test]
fn devices_list_json_and_long() {
    let h = TestHarness::new();

    let out = h.run(&["devices"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("DEV001") && stdout.contains("Pixel_8"));
    assert!(stdout.contains("DEV002") && stdout.contains("Cayin_DMP"));

    let json_out = h.run(&["devices", "--json"]);
    assert!(json_out.status.success());
    let parsed: Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 2);

    let long_out = h.run(&["devices", "-l"]);
    assert!(long_out.status.success());
    assert!(String::from_utf8_lossy(&long_out.stdout).contains("product:pixel"));

    for (args, expected) in [
        (&["list"][..], &out),
        (&["list", "--json"][..], &json_out),
        (&["list", "-l"][..], &long_out),
        (&["--pick", "list", "--json"][..], &json_out),
        (&["@all", "--json", "devices"][..], &json_out),
    ] {
        let listing = h.run(args);
        assert!(listing.status.success(), "{args:?}");
        assert_eq!(listing.stdout, expected.stdout);
    }
    let calls = h.read_adb_log();
    assert_eq!(calls.trim_start_matches('\u{feff}').lines().collect::<Vec<_>>(), ["devices -l"; 8]);
}

#[test]
fn unknown_commands_fail_before_device_selection() {
    let h = TestHarness::new();
    for args in [
        &["ali"][..],
        &["--device", "DEV001", "ali"][..],
        &["--pick", "ali"][..],
        &["@all", "--jobs", "2", "ali"][..],
    ] {
        let out = h.run(args);
        assert!(!out.status.success(), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("adbx alias"), "{args:?}: {err}");
        assert!(out.stdout.is_empty());
    }
    let typo = h.run(&["no-such-adb-command", "-d"]);
    assert!(!typo.status.success());
    assert!(String::from_utf8_lossy(&typo.stderr).contains("adbx --help"));
    assert!(h.read_adb_log().is_empty());

    let native = h.run(&["--device", "DEV001", "exec-out", "pwd"]);
    assert!(native.status.success());
    assert!(h.read_adb_log().contains("-s DEV001 exec-out pwd"));

    let raw = h.run(&["--", "vendor-command"]);
    assert!(raw.status.success());
    assert!(h.read_adb_log().contains("vendor-command"));
}

#[test]
fn alias_group_and_session_use_isolation() {
    let h = TestHarness::new();

    assert!(h.run(&["alias", "DEV001", "pixel"]).status.success());
    assert!(h.run(&["alias", "DEV002", "cayin"]).status.success());
    assert!(!h.run(&["alias", "DEV001", "all"]).status.success());

    assert!(h.run(&["group", "set", "qa", "pixel", "cayin"]).status.success());
    let grp_list = h.run(&["group", "list"]);
    assert!(grp_list.status.success());
    assert!(String::from_utf8_lossy(&grp_list.stdout).contains("qa"));

    assert!(h.run(&["use", "pixel"]).status.success());
    let cur_a = h.run(&["current", "--prompt"]);
    assert!(cur_a.status.success());
    assert_eq!(String::from_utf8_lossy(&cur_a.stdout), "[adb:pixel (DEV001)]");

    // 另一终端会话互不串机
    let cur_b = h.cmd_with_session("test-session-b")
        .args(["current"])
        .output()
        .unwrap();
    assert!(cur_b.status.success());
    assert!(String::from_utf8_lossy(&cur_b.stdout).contains("未锁定"));

    assert!(h.run(&["unuse"]).status.success());
    let after_unuse = h.run(&["current"]);
    assert!(String::from_utf8_lossy(&after_unuse.stdout).contains("未锁定"));

    assert!(h.run(&["group", "rm", "qa"]).status.success());
}

#[test]
fn offline_lock_and_remote_options_preserve_the_selected_serial() {
    let h = TestHarness::new();
    assert!(h.run(&["use", "DEV001"]).status.success());
    let offline = h
        .cmd()
        .env("ADBX_TEST_ONLY_SECOND", "1")
        .args(["shell", "getprop", "ro.product.model"])
        .output()
        .unwrap();
    assert!(!offline.status.success());
    assert!(String::from_utf8_lossy(&offline.stderr).contains("DEV001"));
    assert!(!h.read_adb_log().contains("shell getprop"));

    assert!(h
        .run(&["--device", "DEV002", "shell", "ls", "-d", "/system"])
        .status
        .success());
    assert!(h.read_adb_log().contains("-s DEV002 shell ls -d /system"));
    assert!(h.run(&["logcat", "-d"]).status.success());
    assert!(h.read_adb_log().contains("-s DEV001 logcat -d"));
}

#[test]
fn concurrent_terminals_keep_their_own_device_locks() {
    let h = TestHarness::new();
    let mut children = Vec::new();
    for i in 0..8 {
        children.push(
            h.cmd_with_session(&format!("terminal-{i}"))
                .args(["use", if i % 2 == 0 { "DEV001" } else { "DEV002" }])
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let state: Value =
        serde_json::from_slice(&fs::read(h.xdg_config.join("adbx/sessions.json")).unwrap())
            .unwrap();
    for i in 0..8 {
        assert_eq!(
            state["locked"][format!("WT_SESSION:terminal-{i}")]["serial"],
            if i % 2 == 0 { "DEV001" } else { "DEV002" }
        );
    }
}

#[test]
fn malformed_config_is_preserved_until_repaired() {
    let h = TestHarness::new();
    assert!(h.run(&["alias", "DEV001", "pixel"]).status.success());
    let path = h.xdg_config.join("adbx/devices.toml");
    let valid = fs::read_to_string(&path).unwrap();
    let broken = format!("{valid}\n[unclosed");
    fs::write(&path, &broken).unwrap();
    let out = h.run(&["group", "set", "qa", "DEV002"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("config edit"));
    assert_eq!(fs::read_to_string(&path).unwrap(), broken);
    assert!(h.run(&["config", "show"]).status.success());
    let backup = h.root.join("valid.toml");
    fs::write(&backup, &valid).unwrap();
    assert!(h
        .run(&["config", "import", backup.to_str().unwrap()])
        .status
        .success());
    assert_eq!(fs::read_to_string(path).unwrap(), valid);
}

#[test]
fn multi_device_broadcast_flags_and_piped_prefix() {
    let h = TestHarness::new();
    assert!(h.run(&["alias", "DEV001", "pixel"]).status.success());
    assert!(h.run(&["alias", "DEV002", "cayin"]).status.success());
    assert!(h.run(&["group", "set", "qa", "pixel", "cayin"]).status.success());
    assert!(h.run(&["use", "pixel"]).status.success());

    // 锁定单台后，普通命令仍只发给 pixel
    let single = h.run(&["shell", "getprop", "ro.product.model"]);
    assert!(single.status.success());
    let single_txt = String::from_utf8_lossy(&single.stdout);
    assert!(single_txt.contains("Model_DEV001"));
    assert!(!single_txt.contains("Model_DEV002"));

    // `--all` 覆盖单机锁定，且在管道/非 TTY 下每行带 `[设备名]` 前缀
    let all_out = h.run(&["--all", "shell", "getprop", "ro.product.model"]);
    assert!(all_out.status.success());
    let all_txt = String::from_utf8_lossy(&all_out.stdout);
    assert!(all_txt.contains("[pixel] [ro.product.model]: [Model_DEV001]"));
    assert!(all_txt.contains("[cayin] [ro.product.model]: [Model_DEV002]"));

    // `@all` 向下兼容与 `--group` / `--device` / `--json`
    assert!(h.run(&["@all", "shell", "id"]).status.success());
    let grp_json = h.run(&["--group", "qa", "--json", "shell", "getprop", "ro.product.model"]);
    assert!(grp_json.status.success());
    let parsed: Value = serde_json::from_slice(&grp_json.stdout).unwrap();
    assert_eq!(parsed["summary"]["ok"], 2);

    let dev_out = h.run(&["--device", "cayin", "shell", "getprop", "ro.product.model"]);
    assert!(dev_out.status.success());
    assert!(String::from_utf8_lossy(&dev_out.stdout).contains("Model_DEV002"));
}

#[test]
fn shell_command_escape_hatch_and_dynamic_completion() {
    let h = TestHarness::new();
    assert!(h.run(&["alias", "DEV001", "pixel"]).status.success());
    assert!(h.run(&["use", "pixel"]).status.success());

    let help = h.run(&["--help"]);
    assert!(help.status.success());
    let help_txt = String::from_utf8_lossy(&help.stdout);
    assert!(help_txt.contains("--all") && help_txt.contains("--device"));

    let alias_completion = h.cmd()
        .env("COMPLETE", "zsh")
        .env("_CLAP_COMPLETE_INDEX", "1")
        .args(["--", "adbx", "ali"])
        .output()
        .unwrap();
    assert!(alias_completion.status.success());
    assert!(String::from_utf8_lossy(&alias_completion.stdout).starts_with("alias"));

    let ver = h.run(&["--", "version"]);
    assert!(ver.status.success());
    assert!(String::from_utf8_lossy(&ver.stdout).contains("1.0.41"));

    let comp_script = h.run(&["completions", "zsh"]);
    assert!(comp_script.status.success());
    assert!(String::from_utf8_lossy(&comp_script.stdout).contains("compdef"));

    // 动态补全：命令名列表
    let cmd_comp = h
        .cmd()
        .env("COMPLETE", "zsh")
        .env("_CLAP_COMPLETE_INDEX", "2")
        .args(["--", "adbx", "shell", ""])
        .output()
        .unwrap();
    assert!(cmd_comp.status.success());
    let cmd_txt = String::from_utf8_lossy(&cmd_comp.stdout);
    assert!(cmd_txt.contains("cat") && cmd_txt.contains("getprop"));

    // 动态补全：远端路径 `/proc/asound/`（通过会话锁定）
    let path_comp = h
        .cmd()
        .env("COMPLETE", "zsh")
        .env("_CLAP_COMPLETE_INDEX", "3")
        .args(["--", "adbx", "shell", "cat", "/proc/asound/"])
        .output()
        .unwrap();
    assert!(path_comp.status.success());
    let path_txt = String::from_utf8_lossy(&path_comp.stdout);
    assert!(path_txt.contains("/proc/asound/card0/"));
    assert!(path_txt.contains("/proc/asound/cards"));

    // 动态补全：未锁定会话时直接传 `--device pixel`
    let dev_path_comp = h
        .cmd_with_session("test-session-no-lock")
        .env("COMPLETE", "zsh")
        .env("_CLAP_COMPLETE_INDEX", "5")
        .args(["--", "adbx", "--device", "pixel", "shell", "cat", "/proc/asound/"])
        .output()
        .unwrap();
    assert!(dev_path_comp.status.success());
    let dev_path_txt = String::from_utf8_lossy(&dev_path_comp.stdout);
    assert!(dev_path_txt.contains("/proc/asound/card0/"));
    assert!(dev_path_txt.contains("/proc/asound/cards"));
}

#[test]
fn log_snapshot_and_logcat_passthrough() {
    let h = TestHarness::new();
    assert!(h.run(&["use", "DEV001"]).status.success());

    let snap = h.run(&[
        "log",
        "--pkg",
        "com.example.app",
        "--level",
        "W",
        "--regex",
        "timeout|underrun",
        "--exclude",
        "noise",
        "--tail",
        "10",
        "--json",
    ]);
    assert!(snap.status.success());
    let parsed: Value = serde_json::from_slice(&snap.stdout).unwrap();
    assert_eq!(parsed["package"], "com.example.app");
    let lines = parsed["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["tag"], "Audio");
    assert_eq!(lines[1]["tag"], "OkHttp");

    let lc = h.run(&["logcat", "-d"]);
    assert!(lc.status.success());
    assert!(String::from_utf8_lossy(&lc.stdout).contains("OkHttp"));
}

#[test]
fn macro_and_quick_actions() {
    let h = TestHarness::new();
    assert!(h.run(&["use", "DEV001"]).status.success());

    assert!(h
        .run(&["macro", "set", "check-id", "shell", "id"])
        .status
        .success());
    let list = h.run(&["macro", "list"]);
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains("check-id"));

    let quick_custom = h.run(&["quick", "check-id"]);
    assert!(quick_custom.status.success());
    assert!(String::from_utf8_lossy(&quick_custom.stdout).contains("uid=2000"));

    let quick_top = h.run(&["quick", "top-activity"]);
    assert!(quick_top.status.success());
    assert!(String::from_utf8_lossy(&quick_top.stdout).contains("MainActivity"));

    assert!(h.run(&["macro", "rm", "check-id"]).status.success());
}

#[test]
fn config_lifecycle_and_validation() {
    let h = TestHarness::new();
    assert!(h.run(&["alias", "DEV001", "pixel"]).status.success());

    let paths = h.run(&["config", "path"]);
    assert!(paths.status.success());
    assert!(String::from_utf8_lossy(&paths.stdout).contains("devices.toml"));

    let exported = h.root.join("exported.toml");
    assert!(h
        .run(&["config", "export", exported.to_str().unwrap()])
        .status
        .success());
    assert!(fs::read_to_string(&exported).unwrap().contains("pixel"));

    let bad_toml = h.root.join("bad.toml");
    fs::write(&bad_toml, "invalid = [").unwrap();
    assert!(!h
        .run(&["config", "import", bad_toml.to_str().unwrap()])
        .status
        .success());

    assert!(h
        .run(&["config", "import", exported.to_str().unwrap()])
        .status
        .success());
    assert!(h.run(&["config", "reset", "-y"]).status.success());

    let show_after = h.run(&["config", "show"]);
    assert!(show_after.status.success());
    assert!(String::from_utf8_lossy(&show_after.stdout).contains("尚未生成"));
}

#[test]
fn multi_pull_isolates_files_and_rejects_collisions_before_transfer() {
    let h = TestHarness::new();
    let run = |args: &[&str]| h.cmd().current_dir(&h.root).args(args).output().unwrap();
    for (args, base) in [
        (&["--all", "pull", "/sdcard/run.log"][..], h.root.clone()),
        (
            &[
                "@all",
                "--json",
                "pull",
                "-a",
                "-z",
                "lz4",
                "/sdcard/run.log",
                "new dir/",
            ][..],
            h.root.join("new dir"),
        ),
    ] {
        let out = run(args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        for serial in ["DEV001", "DEV002"] {
            assert_eq!(
                fs::read_to_string(base.join(serial).join("run.log")).unwrap(),
                serial
            );
        }
    }
    let shared = h.root.join("one.log");
    fs::write(&shared, "keep").unwrap();
    let before = h
        .read_adb_log()
        .lines()
        .filter(|line| line.contains(" pull "))
        .count();
    for local_file in ["one.log", "missing.log"] {
        assert!(!run(&["--all", "pull", "/sdcard/run.log", local_file])
            .status
            .success());
    }
    assert_eq!(
        before,
        h.read_adb_log()
            .lines()
            .filter(|line| line.contains(" pull "))
            .count()
    );
    assert_eq!(fs::read_to_string(&shared).unwrap(), "keep");
    assert!(!h.root.join("missing.log").exists());

    assert!(run(&[
        "--device",
        "DEV001",
        "pull",
        "/sdcard/run.log",
        "single.log"
    ])
    .status
    .success());
    assert_eq!(
        fs::read_to_string(h.root.join("single.log")).unwrap(),
        "DEV001"
    );
}

#[test]
fn replay_uses_the_original_serials_even_after_alias_and_group_changes() {
    let h = TestHarness::new();
    assert!(h.run(&["alias", "DEV001", "pixel"]).status.success());
    assert!(h
        .run(&["--device", "pixel", "shell", "id"])
        .status
        .success());
    assert!(h
        .run(&["group", "set", "qa", "DEV001", "DEV002"])
        .status
        .success());
    assert!(h.run(&["--group", "qa", "shell", "id"]).status.success());
    assert!(h.run(&["alias", "DEV002", "pixel"]).status.success());
    assert!(h.run(&["group", "set", "qa", "DEV002"]).status.success());
    let listing = h.run(&["history", "list", "--json"]);
    let entries: Vec<Value> = serde_json::from_slice(&listing.stdout).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["target"], "pixel");
    assert_eq!(entries[0]["serials"], serde_json::json!(["DEV001"]));
    assert_eq!(
        entries[1]["serials"],
        serde_json::json!(["DEV001", "DEV002"])
    );

    let sh = h.root.join("commands.sh");
    assert!(h
        .run(&["history", "export", sh.to_str().unwrap()])
        .status
        .success());
    let script = fs::read_to_string(sh).unwrap();
    assert!(script.contains("adbx history replay --serial=DEV001 --serial=DEV002 -- shell id"));
    assert!(h.run(&["alias", "DEV002", "DEV001"]).status.success());
    assert!(h
        .run(&["history", "replay", "--serial", "DEV001", "--", "shell", "id"])
        .status
        .success());
    assert_eq!(h.read_adb_log().lines().last(), Some("-s DEV001 shell id"));
    let count = || {
        h.read_adb_log()
            .lines()
            .filter(|line| line.ends_with("shell id"))
            .count()
    };
    let before = count();
    let offline = h
        .cmd()
        .env("ADBX_TEST_ONLY_SECOND", "1")
        .args([
            "history", "replay", "--serial", "DEV002", "--serial", "DEV001", "--", "shell", "id",
        ])
        .output()
        .unwrap();
    assert!(!offline.status.success());
    assert!(String::from_utf8_lossy(&offline.stderr).contains("DEV001"));
    assert_eq!(count(), before);
    assert!(!h
        .run(&["history", "replay", "--serial", "DEV00", "--", "shell", "id"])
        .status
        .success());
    assert!(!h
        .run(&["history", "replay", "--", "shell", "id"])
        .status
        .success());
    assert_eq!(count(), before);
}

#[test]
fn session_locks_keep_their_kind_and_repl_never_re_resolves_fixed_serials() {
    let h = TestHarness::new();
    assert!(h.run(&["use", "DEV001"]).status.success());
    assert!(h
        .run(&["group", "set", "DEV001", "DEV002"])
        .status
        .success());
    assert!(h.run(&["shell", "id"]).status.success());
    assert_eq!(h.read_adb_log().lines().last(), Some("-s DEV001 shell id"));
    assert!(h.run(&["--device", "DEV001", "shell", "id"]).status.success());
    assert_eq!(h.read_adb_log().lines().last(), Some("-s DEV001 shell id"));
    assert!(h.run(&["--group", "DEV001", "shell", "id"]).status.success());
    assert_eq!(h.read_adb_log().lines().last(), Some("-s DEV002 shell id"));
    let json: Value = serde_json::from_slice(&h.run(&["list", "--json"]).stdout).unwrap();
    assert_eq!(json[0]["locked"], true);
    assert_eq!(json[1]["locked"], false);

    assert!(h.run(&["use", "--group", "DEV001"]).status.success());
    assert!(h.run(&["shell", "id"]).status.success());
    assert_eq!(h.read_adb_log().lines().last(), Some("-s DEV002 shell id"));
    assert!(h
        .run(&["group", "set", "DEV001", "DEV001"])
        .status
        .success());
    assert!(h.run(&["shell", "id"]).status.success());
    assert_eq!(h.read_adb_log().lines().last(), Some("-s DEV001 shell id"));
    assert!(h.run(&["group", "rm", "DEV001"]).status.success());
    assert!(
        !h.run(&["shell", "id"]).status.success(),
        "a deleted group must not turn into a serial"
    );

    assert!(h.run(&["use", "DEV001,DEV002"]).status.success());
    let devices = h.root.join("online");
    fs::write(&devices, "DEV001 device\nDEV002 device\n").unwrap();
    let mut child = h
        .cmd()
        .env("ADBX_TEST_DEVICES_FILE", &devices)
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stderr.take().unwrap());
    let mut banner = String::new();
    reader.read_line(&mut banner).unwrap();
    assert!(banner.contains("增强 Shell"), "{banner}");
    fs::write(&devices, "DEV001-NEW device\nDEV002 device\n").unwrap();
    writeln!(child.stdin.take().unwrap(), "echo fixed-only\nexit").unwrap();
    assert!(child.wait().unwrap().success());
    let mut errors = String::new();
    std::io::Read::read_to_string(&mut reader, &mut errors).unwrap();
    assert!(
        errors.contains("DEV001") && errors.contains("未在线"),
        "{errors}"
    );
    assert!(!h.read_adb_log().contains("fixed-only"));
}

#[test]
fn port_sync_preflights_host_listener_conflicts() {
    let h = TestHarness::new();
    assert!(h.run(&["use", "DEV001"]).status.success());
    assert!(h
        .run(&["port", "add", "reverse", "7001", "7001"])
        .status
        .success());
    assert!(h
        .run(&["port", "add", "forward", "8081", "7000"])
        .status
        .success());
    let before = h.read_adb_log();
    let out = h.run(&["port", "sync", "-t", "all"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("tcp:8081"));
    let after = h.read_adb_log();
    assert!(!after[before.len()..].contains(" reverse "));
    assert!(!after[before.len()..].contains(" forward "));
    assert!(h.run(&["port", "sync", "-t", "DEV002"]).status.success());
    assert!(h
        .read_adb_log()
        .contains("-s DEV002 forward --no-rebind tcp:8081 tcp:7000"));
    let before = h.read_adb_log();
    let busy = h
        .cmd()
        .env("ADBX_TEST_FORWARDS", "DEV001 tcp:8081 tcp:7000")
        .args(["port", "sync", "-t", "DEV002"])
        .output()
        .unwrap();
    assert!(!busy.status.success());
    let after = h.read_adb_log();
    assert!(!after[before.len()..].contains("--no-rebind"));
    assert!(!after[before.len()..].contains(" reverse "));

    assert!(h
        .run(&["port", "rm", "forward", "8081", "7000"])
        .status
        .success());
    assert!(h
        .run(&["port", "add", "forward", "0", "7000"])
        .status
        .success());
    let dynamic = h.run(&["port", "sync", "-t", "all"]);
    assert!(dynamic.status.success());
    let report = String::from_utf8_lossy(&dynamic.stdout);
    assert!(
        report.contains("tcp:41001") && report.contains("tcp:41002"),
        "{report}"
    );
}

#[test]
fn history_replays_relative_files_from_the_recorded_working_directory() {
    let h = TestHarness::new();
    let first = h.root.join("project one");
    let second = h.root.join("project two");
    for (dir, data) in [(&first, "alpha"), (&second, "beta")] {
        fs::create_dir(dir).unwrap();
        fs::write(dir.join("settings.txt"), data).unwrap();
    }
    let args = ["push", "settings.txt", "/sdcard/settings.txt"];
    assert!(h
        .cmd()
        .current_dir(&first)
        .args(["--all", "--grouped"])
        .args(args)
        .output()
        .unwrap()
        .status
        .success());
    let listing = h.run(&["history", "list", "--json"]);
    let entries: Vec<Value> = serde_json::from_slice(&listing.stdout).unwrap();
    let cwd = entries[0]["cwd"].as_str().unwrap();
    let script = h.root.join("replay.sh");
    assert!(h
        .run(&["history", "export", script.to_str().unwrap()])
        .status
        .success());
    assert!(fs::read_to_string(&script).unwrap().contains("--cwd="));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_adbx"), h.bin_dir.join("adbx")).unwrap();
        let env_source = h.cmd();
        let mut bash = Command::new("bash");
        for (key, value) in env_source.get_envs() {
            if let Some(value) = value {
                bash.env(key, value);
            } else {
                bash.env_remove(key);
            }
        }
        let out = bash.current_dir(&second).arg(&script).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let data = String::from_utf8_lossy(&out.stdout);
        assert!(
            data.contains("[DEV001] alpha") && data.contains("[DEV002] alpha"),
            "{data}"
        );
    }
    for targets in [vec!["DEV001"], vec!["DEV001", "DEV002"]] {
        let mut replay = h.cmd();
        replay
            .current_dir(&second)
            .args(["history", "replay", "--cwd", cwd]);
        for serial in targets {
            replay.args(["--serial", serial]);
        }
        let out = replay.arg("--").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("alpha"));
        assert!(!String::from_utf8_lossy(&out.stdout).contains("beta"));
    }
    fs::rename(&first, h.root.join("moved")).unwrap();
    let before = h.read_adb_log();
    let missing = h
        .cmd()
        .current_dir(&second)
        .args([
            "history", "replay", "--serial", "DEV001", "--cwd", cwd, "--",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains(cwd));
    assert_eq!(h.read_adb_log(), before);
}

#[test]
fn config_edit_reports_a_concurrent_write_and_preserves_both_versions() {
    let h = TestHarness::new();
    assert!(h.run(&["alias", "DEV001", "alpha"]).status.success());
    #[cfg(unix)]
    let (name, contents, editor) = ("editor.sh",
        "#!/bin/sh\n\"$ADBX_TEST_EXE\" alias DEV002 beta || exit 1\nprintf '[groups]\\nqa = [\"DEV001\"]\\n' > \"$1\"\n",
        "/bin/sh");
    #[cfg(windows)]
    let (name, contents, editor) = ("editor.ps1",
        "& $env:ADBX_TEST_EXE alias DEV002 beta\nif ($LASTEXITCODE -ne 0) { exit 1 }\n[IO.File]::WriteAllText($args[0], \"[groups]`nqa = ['DEV001']`n\")\n",
        "powershell -NoProfile -ExecutionPolicy Bypass -File");
    let path = h.root.join(name);
    fs::write(&path, contents).unwrap();
    let result = h
        .cmd()
        .env("VISUAL", format!("{editor} \"{}\"", path.display()))
        .env("ADBX_TEST_EXE", env!("CARGO_BIN_EXE_adbx"))
        .args(["config", "edit"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let err = String::from_utf8_lossy(&result.stderr);
    assert!(err.contains("草稿"), "{err}");
    let dir = h.xdg_config.join("adbx");
    assert!(fs::read_to_string(dir.join("devices.toml"))
        .unwrap()
        .contains("beta"));
    let draft = fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().starts_with("adbx-edit-"))
        .unwrap();
    assert!(fs::read_to_string(draft.path()).unwrap().contains("qa"));
}

#[test]
fn idle_piped_repl_exits_when_its_device_disconnects() {
    let h = TestHarness::new();
    let listing = h.root.join("online.txt");
    fs::write(&listing, "DEV001 device\nDEV002 device\n").unwrap();
    let mut child = h
        .cmd()
        .env("ADBX_TEST_DEVICES_FILE", &listing)
        .args(["--device", "DEV001", "repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut banner = String::new();
    stderr.read_line(&mut banner).unwrap();
    assert!(banner.contains("增强 Shell"), "{banner}");
    fs::write(&listing, "DEV002 device\n").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(1));
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("REPL waited for pipe EOF after disconnect");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
#[cfg(unix)]
fn redirected_repl_accepts_terminal_input_and_exits_cleanly_on_unplug() {
    use nix::fcntl::{fcntl, FcntlArg, OFlag};
    use nix::pty::{openpty, Winsize};
    use nix::sys::termios::tcgetattr;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    let h = TestHarness::new();
    let listing = h.root.join("devices");
    fs::write(&listing, "DEV001 device\nDEV002 device\n").unwrap();
    let size = Winsize {
        ws_row: 24,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let tty = openpty(Some(&size), None).unwrap();
    let slave = fs::File::from(tty.slave);
    let original = tcgetattr(&slave).unwrap().local_flags;
    let mut master = fs::File::from(tty.master);
    fcntl(master.as_raw_fd(), FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).unwrap();
    let mut child = h
        .cmd()
        .env("ADBX_TEST_DEVICES_FILE", &listing)
        .args(["--device", "DEV001", "repl"])
        .stdin(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut terminal = Vec::new();
    let mut sent = false;
    let mut unplugged = false;
    loop {
        let mut buf = [0; 2048];
        if let Ok(n) = master.read(&mut buf) {
            terminal.extend_from_slice(&buf[..n]);
        }
        let prompts = String::from_utf8_lossy(&terminal)
            .matches("[DEV001:/]")
            .count();
        if !sent && prompts > 0 {
            master.write_all(b"getprop ro.product.model\r").unwrap();
            sent = true;
        } else if !unplugged && prompts >= 2 {
            master.write_all(b"unfinished").unwrap();
            fs::write(&listing, "DEV002 device\n").unwrap();
            unplugged = true;
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("REPL did not exit: {}", String::from_utf8_lossy(&terminal));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    assert!(sent && unplugged, "{}", String::from_utf8_lossy(&terminal));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(tcgetattr(&slave).unwrap().local_flags, original);
    assert!(!out.stdout.contains(&0x1b));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Model_DEV001"));
    assert!(!h.read_adb_log().contains("unfinished"));
}

#[test]
fn history_record_search_export_and_clear() {
    let h = TestHarness::new();
    assert!(h.run(&["use", "DEV001"]).status.success());
    assert!(h.run(&["shell", "getprop", "ro.product.model"]).status.success());
    assert!(h.run(&["shell", "id"]).status.success());

    let hist_json = h.run(&["history", "list", "--json"]);
    assert!(hist_json.status.success());
    let arr: Value = serde_json::from_slice(&hist_json.stdout).unwrap();
    assert!(arr.as_array().unwrap().len() >= 2);

    let search = h.run(&["history", "list", "--grep", "getprop"]);
    assert!(search.status.success());
    assert!(String::from_utf8_lossy(&search.stdout).contains("ro.product.model"));

    let export_sh = h.root.join("hist.sh");
    assert!(h
        .run(&["history", "export", export_sh.to_str().unwrap()])
        .status
        .success());
    assert!(fs::read_to_string(&export_sh)
        .unwrap()
        .contains("adbx history replay --serial=DEV001 -- shell getprop"));

    let export_json = h.root.join("hist.json");
    assert!(h
        .run(&["history", "export", export_json.to_str().unwrap()])
        .status
        .success());
    let exported_arr: Value =
        serde_json::from_str(&fs::read_to_string(&export_json).unwrap()).unwrap();
    assert!(exported_arr.as_array().unwrap().len() >= 2);

    assert!(h.run(&["history", "clear", "-y"]).status.success());
    let after_clear = h.run(&["history", "list", "--json"]);
    let empty_arr: Value = serde_json::from_slice(&after_clear.stdout).unwrap();
    assert_eq!(empty_arr.as_array().unwrap().len(), 0);
}

#[test]
fn wifi_and_port_management() {
    let h = TestHarness::new();
    assert!(h.run(&["alias", "DEV001", "pixel"]).status.success());
    assert!(h.run(&["use", "pixel"]).status.success());

    let wifi_en = h.run(&["wifi", "enable"]);
    assert!(wifi_en.status.success());
    assert!(String::from_utf8_lossy(&wifi_en.stdout).contains("192.168.1.88:5555"));

    let wifi_ls = h.run(&["wifi", "list"]);
    assert!(wifi_ls.status.success());
    let ls_txt = String::from_utf8_lossy(&wifi_ls.stdout);
    assert!(ls_txt.contains("192.168.1.88:5555") && ls_txt.contains("_adb-tls-connect._tcp"));

    let wifi_re = h.run(&["wifi", "reconnect"]);
    assert!(wifi_re.status.success());
    assert!(String::from_utf8_lossy(&wifi_re.stdout).contains("[OK]"));

    assert!(h.run(&["port", "add", "reverse", "8081", "8081"]).status.success());
    let port_ls = h.run(&["port", "list"]);
    assert!(port_ls.status.success());
    assert!(String::from_utf8_lossy(&port_ls.stdout).contains("tcp:8081"));

    assert!(h.run(&["port", "sync", "-t", "all"]).status.success());
    assert!(h.run(&["port", "rm", "reverse", "8081", "8081"]).status.success());
    assert!(h.run(&["port", "clear"]).status.success());
}

#[test]
fn install_plus_and_reboot_bootloader() {
    let h = TestHarness::new();
    assert!(h.run(&["use", "DEV001"]).status.success());

    let apk = h.root.join("app.apk");
    fs::write(&apk, b"fake-apk").unwrap();

    let bad_apk = h.run(&["install+", "/nonexistent/app.apk"]);
    assert!(!bad_apk.status.success());

    let bad_pkg = h.run(&["install+", apk.to_str().unwrap(), "--launch", "bad;rm"]);
    assert!(!bad_pkg.status.success());

    let inst = h.run(&[
        "install+",
        apk.to_str().unwrap(),
        "--launch",
        "com.example.app",
        "-t",
        "DEV001",
    ]);
    assert!(inst.status.success());
    let log = h.read_adb_log();
    assert!(log.contains("install -r -d -g"));
    assert!(log.contains("monkey -p com.example.app"));

    let rb = h.run(&["reboot", "bootloader", "-y"]);
    assert!(rb.status.success());
    assert!(h.read_adb_log().contains("-s DEV001 reboot bootloader"));
}

#[test]
fn mcp_stdio_server_end_to_end() {
    let h = TestHarness::new();
    assert!(h.run(&["alias", "DEV001", "pixel"]).status.success());

    let mut child = h
        .cmd()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"adbx://devices"}}"#,
        r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"list_devices","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"get_logcat","arguments":{"device":"pixel","level":"E"}}}"#,
        r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"run_shell","arguments":{"device":"pixel","command":"getprop ro.product.model"}}}"#,
        r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"run_shell","arguments":{"device":"pixel","command":"rm -rf /sdcard/a"}}}"#,
        r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"app_action","arguments":{"device":"pixel","pkg":"com.example.app","action":"clear"}}}"#,
        r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"app_action","arguments":{"device":"pixel","pkg":"com.example.app","action":"stop"}}}"#,
        r#"not-json"#,
        r#"{"jsonrpc":"2.0","id":12,"method":"unknown/method"}"#,
        r#"{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"dump_ui_tree","arguments":{"device":"pixel"}}}"#,
    ];

    for req in requests {
        writeln!(stdin, "{req}").unwrap();
    }
    drop(stdin);

    let mut responses = Vec::new();
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap() > 0 {
        responses.push(serde_json::from_str::<Value>(&line).unwrap());
        line.clear();
    }
    let status = child.wait().unwrap();
    assert!(status.success());
    assert_eq!(responses.len(), 14);

    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "adbx");
    assert_eq!(responses[1]["result"], serde_json::json!({}));
    assert!(!responses[2]["result"]["tools"].as_array().unwrap().is_empty());
    assert_eq!(responses[3]["result"]["resources"][0]["uri"], "adbx://devices");
    assert!(responses[4]["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("DEV001"));
    assert_eq!(responses[5]["result"]["isError"], false);
    assert!(responses[6]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("OkHttp"));
    assert_eq!(responses[7]["result"]["isError"], false);
    assert_eq!(responses[8]["result"]["isError"], true);
    assert_eq!(responses[9]["result"]["isError"], true);
    assert_eq!(responses[10]["result"]["isError"], true);
    assert_eq!(responses[11]["error"]["code"], -32700);
    assert_eq!(responses[12]["error"]["code"], -32603);
    assert_eq!(responses[13]["result"]["isError"], false);
    assert_eq!(responses[13]["result"]["content"][0]["text"], r#"<hierarchy rotation="0"><node/></hierarchy>"#);
    assert!(!h.read_adb_log().contains("force-stop"));

    let mut full = h.cmd().args(["mcp", "--allow-destructive"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    writeln!(full.stdin.take().unwrap(), "{}", serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "ui_tap", "arguments": { "device": "pixel", "x": 1, "y": 2 } }
    })).unwrap();
    let out = full.wait_with_output().unwrap();
    assert!(out.status.success());
    let response: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(response["result"]["isError"], false);
    assert!(h.read_adb_log().contains("-s DEV001 shell input tap 1 2"));
}

#[test]
fn uninstall_dry_run_and_adb_apk_passthrough() {
    let h = TestHarness::new();

    // 裸命令 `adbx uninstall` 在非交互终端不直接触发自卸载，给出清晰提示
    let bare = h.run(&["uninstall"]);
    assert!(!bare.status.success());
    assert!(String::from_utf8_lossy(&bare.stderr).contains("adbx uninstall --self"));

    // 传入 --dry-run 但漏传 --self 时提示补全 `--self`
    let missing_self = h.run(&["uninstall", "--dry-run"]);
    assert!(!missing_self.status.success());
    assert!(String::from_utf8_lossy(&missing_self.stderr).contains("--self"));

    let preview = h.run(&["uninstall", "--self", "--dry-run"]);
    assert!(preview.status.success());
    let preview_txt = String::from_utf8_lossy(&preview.stdout);
    assert!(preview_txt.contains("检测到以下 adbx 安装"));
    assert!(preview_txt.contains("--dry-run 仅预览"));

    let alias_preview = h.run(&["self-uninstall", "--dry-run"]);
    assert!(alias_preview.status.success());
    assert!(String::from_utf8_lossy(&alias_preview.stdout).contains("检测到以下 adbx 安装"));

    // 非交互终端未传 --yes 时应安全拦截
    let blocked = h.run(&["uninstall", "--self"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("--yes"));

    // `adbx uninstall <包名>` 仍保持与原生 `adb uninstall` 兼容（命中非 TTY 高危命令保护）
    assert!(h.run(&["use", "DEV001"]).status.success());
    let apk_uninstall = h.run(&["uninstall", "com.example.app"]);
    assert!(!apk_uninstall.status.success());
    let apk_err = String::from_utf8_lossy(&apk_uninstall.stderr);
    assert!(apk_err.contains("高危命令") && apk_err.contains("adb uninstall com.example.app"));
}
