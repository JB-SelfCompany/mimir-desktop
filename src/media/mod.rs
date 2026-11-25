//! Media helpers: thumbnails and the attachment file actions (open with the system app, save as).
pub mod avatar;
pub mod ffmpeg;
pub mod player;
pub mod recorder;
pub mod stage;
pub mod thumbs;

use crate::{core::worker::{WorkResult, Worker}, i18n::{tr, Key, Lang}};
use std::{ffi::OsString, path::{Path, PathBuf}};

/// Extensions the OS default app may open directly (C1). The stored name is the sender's choice, so any other
/// file could be a program dressed up as a document: it is only shown in its folder.
const SAFE_TO_OPEN: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", // images
    "mp3", "m4a", "aac", "ogg", "oga", "opus", "wav", "flac", // audio
    "mp4", "m4v", "webm", "mkv", "mov", "3gp", "avi", // video
    "pdf", "txt",
];

fn safe_to_open(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| SAFE_TO_OPEN.contains(&e.to_ascii_lowercase().as_str()))
}

/// Program and arguments that open `path` with the OS default app (P3-R8 as revised: not the web browser), or,
/// for a type off the allowlist, reveal it in its folder (Windows: selected in Explorer).
pub fn open_command(path: &Path) -> (&'static str, Vec<OsString>) {
    let prog = if cfg!(windows) { "explorer.exe" } else if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if safe_to_open(path) {
        return (prog, vec![path.as_os_str().to_owned()]);
    }
    let mut select = OsString::from("/select,");
    select.push(path);
    let args = if cfg!(windows) {
        vec![select]
    } else if cfg!(target_os = "macos") {
        vec!["-R".into(), path.as_os_str().to_owned()]
    } else {
        // ponytail: xdg-open cannot select a file; the folder opens.
        vec![path.parent().unwrap_or(path).as_os_str().to_owned()]
    };
    (prog, args)
}

#[cfg(test)]
thread_local! {
    static OPENED: std::cell::RefCell<Vec<(&'static str, Vec<OsString>)>> = const { std::cell::RefCell::new(vec![]) };
}

/// Test seam: the open commands issued on this thread since the last call (tests never launch anything).
#[cfg(test)]
pub fn take_opened() -> Vec<(&'static str, Vec<OsString>)> {
    OPENED.with(|o| std::mem::take(&mut *o.borrow_mut()))
}

/// Opens `path` with the OS default app, launched off the UI thread; a failed launch reaches the status line.
pub fn open_file(worker: &Worker, lang: Lang, path: &Path) {
    let (prog, args) = open_command(path);
    #[cfg(test)]
    {
        let _ = (worker, lang);
        OPENED.with(|o| o.borrow_mut().push((prog, args)));
    }
    #[cfg(not(test))]
    {
        let worker = worker.clone();
        std::thread::spawn(move || {
            let mut cmd = std::process::Command::new(prog);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                // Explorer wants `/select,"<path>"`, not the whole argument quoted (Windows names hold no `"`).
                match args[0].to_str().and_then(|a| a.strip_prefix("/select,")) {
                    Some(p) => cmd.raw_arg(format!("/select,\"{p}\"")),
                    None => cmd.args(&args),
                };
            }
            #[cfg(not(windows))]
            cmd.args(&args);
            // Waited on so it is reaped; explorer exits 1 even on success, so only a failed launch is an error.
            if let Err(e) = cmd.status() {
                let text = crate::i18n::trf(lang, Key::CannotOpenFile, &[&e.to_string()]);
                worker.submit(Box::new(move |_| WorkResult::Error(text)));
            }
        });
    }
}

/// «Сохранить»: asks where with rfd and copies there (P3-R8); the result reaches the status line through `worker`.
pub fn save_as(worker: &Worker, lang: Lang, src: PathBuf, name: String) {
    let worker = worker.clone();
    // The dialog blocks until the user answers and the copy can be large: a thread of its own, so no lane waits.
    std::thread::spawn(move || {
        let Some(dest) = rfd::FileDialog::new().set_file_name(name).save_file() else { return };
        let result = copy_to(&src, &dest, lang);
        worker.submit(Box::new(move |_| result));
    });
}

fn copy_to(src: &Path, dest: &Path, lang: Lang) -> WorkResult {
    // Picking the attachment itself: copying a file onto itself would truncate it.
    if src.canonicalize().is_ok_and(|s| dest.canonicalize().is_ok_and(|d| d == s)) {
        return WorkResult::Notice(Key::FileSaved);
    }
    match std::fs::copy(src, dest) {
        Ok(_) => WorkResult::Notice(Key::FileSaved),
        Err(e) => WorkResult::Failed { what: tr(lang, Key::CannotSaveFile), err: e.to_string() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_reports_saved_or_localized_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dest) = (tmp.path().join("a"), tmp.path().join("b.txt"));
        std::fs::write(&src, b"hi").unwrap();
        assert!(matches!(copy_to(&src, &dest, Lang::Ru), WorkResult::Notice(Key::FileSaved)));
        assert_eq!(std::fs::read(&dest).unwrap(), b"hi");
        let WorkResult::Failed { what, .. } = copy_to(&tmp.path().join("gone"), &dest, Lang::Ru) else { panic!() };
        assert_eq!(what, "Не удалось сохранить файл");
        // Saving the attachment onto itself must not truncate it.
        let same = tmp.path().join(".").join("a");
        assert!(matches!(copy_to(&src, &same, Lang::Ru), WorkResult::Notice(Key::FileSaved)));
        assert_eq!(std::fs::read(&src).unwrap(), b"hi");
    }

    #[test]
    fn open_command_uses_the_os_default_app() {
        let p = Path::new("files").join("Ab3d.pdf");
        let (prog, args) = open_command(&p);
        assert_eq!(args.last().map(|a| Path::new(a)), Some(p.as_path()));
        assert_eq!(prog, if cfg!(windows) { "explorer.exe" } else if cfg!(target_os = "macos") { "open" } else { "xdg-open" });
    }
}
