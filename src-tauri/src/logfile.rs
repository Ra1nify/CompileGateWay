//! Log persistence (design doc §10).
//!
//! - **Rolls by day**: the file name is `YYYY-MM-DD`, appended by timestamp within a day.
//! - **The day boundary follows the daemon host's local time** (so Windows uses
//!   `GetLocalTime` and Unix uses `localtime_r`, never UTC).
//! - **Keeps 7 days by default**, configurable, and prunes expired files during writes.
//!
//! One record per line, fields tab-separated and control characters escaped, so
//! "one record = one line" holds and `tail` / `grep` work directly.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::log::LogEntry;

pub const DIR_NAME: &str = "logs";
pub const DEFAULT_RETAIN_DAYS: u64 = 7;
/// Retention ceiling, so an absurd config value cannot fill the disk.
pub const MAX_RETAIN_DAYS: u64 = 3650;
/// Max entries returned when reading one day (taken from the tail).
pub const MAX_READ_ENTRIES: usize = 5000;

/// One persisted record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredEntry {
    pub ts_ms: u64,
    pub level: String,
    /// Source marker (design doc §10: `webui` / `cli` / `mcp` / `daemon`).
    pub source: String,
    /// Log scope (a client id or `job-xxx`).
    pub scope: String,
    pub text: String,
}

/// Encode one record as a line. Newlines / tabs in the text are escaped so one
/// record stays one line.
pub fn format_line(scope: &str, entry: &LogEntry) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}",
        entry.ts_ms,
        entry.level,
        escape(&entry.source),
        escape(scope),
        escape(&entry.text)
    )
}

/// Parse one line; `None` on bad format (tolerates hand edits or lines appended by
/// other tools).
pub fn parse_line(line: &str) -> Option<StoredEntry> {
    let mut parts = line.splitn(5, '\t');
    let ts_ms = parts.next()?.trim().parse::<u64>().ok()?;
    let level = parts.next()?.to_string();
    let source = unescape(parts.next()?);
    let scope = unescape(parts.next()?);
    let text = unescape(parts.next()?);
    Some(StoredEntry {
        ts_ms,
        level,
        source,
        scope,
        text,
    })
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            other => out.push(other),
        }
    }
    out
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

// ------------------------------------------------------------------ local date

/// Today's `YYYY-MM-DD` in local time.
pub fn local_day_now() -> String {
    let (year, month, day) = local_ymd_now();
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(windows)]
fn local_ymd_now() -> (i32, u32, u32) {
    // SYSTEMTIME's layout is a documented, stable Win32 ABI, so declaring it beats
    // pulling in a binding crate.
    #[repr(C)]
    #[derive(Default)]
    struct WinSystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(out: *mut WinSystemTime);
    }

    let mut now = WinSystemTime::default();
    unsafe { GetLocalTime(&mut now) };
    if now.year < 1980 {
        // Should not happen; if it does, fall back to UTC rather than name a file
        // 0000-00-00
        return utc_ymd_now();
    }
    (
        now.year as i32,
        (now.month as u32).clamp(1, 12),
        (now.day as u32).clamp(1, 31),
    )
}

#[cfg(unix)]
fn local_ymd_now() -> (i32, u32, u32) {
    // localtime_r hands back local time directly, with no UTC offset to compute and
    // daylight saving handled for free.
    let mut seconds = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&mut seconds, &mut tm).is_null() };
    if !ok {
        return utc_ymd_now();
    }
    (
        tm.tm_year + 1900,
        (tm.tm_mon + 1).clamp(1, 12) as u32,
        tm.tm_mday.clamp(1, 31) as u32,
    )
}

/// Render a unix millisecond timestamp as `YYYY-MM-DD HH:MM:SS UTC`.
///
/// Deliberately **UTC**: local time would need a timezone library or platform API
/// (this file already has a separate path for "what day is it"), and this stamp is
/// only for eyeballing ("when was this bundle exported"), not worth another layer.
pub fn stamp_utc(ms: u64) -> String {
    let seconds = (ms / 1000) as i64;
    let (year, month, day) = crate::zip::civil_from_days(seconds.div_euclid(86_400));
    let rest = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

#[cfg(not(any(windows, unix)))]
fn local_ymd_now() -> (i32, u32, u32) {
    utc_ymd_now()
}

/// Fallback when local time is unavailable (platforms that are neither Windows nor Unix).
fn utc_ymd_now() -> (i32, u32, u32) {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    crate::zip::civil_from_days(ms.div_euclid(1_000).div_euclid(86_400))
}

// ------------------------------------------------------------------ write

/// Log directory that rolls by day. One shared instance per process.
pub struct LogFile {
    dir: PathBuf,
    /// Which day is currently open.
    day: Mutex<String>,
    writer: Mutex<Option<File>>,
    retain_days: AtomicU64,
    /// Once a write has failed, stop re-reporting it every time (a read-only
    /// directory, say).
    broken: Mutex<bool>,
}

impl LogFile {
    pub fn new(dir: PathBuf, retain_days: u64) -> LogFile {
        LogFile {
            dir,
            day: Mutex::new(String::new()),
            writer: Mutex::new(None),
            retain_days: AtomicU64::new(clamp_days(retain_days)),
            broken: Mutex::new(false),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn retain_days(&self) -> u64 {
        self.retain_days.load(Ordering::Relaxed)
    }

    pub fn set_retain_days(&self, days: u64) {
        self.retain_days.store(clamp_days(days), Ordering::Relaxed);
        self.prune();
    }

    /// Append one record to **today's** log. Any IO error is swallowed (a failed log
    /// write must not take down the main flow), but the first failure is remembered
    /// and exposed through [`LogFile::failed`].
    pub fn append(&self, scope: &str, entry: &LogEntry) {
        let today = local_day_now();
        let rolled = crate::lock::lock(&self.day).as_str() != today;
        if self.write(&today, scope, entry).is_err() {
            *crate::lock::lock(&self.broken) = true;
            return;
        }
        // Prune expired files on the day roll; no external timer needed
        if rolled {
            self.prune();
        }
    }

    /// Write into a named day. Production never uses it; it exists so **tests can
    /// build history**, and as the entry point for importing old logs later.
    pub fn append_to_day(&self, day: &str, scope: &str, entry: &LogEntry) {
        if self.write(day, scope, entry).is_err() {
            *crate::lock::lock(&self.broken) = true;
        }
    }

    fn write(&self, day: &str, scope: &str, entry: &LogEntry) -> std::io::Result<()> {
        self.ensure_open(day)?;
        let line = format_line(scope, entry);
        let mut writer = crate::lock::lock(&self.writer);
        if let Some(file) = writer.as_mut() {
            file.write_all(line.as_bytes())?;
            file.write_all(b"\n")?;
            // Flush so a crash mid-build does not lose the whole day's log
            file.flush()?;
        }
        Ok(())
    }

    /// Make sure the open file is `day` (switch to it if not).
    fn ensure_open(&self, day: &str) -> std::io::Result<()> {
        if crate::lock::lock(&self.day).as_str() == day && crate::lock::lock(&self.writer).is_some() {
            return Ok(());
        }
        self.roll(day)
    }

    /// Switch to the given day (tests call this directly too).
    pub fn roll(&self, day: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path_of(day);
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        *crate::lock::lock(&self.writer) = Some(file);
        *crate::lock::lock(&self.day) = day.to_string();
        *crate::lock::lock(&self.broken) = false;
        Ok(())
    }

    pub fn failed(&self) -> bool {
        *crate::lock::lock(&self.broken)
    }

    pub fn path_of(&self, day: &str) -> PathBuf {
        self.dir.join(format!("{day}.log"))
    }

    /// Delete files beyond the retention window. Anything not named
    /// `YYYY-MM-DD.log` is left alone.
    pub fn prune(&self) -> Vec<String> {
        let keep = self.retain_days() as usize;
        let days = self.days();
        let mut removed = Vec::new();
        for day in days.into_iter().skip(keep) {
            let path = self.path_of(&day);
            let is_today = crate::lock::lock(&self.day).as_str() == day;
            if is_today {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                removed.push(day);
            }
        }
        removed
    }

    /// Dates present on disk, newest first.
    pub fn days(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut days: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                let stem = name.strip_suffix(".log")?;
                if is_day_name(stem) {
                    Some(stem.to_string())
                } else {
                    None
                }
            })
            .collect();
        days.sort();
        days.reverse();
        days
    }

    /// Read the last `limit` entries of one day (defaults to [`MAX_READ_ENTRIES`]).
    pub fn read_day(&self, day: &str, limit: usize) -> std::io::Result<Vec<StoredEntry>> {
        if !is_day_name(day) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                crate::i18n::logfile_bad_date(crate::i18n::current(), day),
            ));
        }
        let file = File::open(self.path_of(day))?;
        let limit = if limit == 0 { MAX_READ_ENTRIES } else { limit };

        // A day can hold hundreds of thousands of lines; a ring buffer holds the tail
        // without reading the whole file into memory
        let mut ring: std::collections::VecDeque<StoredEntry> = std::collections::VecDeque::new();
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { continue };
            let Some(entry) = parse_line(&line) else {
                continue;
            };
            if ring.len() == limit {
                ring.pop_front();
            }
            ring.push_back(entry);
        }
        Ok(ring.into_iter().collect())
    }
}

pub fn clamp_days(days: u64) -> u64 {
    days.clamp(1, MAX_RETAIN_DAYS)
}

/// Log directory = `logs/` next to `config.json`.
pub fn dir_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(DIR_NAME)
}

/// `YYYY-MM-DD` and a real date (filters out other files in the directory).
fn is_day_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if !(digits(0..4) && digits(5..7) && digits(8..10)) {
        return false;
    }
    let month: u32 = name[5..7].parse().unwrap_or(0);
    let day: u32 = name[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> LogEntry {
        LogEntry {
            seq: 1,
            level: "info",
            text: text.to_string(),
            ts_ms: 1_700_000_000_000,
            source: "cli".to_string(),
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cgw-logs-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn one_record_is_exactly_one_line() {
        let line = format_line("c-1", &entry("first\nsecond\twith\ttabs\\and\\slash"));
        assert!(!line.contains('\n'), "newline must be escaped: {line}");
        let parsed = parse_line(&line).unwrap();
        assert_eq!(parsed.text, "first\nsecond\twith\ttabs\\and\\slash");
        assert_eq!(parsed.source, "cli");
        assert_eq!(parsed.scope, "c-1");
        assert_eq!(parsed.level, "info");
        assert_eq!(parsed.ts_ms, 1_700_000_000_000);
    }

    #[test]
    fn parse_tolerates_garbage() {
        assert!(parse_line("").is_none());
        assert!(parse_line("not a record").is_none());
        assert!(parse_line("abc\tinfo\tcli\tc-1\ttext").is_none());
        // Missing fields must not panic
        assert!(parse_line("123\tinfo").is_none());
    }

    #[test]
    fn day_names_are_validated() {
        assert!(is_day_name("2026-09-27"));
        assert!(is_day_name("2000-01-01"));
        assert!(!is_day_name("2026-13-01"));
        assert!(!is_day_name("2026-00-10"));
        assert!(!is_day_name("2026-09-32"));
        assert!(!is_day_name("20260927"));
        assert!(!is_day_name("2026-09-27.log"));
        assert!(!is_day_name("README"));
        assert!(!is_day_name(""));
    }

    #[test]
    fn appends_and_reads_back() {
        let dir = temp("roundtrip");
        let sink = LogFile::new(dir.clone(), DEFAULT_RETAIN_DAYS);
        let today = local_day_now();

        let mut first = entry("hello");
        first.seq = 1;
        let mut second = entry("世界\n第二行");
        second.seq = 2;
        second.source = "webui".to_string();
        // append writes to "today", so asserts read back under today's date
        sink.append("c-1", &first);
        sink.append("c-1", &second);
        assert!(!sink.failed(), "writing a log must not fail");

        let read = sink.read_day(&today, 0).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].text, "hello");
        assert_eq!(read[1].text, "世界\n第二行");
        assert_eq!(read[1].source, "webui");
        assert_eq!(read[0].scope, "c-1");
        // Today's file really exists on disk
        assert!(dir.join(format!("{today}.log")).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_day_rejects_bad_names_and_limits_the_tail() {
        let dir = temp("limits");
        let sink = LogFile::new(dir.clone(), DEFAULT_RETAIN_DAYS);
        for index in 0..20 {
            let mut item = entry(&format!("line {index}"));
            item.seq = index;
            sink.append_to_day("2026-09-27", "c-1", &item);
        }

        let tail = sink.read_day("2026-09-27", 3).unwrap();
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[2].text, "line 19", "it takes the tail");

        assert!(sink.read_day("../etc/passwd", 0).is_err());
        assert!(sink.read_day("2026-9-27", 0).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_the_configured_number_of_days() {
        let dir = temp("prune");
        // Lay down 5 days with a long retention first, then tighten to 3 -- otherwise
        // every roll prunes along the way and 5 files never accumulate
        let sink = LogFile::new(dir.clone(), 30);
        for day in [
            "2026-09-20",
            "2026-09-21",
            "2026-09-22",
            "2026-09-23",
            "2026-09-24",
            // A non-date file name must not be deleted as a log
            "notes",
        ] {
            let mut item = entry(day);
            item.text = day.to_string();
            sink.append_to_day(day, "c-1", &item);
        }
        // roll ends on the last day, so "today" is 2026-09-24
        assert_eq!(sink.days().len(), 5, "five date files");

        sink.set_retain_days(3);
        let left = sink.days();
        assert_eq!(
            left,
            vec!["2026-09-24", "2026-09-23", "2026-09-22"],
            "keeps only the 3 most recent days"
        );
        // Files whose name is not a date are never touched
        assert!(dir.join("notes.log").exists());
        std::fs::write(dir.join("something.txt"), b"x").unwrap();
        sink.prune();
        assert!(dir.join("something.txt").exists());
        // Even less may the day being written be deleted
        assert!(dir.join("2026-09-24.log").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A day roll during writes prunes expired files along the way, with no external
    /// timer.
    #[test]
    fn append_prunes_when_the_day_rolls() {
        let dir = temp("auto-prune");
        let sink = LogFile::new(dir.clone(), 2);
        // Build two days of history first
        for day in ["2026-09-20", "2026-09-21"] {
            let mut item = entry(day);
            item.text = day.to_string();
            sink.append_to_day(day, "c-1", &item);
        }
        assert_eq!(sink.days().len(), 2);

        // Then use append to write "today": only this path triggers prune
        sink.append("c-1", &entry("today"));
        let days = sink.days();
        assert_eq!(days.len(), 2, "retention is 2 days, so writing today should drop the old ones: {days:?}");
        assert_eq!(days[0], local_day_now());
        assert!(!days.contains(&"2026-09-20".to_string()), "{days:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_is_clamped() {
        let dir = temp("clamp");
        let sink = LogFile::new(dir.clone(), 0);
        assert_eq!(sink.retain_days(), 1, "keep at least one day");
        sink.set_retain_days(u64::MAX);
        assert_eq!(sink.retain_days(), MAX_RETAIN_DAYS);
        sink.set_retain_days(7);
        assert_eq!(sink.retain_days(), 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn days_are_listed_newest_first_and_ignore_other_files() {
        let dir = temp("days");
        let sink = LogFile::new(dir.clone(), DEFAULT_RETAIN_DAYS);
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "2026-09-25.log",
            "2026-09-27.log",
            "2026-09-26.log",
            "something.log",
            "2026-09-27.txt",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        assert_eq!(
            sink.days(),
            vec!["2026-09-27", "2026-09-26", "2026-09-25"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_day_looks_like_a_date() {
        let day = local_day_now();
        assert!(is_day_name(&day), "local date has the wrong format: {day}");
        // This moment's UTC date should differ from the local date by at most a day
        let utc = utc_ymd_now();
        assert!(
            (utc.0 - day[..4].parse::<i32>().unwrap()).abs() <= 1,
            "local date {day} and UTC {utc:?} differ too much"
        );
    }
}
