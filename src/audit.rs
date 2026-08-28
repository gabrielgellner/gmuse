//! A session log, for auditing playback over a long listen.
//!
//! Exists because underruns are the one defect that cannot be caught by a
//! test: DESIGN §3 warns they show up only in listening, and by the time you
//! notice the counter has moved, the moment has passed. This records *when*,
//! *in what*, and *how badly*, so a pattern can be seen afterwards.
//!
//! **Nothing here runs on the audio callback.** The callback records to
//! atomics; this is called from the UI loop, which may block, allocate and
//! write files freely.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// `$XDG_STATE_HOME/gmuse/session.log`, falling back to `~/.local/state`.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gmuse").join("session.log")
}

/// Writes timestamped lines, or does nothing at all when disabled.
///
/// Disabled is the default: a player that writes to disk during every session
/// without being asked is a surprise.
#[derive(Debug, Default)]
pub struct Audit {
    file: Option<File>,
    started: Option<Instant>,
}

impl Audit {
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Open the log, appending. A failure to open is reported and then
    /// ignored: losing the audit is not worth refusing to play music.
    pub fn open(path: &Path) -> Result<Self, std::io::Error> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut audit = Self {
            file: Some(file),
            started: Some(Instant::now()),
        };
        audit.line(
            "session",
            &format!(
                "started {}",
                humantime_now().unwrap_or_else(|| "at an unknown time".into())
            ),
        );
        Ok(audit)
    }

    pub fn is_enabled(&self) -> bool {
        self.file.is_some()
    }

    /// Write one event. Elapsed time comes first, because the question being
    /// asked of this file is always "when, relative to everything else".
    pub fn line(&mut self, kind: &str, detail: &str) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let elapsed = self.started.map(|s| s.elapsed()).unwrap_or(Duration::ZERO);
        let _ = writeln!(file, "[{}] {kind:<9} {detail}", stamp(elapsed));
        // Flushed every line: a session that ends by being killed is exactly
        // the one worth having a log of.
        let _ = file.flush();
    }
}

fn stamp(d: Duration) -> String {
    let total = d.as_secs();
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        total / 3600,
        (total % 3600) / 60,
        total % 60,
        d.subsec_millis()
    )
}

fn humantime_now() -> Option<String> {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(format!("at unix time {secs}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_writes_nothing_and_does_not_complain() {
        let mut audit = Audit::disabled();
        assert!(!audit.is_enabled());
        // Must be safe to call unconditionally, or every call site needs a
        // branch around it.
        audit.line("underrun", "should go nowhere");
    }

    #[test]
    fn events_are_written_and_flushed_immediately() {
        let dir = std::env::temp_dir().join("gmuse-audit-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("session.log");

        let mut audit = Audit::open(&path).unwrap();
        audit.line("underrun", "42 frames short");
        // Not dropped: a killed session must still have its log.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("session"), "{text}");
        assert!(text.contains("underrun  42 frames short"), "{text}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_twice_appends_rather_than_truncating() {
        let dir = std::env::temp_dir().join("gmuse-audit-append");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("session.log");

        Audit::open(&path).unwrap().line("first", "one");
        Audit::open(&path).unwrap().line("second", "two");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("one"),
            "the earlier session was lost:\n{text}"
        );
        assert!(text.contains("two"), "{text}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timestamps_are_elapsed_not_wall_clock() {
        assert_eq!(stamp(Duration::from_millis(0)), "00:00:00.000");
        assert_eq!(stamp(Duration::from_millis(1500)), "00:00:01.500");
        assert_eq!(stamp(Duration::from_secs(3661)), "01:01:01.000");
    }
}
