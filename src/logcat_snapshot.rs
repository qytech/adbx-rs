use crate::adb;
use crate::logcat::{parse_threadtime_raw, LogLevel};
use anyhow::Result;
use regex::RegexBuilder;
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotLine {
    pub timestamp: String,
    pub pid: String,
    pub level: String,
    pub tag: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct SnapshotResult {
    pub serial: String,
    pub package: Option<String>,
    pub matched_pids: Vec<String>,
    pub total_matched: usize,
    pub returned: usize,
    pub truncated: bool,
    pub lines: Vec<SnapshotLine>,
}

pub fn fetch_snapshot(
    adb_path: &PathBuf,
    serial: &str,
    package: Option<&str>,
    min_level: LogLevel,
    include_pattern: &str,
    exclude_pattern: &str,
    max_lines: usize,
) -> Result<SnapshotResult> {
    let clean_pkg = match package.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(p) => Some(adb::validate_package_name(p)?),
        None => None,
    };

    let pids: HashSet<String> = match clean_pkg {
        Some(pkg) => fetch_pids(adb_path, serial, pkg),
        None => HashSet::new(),
    };

    if clean_pkg.is_some() && pids.is_empty() {
        return Ok(SnapshotResult {
            serial: serial.to_string(),
            package: clean_pkg.map(|s| s.to_string()),
            matched_pids: Vec::new(),
            total_matched: 0,
            returned: 0,
            truncated: false,
            lines: Vec::new(),
        });
    }

    let include_re = compile_re(include_pattern);
    let exclude_re = compile_re(exclude_pattern);

    let capture = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["logcat".into(), "-d".into(), "-v".into(), "threadtime".into()],
        10,
    )?;

    let text = String::from_utf8_lossy(&capture.stdout);
    let mut matched = Vec::new();

    for raw in text.lines() {
        let p = parse_threadtime_raw(raw);
        if p.level < min_level {
            continue;
        }
        if clean_pkg.is_some() && !pids.contains(&p.pid) {
            continue;
        }
        if let Some(re) = &include_re {
            if !re.is_match(raw) {
                continue;
            }
        }
        if let Some(re) = &exclude_re {
            if re.is_match(raw) {
                continue;
            }
        }
        matched.push(SnapshotLine {
            timestamp: p.timestamp,
            pid: p.pid,
            level: p.level.code().to_string(),
            tag: p.tag,
            message: p.message,
        });
    }

    let total_matched = matched.len();
    let limit = max_lines.clamp(1, 500);
    let truncated = total_matched > limit;
    let lines = if truncated {
        matched.split_off(total_matched - limit)
    } else {
        matched
    };

    Ok(SnapshotResult {
        serial: serial.to_string(),
        package: clean_pkg.map(|s| s.to_string()),
        matched_pids: pids.into_iter().collect(),
        total_matched,
        returned: lines.len(),
        truncated,
        lines,
    })
}

fn compile_re(pattern: &str) -> Option<regex::Regex> {
    let t = pattern.trim();
    if t.is_empty() {
        return None;
    }
    RegexBuilder::new(t).case_insensitive(true).build().ok()
}

fn fetch_pids(adb_path: &PathBuf, serial: &str, pkg: &str) -> HashSet<String> {
    let Ok(cap) = adb::run_captured_with_timeout(
        adb_path,
        Some(serial),
        &["shell".into(), "pidof".into(), pkg.into()],
        3,
    ) else {
        return HashSet::new();
    };
    if cap.exit_code != 0 {
        return HashSet::new();
    }
    String::from_utf8_lossy(&cap.stdout)
        .split_whitespace()
        .filter(|s| s.chars().all(|c| c.is_ascii_digit()))
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_parser_handles_threadtime_and_raw() {
        let p = parse_threadtime_raw("11-26 22:15:01.123  4321  4355 E OkHttp  : timeout on /api/v1");
        assert_eq!(p.level, LogLevel::Error);
        assert_eq!(p.pid, "4321");
        assert_eq!(p.tag, "OkHttp");
        assert_eq!(p.message, "timeout on /api/v1");

        let r = parse_threadtime_raw("--------- beginning of main");
        assert_eq!(r.tag, "RAW");
    }
}
