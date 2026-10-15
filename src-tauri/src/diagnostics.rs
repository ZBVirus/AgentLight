//! The on-disk diagnostics log and the "Copy diagnostics" report.
//!
//! The widget is a tray app: a user cannot see `stderr`, so failures left no
//! trace. Every shell-level event worth debuggging is appended to
//! `<config dir>/agentlight.log` (rotated once per startup when oversized), and
//! the Settings "Copy diagnostics" action puts a redacted summary plus the log
//! tail on the clipboard.
//!
//! No secrets ever reach either: tokens are reported as set/not set only.

use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Rotate the log once it passes this size at startup, so a long-lived widget
/// stays bounded.
const MAX_LOG_BYTES: u64 = 512 * 1024;
/// How many trailing log lines a diagnostics report carries.
const TAIL_LINES: usize = 40;
/// How much of the log tail is read from disk before splitting lines.
const TAIL_BYTES: u64 = 256 * 1024;

static LOG_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

fn log_path() -> &'static Mutex<Option<PathBuf>> {
    LOG_PATH.get_or_init(|| Mutex::new(None))
}

/// Point the logger at `path`. An oversized existing file is moved aside to
/// `<name>.1` first, so the new session starts a fresh file.
pub fn init(path: PathBuf) {
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > MAX_LOG_BYTES) {
        let rotated = path.with_extension("log.1");
        let _ = std::fs::remove_file(&rotated);
        let _ = std::fs::rename(&path, rotated);
    }
    *log_path().lock().unwrap() = Some(path);
}

/// Append one timestamped line to the log file (when initialized) and mirror
/// it to stderr. Failures are swallowed: logging must never affect behavior.
pub fn line(level: &str, message: &str) {
    eprintln!("agentlight [{level}] {message}");
    let path = log_path().lock().unwrap().clone();
    let Some(path) = path else {
        return;
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{stamp} [{level}] {message}");
    }
}

/// The last [`TAIL_LINES`] lines of `path`, oldest first. A missing or
/// unreadable file yields an empty tail.
pub fn tail(path: &Path) -> Vec<String> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if len > TAIL_BYTES && file.seek(SeekFrom::Start(len - TAIL_BYTES)).is_err() {
        return Vec::new();
    }
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.lines().collect();
    if (len > TAIL_BYTES) && !lines.is_empty() {
        // The first line is likely cut mid-way; drop it.
        lines.remove(0);
    }
    lines
        .iter()
        .rev()
        .take(TAIL_LINES)
        .rev()
        .map(|line| line.to_string())
        .collect()
}

/// Everything the diagnostics report shows. Only redacted summaries are stored
/// here; the renderer is pure, so the report shape is unit-testable.
pub struct Diagnostics {
    pub app: String,
    pub os: String,
    pub config_path: String,
    pub source: String,
    pub settings: String,
    pub snapshot: String,
    pub server: String,
    pub hub: String,
    pub log_path: String,
    pub log_tail: Vec<String>,
}

impl Diagnostics {
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "AgentLight diagnostics");
        let _ = writeln!(
            out,
            "generated: {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        let _ = writeln!(out, "app:       {}", self.app);
        let _ = writeln!(out, "os:        {}", self.os);
        let _ = writeln!(out, "config:    {}", self.config_path);
        let _ = writeln!(out, "source:    {}", self.source);
        let _ = writeln!(out, "settings:  {}", self.settings);
        let _ = writeln!(out, "snapshot:  {}", self.snapshot);
        let _ = writeln!(out, "server:    {}", self.server);
        let _ = writeln!(out, "hub:       {}", self.hub);
        let _ = writeln!(out, "log:       {}", self.log_path);
        let _ = writeln!(out, "----------- log tail -----------");
        if self.log_tail.is_empty() {
            let _ = writeln!(out, "(no log lines yet)");
        } else {
            for line in &self.log_tail {
                let _ = writeln!(out, "{line}");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let unique = format!(
            "agentlight-log-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }

    /// One test exercises the global logger so parallel tests cannot race on it.
    #[test]
    fn log_writes_rotates_and_tails() {
        let path = temp_path("agentlight.log");
        init(path.clone());
        line("info", "first line");
        line("error", "second line");
        let lines = tail(&path);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("[info] first line"), "{lines:?}");
        assert!(lines[1].contains("[error] second line"), "{lines:?}");

        // An oversized file is rotated to `.1` at init and the session starts
        // a fresh log; the old content is not lost.
        std::fs::write(&path, vec![b'x'; (MAX_LOG_BYTES + 1) as usize]).unwrap();
        init(path.clone());
        assert!(path.with_extension("log.1").exists(), "rotated file exists");
        assert!(
            tail(&path).is_empty(),
            "the new session starts empty: {:?}",
            tail(&path)
        );
        line("info", "after rotation");
        let lines = tail(&path);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("after rotation"), "{lines:?}");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("log.1"));
    }

    #[test]
    fn render_includes_every_section_and_the_tail() {
        let report = Diagnostics {
            app: "0.5.0 · test".to_string(),
            os: "windows x86_64".to_string(),
            config_path: "C:\\cfg\\config.json".to_string(),
            source: "hub http://127.0.0.1:8787 (token set: no)".to_string(),
            settings: "always_on_top=true notifications=false".to_string(),
            snapshot: "ok=true aggregate=green".to_string(),
            server: "stopped".to_string(),
            hub: "reachable · version 0.4.0 · schema 1".to_string(),
            log_path: "C:\\cfg\\agentlight.log".to_string(),
            log_tail: vec!["2026-01-01 00:00:00 [info] hello".to_string()],
        }
        .render();
        for needle in [
            "AgentLight diagnostics",
            "app:       0.5.0 · test",
            "os:        windows x86_64",
            "config:    C:\\cfg\\config.json",
            "source:    hub",
            "settings:  always_on_top=true",
            "snapshot:  ok=true",
            "server:    stopped",
            "hub:       reachable",
            "log:       C:\\cfg\\agentlight.log",
            "hello",
        ] {
            assert!(report.contains(needle), "missing {needle:?} in\n{report}");
        }
    }
}
