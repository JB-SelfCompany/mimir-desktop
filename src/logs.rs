//! The client log: the core's tracing output (and `log` records) go to `<root>/logs/mimir.log`, which is
//! rotated once to `mimir.log.1` at startup when it has grown past 5 MiB. «Собрать логи» saves a copy of it.
use std::{fs, path::{Path, PathBuf}, sync::Mutex};
use tracing_subscriber::filter::LevelFilter;

/// Past this size the log is rotated at the next start.
const MAX_LOG: u64 = 5 << 20;

pub fn path(root: &Path) -> PathBuf {
    root.join("logs").join("mimir.log")
}

/// Rotates the log if it is too big, then installs a global subscriber writing to it. Call before the core opens:
/// the core installs its own (stderr) subscriber on start with `try_init`, which then fails silently.
/// ponytail: if the log cannot be opened, or a subscriber is already set, logging is simply off (stderr only);
/// the size is checked only at startup, so one long run can grow the file past 5 MiB.
pub fn init(root: &Path) {
    let log = path(root);
    if fs::metadata(&log).is_ok_and(|m| m.len() > MAX_LOG) {
        let _ = fs::rename(&log, log.with_extension("log.1"));
    }
    let _ = fs::create_dir_all(log.parent().unwrap_or(root));
    let Ok(file) = fs::OpenOptions::new().create(true).append(true).open(&log) else { return };
    let _ = tracing_subscriber::fmt().with_writer(Mutex::new(file)).with_ansi(false).with_max_level(LevelFilter::INFO).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_rotates() {
        let tmp = tempfile::tempdir().unwrap();
        let log = path(tmp.path());
        assert_eq!(log, tmp.path().join("logs").join("mimir.log"));
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(&log, vec![b'x'; 6 << 20]).unwrap();
        init(tmp.path());
        let old = tmp.path().join("logs").join("mimir.log.1");
        assert_eq!(fs::metadata(&old).unwrap().len(), 6 << 20, "the big log became mimir.log.1");
        assert!(fs::metadata(&log).unwrap().len() < MAX_LOG, "a fresh mimir.log");
        // A small log is kept as it is.
        fs::write(&log, b"small").unwrap();
        init(tmp.path());
        assert_eq!(fs::read(&log).unwrap(), b"small");
        assert_eq!(fs::metadata(&old).unwrap().len(), 6 << 20);
    }
}
