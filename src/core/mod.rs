pub mod events;
pub mod net;
pub mod worker;

use crate::paths::DataPaths;
use mimir::{MimirApp, MimirError};
use std::sync::Arc;

/// Creates the data directories and opens the core (storage + account, no networking).
pub fn open(paths: &DataPaths) -> Result<Arc<MimirApp>, MimirError> {
    // ponytail: dir-creation errors are ignored; MimirApp::open reports the real failure.
    let _ = std::fs::create_dir_all(&paths.core_dir);
    let _ = std::fs::create_dir_all(&paths.files_dir);
    Ok(Arc::new(MimirApp::open(paths.core_dir.to_string_lossy().into_owned())?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let app = open(&DataPaths::under(tmp.path())).unwrap();
        assert_eq!(app.public_key().len(), 32);
        assert!(app.get_account(1).is_some());
    }
}

/// Core keys are 32 bytes; anything else (e.g. a chat row that is gone) is refused with a log line.
// ponytail: logged to stderr; the app has no log file yet.
pub fn valid_key(key: &[u8], what: &str) -> bool {
    let ok = key.len() == 32;
    if !ok {
        eprintln!("mimir-desktop: refusing to {what} with a {}-byte key", key.len());
    }
    ok
}
