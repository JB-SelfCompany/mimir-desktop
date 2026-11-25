//! Backup export and staged import (AS §2), in Android Mimir's zip layout so backups move between the two.
//!
//! Export: a `backup_to` snapshot as `mimir.db`, the four avatar dirs under `mimir/`, the attachments under `files/`, the
//! Android prefs XML and our own `shared_prefs/mimir_desktop.json` (P4-R2). Import is staged: `stage` copies the
//! zip to `<root>/restore_pending.zip`, the app relaunches (P4-R1) and the next start runs `finish_pending` before the
//! core opens the files it replaces (a running core keeps `mimir.db` locked on Windows).
use crate::{i18n::Lang, model::prefs_xml::{parse_prefs, write_prefs}, paths::DataPaths, settings::Settings};
use mimir::MimirApp;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

const PENDING: &str = "restore_pending.zip";
const TMP: &str = "restore_tmp";
const OLD: &str = "restore_old";
const SNAPSHOT: &str = "backup_snapshot.db";
const PREFS_XML: &str = "shared_prefs/com.revertron.mimir_preferences.xml";
const DESKTOP_JSON: &str = "shared_prefs/mimir_desktop.json";
const AVATAR_DIRS: [&str; 4] = ["avatars", "group_avatars", "invite_avatars", "request_avatars"];
/// What an import replaces, under the data root: Android deletes `mimir/` and `files/` (AS §2.2 step 5); our settings
/// file stands in for its `shared_prefs/`.
const SWAPPED: [&str; 3] = ["mimir", "files", "settings.json"];
/// Written into `restore_old` once the backup's data is fully in place.
const SWAP_DONE: &str = ".swap_done";
/// A finished swap's `restore_old`, renamed `restore_trash_<millis>` in one step: `recover` only ever deletes it.
const TRASH: &str = "restore_trash_";
/// Written once a backup's data is in place, removed after the copy got its own client id: a start that dies (or a
/// regenerate that fails) in between retries at the next start (M1).
const REGENERATE: &str = ".regenerate_client_id";
/// Locked (`File::try_lock`) while a start recovers or imports (I2): a second instance must not run `recover` against a
/// swap in progress. The OS drops the lock when the process ends, killed or not; the file itself stays.
const LOCK: &str = ".restore.lock";
/// How long a rename keeps trying: right after the P4-R1 relaunch the old process may still hold `mimir.db`.
const RENAME_WAIT: Duration = Duration::from_secs(5);

/// The desktop-only settings, `shared_prefs/mimir_desktop.json` (P4-R2).
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct DesktopPrefs {
    lang: Option<Lang>,
    dark: Option<bool>,
    intro_done: bool,
}

/// Android's export file name (AS §2.1).
pub fn default_name() -> String {
    format!("mimir_backup_{}.zip", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis())
}

fn text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Writes a backup of the running core to `dest`; a failed export leaves no file behind.
pub fn export(core: &MimirApp, paths: &DataPaths, settings: &Settings, dest: &Path) -> Result<(), String> {
    // VACUUM INTO refuses an existing file.
    let snapshot = paths.root.join(SNAPSHOT);
    let _ = fs::remove_file(&snapshot);
    let result = core.backup_to(snapshot.to_string_lossy().into_owned()).map_err(text).and_then(|()| write_zip(paths, settings, &snapshot, dest));
    let _ = fs::remove_file(&snapshot);
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

type Zip = ZipWriter<BufWriter<File>>;

fn write_zip(paths: &DataPaths, settings: &Settings, snapshot: &Path, dest: &Path) -> Result<(), String> {
    let mut zip = ZipWriter::new(BufWriter::new(File::create(dest).map_err(text)?));
    add_file(&mut zip, snapshot, "mimir.db")?;
    for dir in AVATAR_DIRS {
        add_dir(&mut zip, &paths.core_dir.join(dir), &format!("mimir/{dir}"))?;
    }
    add_dir(&mut zip, &paths.files_dir, "files")?;
    let desktop = DesktopPrefs { lang: settings.lang, dark: settings.dark, intro_done: settings.intro_done };
    add_bytes(&mut zip, PREFS_XML, write_prefs(&settings.to_android_prefs()).as_bytes())?;
    add_bytes(&mut zip, DESKTOP_JSON, &serde_json::to_vec_pretty(&desktop).map_err(text)?)?;
    zip.finish().map_err(text)?.into_inner().map_err(|e| e.error().to_string())?;
    Ok(())
}

/// Deflated like Android's ZipOutputStream; zip64 only where a file needs it.
fn options(len: u64) -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).large_file(len >= u32::MAX as u64)
}

fn add_bytes(zip: &mut Zip, name: &str, bytes: &[u8]) -> Result<(), String> {
    zip.start_file(name, options(bytes.len() as u64)).map_err(text)?;
    zip.write_all(bytes).map_err(text)
}

fn add_file(zip: &mut Zip, path: &Path, name: &str) -> Result<(), String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = file.metadata().map_err(text)?.len();
    zip.start_file(name, options(len)).map_err(text)?;
    io::copy(&mut BufReader::new(file), zip).map_err(text)?;
    Ok(())
}

/// Every file under `dir` as `prefix/<relative path>` (no directory entries, as Android writes them). A missing dir
/// adds nothing; the video poster cache (`.thumbs/`) is left out, like Android's preview cache.
fn add_dir(zip: &mut Zip, dir: &Path, prefix: &str) -> Result<(), String> {
    let Ok(entries) = fs::read_dir(dir) else { return Ok(()) };
    for entry in entries {
        let entry = entry.map_err(text)?;
        // ponytail: a name that is not UTF-8 is stored lossily (Android names are ASCII hex or UUIDs).
        let name = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        match entry.file_type().map_err(text)? {
            t if t.is_dir() && entry.file_name() != ".thumbs" => add_dir(zip, &entry.path(), &name)?,
            t if t.is_file() => add_file(zip, &entry.path(), &name)?,
            _ => {}
        }
    }
    Ok(())
}

/// An entry name the way Android reads it (leading `/` and `./` dropped), or `None` when it could land outside
/// the data dir: a `..` component, a colon (drive letter, NTFS stream) or a backslash root.
fn clean(raw: &str) -> Option<&str> {
    let name = raw.trim_start_matches('/');
    let name = name.strip_prefix("./").unwrap_or(name);
    let escapes = name.contains(':') || name.starts_with('\\') || name.split(['/', '\\']).any(|c| c == "..");
    (!escapes).then_some(name)
}

/// Android's whitelist (AS §2.2 step 3).
fn known(name: &str) -> bool {
    name == "mimir.db" || name.starts_with("avatars_") || ["mimir", "files", "shared_prefs", "avatars"].iter().any(|d| name.strip_prefix(d).is_some_and(|r| r.is_empty() || r.starts_with('/')))
}

fn open_zip(path: &Path) -> Result<ZipArchive<BufReader<File>>, String> {
    ZipArchive::new(BufReader::new(File::open(path).map_err(text)?)).map_err(text)
}

/// A Mimir backup: a zip of known entries only, with `mimir.db` among them, and nothing that could escape the data dir.
pub fn validate(zip: &Path) -> Result<(), String> {
    let archive = open_zip(zip)?;
    let mut has_db = false;
    for raw in archive.file_names() {
        let raw = raw.map_err(text)?;
        let name = clean(&raw).ok_or_else(|| format!("unsafe entry {raw:?}"))?;
        if !known(name) {
            return Err(format!("unexpected entry {raw:?}"));
        }
        has_db |= name == "mimir.db";
    }
    if has_db { Ok(()) } else { Err("no mimir.db inside".into()) }
}

/// Copies the chosen backup to where `finish_pending` looks for it at the next start: to a temp file of its own first,
/// then renamed into place, so a failed copy never touches a zip another stage put there.
pub fn stage(paths: &DataPaths, zip: &Path) -> Result<(), String> {
    fs::create_dir_all(&paths.root).map_err(text)?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let part = paths.root.join(format!("{PENDING}.{nanos}.part"));
    fs::copy(zip, &part).map(drop).and_then(|()| fs::rename(&part, paths.root.join(PENDING))).map_err(|e| {
        let _ = fs::remove_file(&part);
        e.to_string()
    })
}

/// Where a `clean` entry goes under `restore_tmp` (AS §2.2 step 6, legacy avatar dirs re-homed); `None`: skipped.
/// Built from its non-empty components, so `avatars//x` cannot turn into a rooted path on join.
fn target(name: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = name.split(['/', '\\']).filter(|c| !c.is_empty() && *c != ".").collect();
    let (top, rest) = parts.split_first()?;
    let base = match *top {
        "mimir.db" if rest.is_empty() => return Some(["mimir", "mimir.db"].iter().collect()),
        "mimir" | "files" => vec![*top],
        "avatars" => vec!["mimir", "avatars"],
        // Android writes the numeric chat id; anything else is skipped.
        t => vec!["mimir", "group_avatars", t.strip_prefix("avatars_").filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))?],
    };
    if rest.is_empty() {
        return None;
    }
    Some(base.iter().chain(rest).collect())
}

/// The locked `<root>/.restore.lock`, held until dropped. `None` inside when the file could not be opened or locked
/// for another reason than another instance holding it (`finish_pending` then reports what is wrong with the data dir).
pub struct RestoreLock(#[allow(dead_code)] Option<File>);

/// The restore lock, or `None` while another instance holds it.
fn lock(root: &Path) -> Option<RestoreLock> {
    let _ = fs::create_dir_all(root);
    let Ok(file) = fs::OpenOptions::new().create(true).write(true).truncate(false).open(root.join(LOCK)) else {
        return Some(RestoreLock(None));
    };
    match file.try_lock() {
        Ok(()) => Some(RestoreLock(Some(file))),
        Err(fs::TryLockError::WouldBlock) => None,
        Err(fs::TryLockError::Error(_)) => Some(RestoreLock(None)),
    }
}

/// The restored database still has the backup's client id: the caller regenerates it before the network starts.
pub fn regenerate_pending(root: &Path) -> bool {
    root.join(REGENERATE).exists()
}

/// The new client id is stored: `regenerate_pending` is false from now on.
pub fn client_id_regenerated(root: &Path) {
    let _ = fs::remove_file(root.join(REGENERATE));
}

/// `finish_pending` under the restore lock; `None`: another instance is restoring right now.
pub fn restore(paths: &DataPaths) -> Option<Result<bool, String>> {
    let _lock = lock(&paths.root)?;
    Some(finish_pending(paths))
}

/// Applies a staged import, if there is one, before the core opens. `Ok(true)`: the backup's data is in place (the caller
/// gives it a new client id); `Err`: the old data is back. The pending zip is removed either way, so a bad one does
/// not come back at every start.
fn finish_pending(paths: &DataPaths) -> Result<bool, String> {
    let pending = paths.root.join(PENDING);
    // Runs at every start, pending zip or not: old data left in restore_old must come back before the core opens.
    let result = recover(&paths.root);
    if !pending.exists() {
        return result.map(|()| false);
    }
    let result = result.and_then(|()| apply(paths, &pending));
    let _ = fs::remove_file(&pending);
    let _ = fs::remove_dir_all(paths.root.join(TMP));
    result.map(|()| true)
}

fn apply(paths: &DataPaths, pending: &Path) -> Result<(), String> {
    let (root, tmp, old) = (&paths.root, paths.root.join(TMP), paths.root.join(OLD));
    validate(pending)?;
    let _ = fs::remove_dir_all(&tmp);
    let (xml, desktop) = extract(pending, &tmp)?;
    if !tmp.join("mimir/mimir.db").is_file() {
        return Err("no mimir.db inside".into());
    }
    fs::create_dir_all(tmp.join("files")).map_err(text)?;
    // Android replaces its prefs with the backup's (AS §2.2 step 5); lang and theme come from our own file, else stay.
    let mut settings = Settings::load(&paths.settings_file);
    settings.apply_android_prefs(&xml.map(|x| parse_prefs(&x)).unwrap_or_default());
    if let Some(d) = desktop.and_then(|j| serde_json::from_str::<DesktopPrefs>(&j).ok()) {
        (settings.lang, settings.dark) = (d.lang, d.dark);
    }
    settings.intro_done = true;
    // Per-group bookkeeping of the replaced database.
    settings.chat_info_applied.clear();
    settings.save(&tmp.join("settings.json")).map_err(text)?;
    fs::create_dir_all(&old).map_err(text)?;
    // After a rollback restore_old is empty and goes; when even the rollback failed it holds the old data and stays.
    swap(root, &tmp, &old).inspect_err(|_| drop(fs::remove_dir(&old)))?;
    // ponytail: if this write fails, only this start's `Ok(true)` makes the caller regenerate.
    let _ = fs::write(root.join(REGENERATE), "");
    // Only now may restore_old go. Windows `remove_dir_all` can stop part-way (a file held open) after deleting the
    // marker, so restore_old first becomes trash in one rename; a leftover trash dir goes at the next start.
    // ponytail: if that rename fails too, only the marker tells `recover` the swap finished.
    let _ = fs::write(old.join(SWAP_DONE), "");
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let trash = root.join(format!("{TRASH}{millis}"));
    let _ = remove_old(if rename_retry(&old, &trash).is_ok() { &trash } else { &old });
    Ok(())
}

/// Puts back old data that a swap left in `restore_old` (a start that died mid-swap, or a rollback that failed): each
/// saved entry replaces whatever half-moved backup data is in its place. With the `SWAP_DONE` marker the swap had
/// finished and restore_old is only deleted. On failure restore_old is kept, never deleted.
fn recover(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with(TRASH) {
            let _ = remove_old(&entry.path());
        }
    }
    let old = root.join(OLD);
    if !old.exists() {
        return Ok(());
    }
    if !old.join(SWAP_DONE).exists() {
        for name in SWAPPED {
            let (saved, live) = (old.join(name), root.join(name));
            if !saved.exists() {
                continue;
            }
            if live.exists() {
                let removed = if live.is_dir() { fs::remove_dir_all(&live) } else { fs::remove_file(&live) };
                removed.map_err(|e| format!("{}: {e}; old data kept in {}", live.display(), old.display()))?;
            }
            rename_retry(&saved, &live).map_err(|e| format!("{}: {e}; old data kept in {}", saved.display(), old.display()))?;
        }
    }
    fs::remove_dir_all(&old).map_err(text)
}

/// Unpacks the mapped entries into `tmp`; returns the Android prefs XML and our desktop JSON, if present. Other prefs files
/// (Android's notification prefs, a lumio `settings.json`) are ignored (P4-R2).
fn extract(zip: &Path, tmp: &Path) -> Result<(Option<String>, Option<String>), String> {
    let mut archive = open_zip(zip)?;
    let (mut xml, mut desktop) = (None, None);
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(text)?;
        let raw = entry.name().map_err(text)?.into_owned();
        let name = clean(&raw).ok_or_else(|| format!("unsafe entry {raw:?}"))?;
        if entry.is_dir() || name.ends_with('/') {
            continue;
        }
        let read = |entry: &mut dyn Read| -> Result<String, String> {
            let mut bytes = vec![];
            entry.read_to_end(&mut bytes).map_err(text)?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        };
        match name {
            PREFS_XML => xml = Some(read(&mut entry)?),
            DESKTOP_JSON => desktop = Some(read(&mut entry)?),
            _ => {
                let Some(rel) = target(name) else { continue };
                let dest = tmp.join(rel);
                if let Some(dir) = dest.parent() {
                    fs::create_dir_all(dir).map_err(text)?;
                }
                // Straight to the file (no BufWriter whose flush on drop would swallow a full disk).
                io::copy(&mut entry, &mut File::create(&dest).map_err(text)?).map_err(|e| format!("{raw}: {e}"))?;
            }
        }
    }
    Ok((xml, desktop))
}

/// Moves the live `SWAPPED` entries into `old` and the extracted ones into place; on any failure undoes what was moved.
fn swap(root: &Path, tmp: &Path, old: &Path) -> Result<(), String> {
    let (mut out, mut ins) = (vec![], vec![]);
    let mut result = Ok(());
    for name in SWAPPED {
        let live = root.join(name);
        if live.exists() {
            if let Err(e) = rename_retry(&live, &old.join(name)) {
                result = Err(format!("{}: {e}", live.display()));
                break;
            }
            out.push(name);
        }
        if let Err(e) = rename_retry(&tmp.join(name), &live) {
            result = Err(format!("{}: {e}", live.display()));
            break;
        }
        ins.push(name);
    }
    let Err(e) = result else { return Ok(()) };
    for name in ins {
        let live = root.join(name);
        if fs::rename(&live, tmp.join(name)).is_err() {
            let _ = if live.is_dir() { fs::remove_dir_all(&live) } else { fs::remove_file(&live) };
        }
    }
    for name in out {
        // ponytail: if even this fails the old data stays in restore_old (named in the error) for the user to move back.
        rename_retry(&old.join(name), &root.join(name)).map_err(|r| format!("{e}; restoring {name} failed ({r}), old data kept in {}", old.display()))?;
    }
    Err(e)
}

#[cfg(test)]
thread_local! {
    static FAIL_RENAME: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(vec![]) };
}

/// Test seam: renaming `from` fails on this thread (a locked dir, without the 5 s wait).
#[cfg(test)]
fn fail_rename(from: PathBuf) {
    FAIL_RENAME.with(|f| f.borrow_mut().push(from));
}

/// Test seam: renames work again.
#[cfg(test)]
fn unlock_renames() {
    FAIL_RENAME.with(|f| f.borrow_mut().clear());
}

#[cfg(test)]
thread_local! {
    static FAIL_CLEANUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test seam: deleting a finished swap's old data fails part-way on this thread, after `.swap_done` went (Windows
/// `remove_dir_all` deletes in name order; a file another process holds open stops it there, os error 32).
#[cfg(test)]
fn fail_cleanup(on: bool) {
    FAIL_CLEANUP.with(|f| f.set(on));
}

/// Deletes the old data of a finished swap.
fn remove_old(dir: &Path) -> io::Result<()> {
    #[cfg(test)]
    if FAIL_CLEANUP.with(|f| f.get()) {
        let _ = fs::remove_file(dir.join(SWAP_DONE));
        return Err(io::Error::other("in use (test)"));
    }
    fs::remove_dir_all(dir)
}

/// `fs::rename`, retried for `RENAME_WAIT` while it fails (a file still open in the exiting old process).
fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(test)]
    if FAIL_RENAME.with(|f| f.borrow().iter().any(|p| p == from)) {
        return Err(io::Error::other("locked (test)"));
    }
    let end = Instant::now() + RENAME_WAIT;
    loop {
        match fs::rename(from, to) {
            Err(_) if Instant::now() < end => std::thread::sleep(Duration::from_millis(100)),
            r => return r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core, model::prefs_xml::PrefValue, settings::MediaSettings, test_support::test_app};

    const PHONE_PREFS: &str = include_str!("../tests/fixtures/android_prefs.xml");

    /// A zip of `(name, bytes)` entries; a name ending in `/` is a directory entry.
    fn zip_of(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        for (name, bytes) in entries {
            if let Some(dir) = name.strip_suffix('/') {
                zip.add_directory(dir, SimpleFileOptions::default()).unwrap();
            } else {
                zip.start_file(*name, SimpleFileOptions::default()).unwrap();
                zip.write_all(bytes).unwrap();
            }
        }
        zip.finish().unwrap();
    }

    fn names(zip: &Path) -> Vec<String> {
        let mut n: Vec<String> = open_zip(zip).unwrap().file_names().map(|n| n.unwrap().into_owned()).collect();
        n.sort();
        n
    }

    fn entry(zip: &Path, name: &str) -> String {
        let mut s = String::new();
        open_zip(zip).unwrap().by_name(name).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    fn leftovers(p: &DataPaths) -> Vec<&'static str> {
        [PENDING, TMP, OLD, SNAPSHOT].into_iter().filter(|n| p.root.join(n).exists()).collect()
    }

    #[test]
    fn export_layout() {
        let (tmp, core) = test_app();
        let paths = DataPaths::under(tmp.path());
        fs::create_dir_all(paths.core_dir.join("avatars")).unwrap();
        fs::write(paths.core_dir.join("avatars/ab.avatar"), "a").unwrap();
        fs::create_dir_all(paths.files_dir.join(".thumbs")).unwrap();
        fs::write(paths.files_dir.join("f1"), "x").unwrap();
        fs::write(paths.files_dir.join(".thumbs/f1.jpg"), "poster").unwrap();
        let media = MediaSettings { voice_bitrate: 64000, ..Default::default() };
        let s = Settings { lang: Some(Lang::En), dark: Some(true), intro_done: true, priority: 7, media, ..Default::default() };
        let out = tempfile::tempdir().unwrap();
        let dest = out.path().join(default_name());
        export(&core, &paths, &s, &dest).unwrap();
        assert_eq!(names(&dest), ["files/f1", "mimir.db", "mimir/avatars/ab.avatar", PREFS_XML, DESKTOP_JSON]);
        assert!(dest.file_name().unwrap().to_string_lossy().starts_with("mimir_backup_1"));
        let mut back = Settings::default();
        back.apply_android_prefs(&parse_prefs(&entry(&dest, PREFS_XML)));
        assert_eq!((back.media, back.priority, back.peers), (s.media.clone(), 7, s.peers.clone()));
        let d: DesktopPrefs = serde_json::from_str(&entry(&dest, DESKTOP_JSON)).unwrap();
        assert_eq!((d.lang, d.dark, d.intro_done), (Some(Lang::En), Some(true), true));
        assert!(leftovers(&paths).is_empty(), "snapshot deleted");
        // A failed export leaves no file: here the destination folder does not exist.
        let bad = out.path().join("missing").join("b.zip");
        assert!(export(&core, &paths, &s, &bad).is_err());
        assert!(!bad.exists() && leftovers(&paths).is_empty());
    }

    #[test]
    fn roundtrip_restores_and_regenerates() {
        let (a_tmp, a) = test_app();
        let pa = DataPaths::under(a_tmp.path());
        let c = a.add_contact(vec![7; 32], "Аня".into()).unwrap();
        a.add_local_message(c, 1, 0, true, true, 1000, 0, 0, b"hi".to_vec()).unwrap();
        fs::write(pa.files_dir.join("att"), "file").unwrap();
        let media = MediaSettings { voice_bitrate: 128000, ..Default::default() };
        let out = tempfile::tempdir().unwrap();
        let zip = out.path().join("b.zip");
        export(&a, &pa, &Settings { lang: Some(Lang::En), intro_done: true, media, ..Default::default() }, &zip).unwrap();
        validate(&zip).unwrap();

        // B already ran once: its own identity, Russian, dark.
        let b_tmp = tempfile::tempdir().unwrap();
        let pb = DataPaths::under(b_tmp.path());
        let b_key = core::open(&pb).unwrap().public_key();
        Settings { lang: Some(Lang::Ru), dark: Some(true), ..Default::default() }.save(&pb.settings_file).unwrap();
        stage(&pb, &zip).unwrap();
        assert!(pb.root.join(PENDING).is_file());
        assert_eq!(finish_pending(&pb), Ok(true));
        assert!(leftovers(&pb).is_empty(), "{:?}", leftovers(&pb));
        let b = core::open(&pb).unwrap();
        assert_eq!(b.public_key(), a.public_key());
        assert_ne!(b.public_key(), b_key);
        let c = b.get_contact_by_pubkey(vec![7; 32]).expect("contact restored").id;
        assert_eq!(b.get_messages(c, i64::MAX, 10).len(), 1);
        assert_eq!(fs::read_to_string(pb.files_dir.join("att")).unwrap(), "file");
        let s = Settings::load(&pb.settings_file);
        assert_eq!((s.media.voice_bitrate, s.lang, s.dark, s.intro_done), (128000, Some(Lang::En), None, true));
        let id = |m: &MimirApp| m.get_account(1).unwrap().client_id;
        assert_eq!(id(&b), id(&a), "a whole-DB restore clones the client id");
        b.regenerate_client_id().unwrap();
        assert_ne!(id(&b), id(&a));
        assert_eq!(finish_pending(&pb), Ok(false), "applied once");
    }

    #[test]
    fn phone_prefs_fixture() {
        // Android 3.3.7's own file: &#10; newlines in peers, extra keys, no client-priority.
        let mut s = Settings::default();
        s.apply_android_prefs(&parse_prefs(PHONE_PREFS));
        assert_eq!(s.media.voice_bitrate, 64000);
        assert_eq!(s.peers.len(), 10);
        assert!(s.peers.iter().all(|p| p.enabled));
        assert_eq!(s.priority, 1);
        let extra: Vec<&str> = s.android_extra.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(extra, ["record_mode", "migrated_to_mimir", "emoji_usage_counts"]);
        assert_eq!(s.android_extra[0].value, PrefValue::Int(1));
        assert!(s.to_android_prefs().iter().any(|e| e.name == "emoji_usage_counts"), "written back on export");
    }

    #[test]
    fn android_zip_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("phone.zip");
        zip_of(&zip, &[
            ("mimir/", b""),
            ("mimir.db", b"db"),
            ("mimir/avatars/me.avatar", b"m"),
            ("avatars/old.avatar", b"o"),
            ("avatars_5/chat.avatar", b"g"),
            ("avatars_7//x.avatar", b"x"),
            ("avatars//rooted.avatar", b"r"),
            ("avatars_../y.avatar", b"y"),
            ("./files/f", b"f"),
            ("shared_prefs/notification_prefs.xml", b"<map />"),
            (PREFS_XML, PHONE_PREFS.as_bytes()),
        ]);
        validate(&zip).unwrap();
        let p = DataPaths::under(&tmp.path().join("data"));
        Settings { lang: Some(Lang::En), dark: Some(false), ..Default::default() }.save(&p.settings_file).unwrap();
        stage(&p, &zip).unwrap();
        assert_eq!(finish_pending(&p), Ok(true));
        let read = |rel: &str| fs::read_to_string(p.root.join(rel)).unwrap();
        assert_eq!(read("mimir/mimir.db"), "db");
        assert_eq!(read("mimir/avatars/me.avatar"), "m");
        assert_eq!(read("mimir/avatars/old.avatar"), "o", "legacy avatars/");
        assert_eq!(read("mimir/group_avatars/5/chat.avatar"), "g", "legacy avatars_<id>/");
        assert_eq!(read("mimir/group_avatars/7/x.avatar"), "x");
        assert_eq!(read("mimir/avatars/rooted.avatar"), "r", "an empty component does not root the path");
        assert!(!p.core_dir.join("y.avatar").exists() && !p.root.join("y.avatar").exists(), "non-numeric chat id skipped");
        assert_eq!(read("files/f"), "f");
        assert!(!p.root.join("shared_prefs").exists(), "notification prefs ignored");
        let s = Settings::load(&p.settings_file);
        assert_eq!((s.media.voice_bitrate, s.peers.len(), s.android_extra.len()), (64000, 10, 3));
        assert_eq!((s.lang, s.dark, s.intro_done), (Some(Lang::En), Some(false), true), "no desktop file: ours stay");
    }

    #[test]
    fn rejects_zip_without_db() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("a.zip");
        zip_of(&zip, &[("files/a", b"1"), ("shared_prefs/x.xml", b"")]);
        assert!(validate(&zip).unwrap_err().contains("mimir.db"));
        zip_of(&zip, &[("mimir.db", b"db"), ("notes.txt", b"")]);
        assert!(validate(&zip).is_err(), "unknown entry");
        fs::write(&zip, "not a zip").unwrap();
        assert!(validate(&zip).is_err());
        // Staged anyway (an old pending file): refused at start, data untouched, the zip gone.
        let p = DataPaths::under(&tmp.path().join("data"));
        fs::create_dir_all(&p.files_dir).unwrap();
        fs::write(p.files_dir.join("keep"), "k").unwrap();
        stage(&p, &zip).unwrap();
        assert!(finish_pending(&p).is_err());
        assert_eq!(fs::read_to_string(p.files_dir.join("keep")).unwrap(), "k");
        assert!(leftovers(&p).is_empty());
    }

    #[test]
    fn rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("a.zip");
        for bad in ["files/../../x", "mimir/a/../../../x", "files\\..\\..\\x", "files/a:stream", "\\files/x"] {
            zip_of(&zip, &[("mimir.db", b"db"), (bad, b"evil")]);
            assert!(validate(&zip).is_err(), "{bad}");
        }
        // Android's own normalisation is accepted.
        zip_of(&zip, &[("/mimir.db", b"db"), ("./files/a", b"1"), ("files", b"")]);
        validate(&zip).unwrap();
        // A traversal zip staged behind validate's back is refused by finish_pending, nothing written outside.
        zip_of(&zip, &[("mimir.db", b"db"), ("files/../../x", b"evil")]);
        let p = DataPaths::under(&tmp.path().join("data"));
        stage(&p, &zip).unwrap();
        assert!(finish_pending(&p).is_err());
        assert!(!tmp.path().join("x").exists() && !p.core_dir.exists());
        assert!(leftovers(&p).is_empty());
    }

    #[test]
    fn foreign_prefs_file_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("lumio.zip");
        let lumio = br#"{"message_font":21.0,"media":{"voice_bitrate":128000},"intro_done":false}"#;
        zip_of(&zip, &[("mimir.db", b"db"), ("shared_prefs/settings.json", lumio), (PREFS_XML, PHONE_PREFS.as_bytes())]);
        validate(&zip).unwrap();
        let p = DataPaths::under(&tmp.path().join("data"));
        stage(&p, &zip).unwrap();
        assert_eq!(finish_pending(&p), Ok(true));
        let s = Settings::load(&p.settings_file);
        assert_eq!((s.media.voice_bitrate, s.message_font, s.intro_done), (64000, 15.0, true), "from the XML only");
        assert!(!p.root.join("shared_prefs").exists());
    }

    #[test]
    fn failed_apply_rolls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        let key = core::open(&p).unwrap().public_key();
        fs::write(p.files_dir.join("old.txt"), "old").unwrap();
        let before = Settings { lang: Some(Lang::En), message_font: 21.0, intro_done: true, ..Default::default() };
        before.save(&p.settings_file).unwrap();
        let out = tempfile::tempdir().unwrap();
        let zip = out.path().join("b.zip");
        zip_of(&zip, &[("mimir.db", b"new"), ("files/new.txt", b"new")]);
        stage(&p, &zip).unwrap();
        // mimir/ is already swapped and files/ moved out when the new files/ cannot be moved in.
        fail_rename(p.root.join(TMP).join("files"));
        let err = finish_pending(&p).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        assert_eq!(fs::read_to_string(p.files_dir.join("old.txt")).unwrap(), "old");
        assert!(!p.files_dir.join("new.txt").exists());
        assert_eq!(core::open(&p).unwrap().public_key(), key, "the old database is back");
        assert_eq!(Settings::load(&p.settings_file), before);
        assert!(leftovers(&p).is_empty(), "{:?}", leftovers(&p));
    }

    #[test]
    fn interrupted_swap_recovers_old_data() {
        // A start that died after moving mimir/ out: the next one puts it back before applying again.
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        fs::create_dir_all(p.root.join(OLD).join("mimir")).unwrap();
        fs::write(p.root.join(OLD).join("mimir/mimir.db"), "old").unwrap();
        let zip = tmp.path().join("x.zip");
        zip_of(&zip, &[("mimir.db", b"new")]);
        stage(&p, &zip).unwrap();
        fail_rename(p.root.join(TMP).join("mimir"));
        assert!(finish_pending(&p).is_err());
        assert_eq!(fs::read_to_string(p.core_dir.join("mimir.db")).unwrap(), "old");
    }

    /// Old data under restore_old/ with a database file (a swap that never finished).
    fn leave_old(p: &DataPaths, db: &str) {
        fs::create_dir_all(p.root.join(OLD).join("mimir")).unwrap();
        fs::write(p.root.join(OLD).join("mimir/mimir.db"), db).unwrap();
    }

    #[test]
    fn leftover_old_data_restored_without_pending() {
        // Died after mimir/ was moved out: restore_old has it, mimir/ is missing, no pending zip.
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        leave_old(&p, "old");
        assert_eq!(finish_pending(&p), Ok(false));
        assert_eq!(fs::read_to_string(p.core_dir.join("mimir.db")).unwrap(), "old");
        assert!(leftovers(&p).is_empty());
        // Died after the backup's mimir/ was moved in: the old one wins, the half-done import goes.
        leave_old(&p, "older");
        fs::write(p.core_dir.join("mimir.db"), "half-imported").unwrap();
        assert_eq!(finish_pending(&p), Ok(false));
        assert_eq!(fs::read_to_string(p.core_dir.join("mimir.db")).unwrap(), "older");
        // A finished swap (marker written, delete cut short): the new data stays, restore_old goes.
        leave_old(&p, "stale");
        fs::write(p.root.join(OLD).join(SWAP_DONE), "").unwrap();
        assert_eq!(finish_pending(&p), Ok(false));
        assert_eq!(fs::read_to_string(p.core_dir.join("mimir.db")).unwrap(), "older");
        assert!(leftovers(&p).is_empty());
    }

    #[test]
    fn import_never_deletes_unrecovered_old_data() {
        // restore_old cannot be moved back (locked): the import is refused and restore_old is kept as it was.
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        leave_old(&p, "old");
        let out = tempfile::tempdir().unwrap();
        let zip = out.path().join("b.zip");
        zip_of(&zip, &[("mimir.db", b"new")]);
        stage(&p, &zip).unwrap();
        fail_rename(p.root.join(OLD).join("mimir"));
        assert!(finish_pending(&p).unwrap_err().contains("old data kept"));
        assert_eq!(fs::read_to_string(p.root.join(OLD).join("mimir/mimir.db")).unwrap(), "old");
        assert!(!p.root.join(PENDING).exists() && !p.core_dir.exists());
        // Unlocked, the next start brings it back.
        unlock_renames();
        assert_eq!(finish_pending(&p), Ok(false));
        assert_eq!(fs::read_to_string(p.core_dir.join("mimir.db")).unwrap(), "old");
    }

    #[test]
    fn failed_rollback_recovered_at_next_start() {
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        fs::create_dir_all(&p.core_dir).unwrap();
        fs::write(p.core_dir.join("mimir.db"), "old").unwrap();
        fs::create_dir_all(&p.files_dir).unwrap();
        fs::write(p.files_dir.join("old.txt"), "old").unwrap();
        let out = tempfile::tempdir().unwrap();
        let zip = out.path().join("b.zip");
        zip_of(&zip, &[("mimir.db", b"new"), ("files/new.txt", b"new")]);
        stage(&p, &zip).unwrap();
        // The new files/ cannot move in, and the old files/ then cannot move back either.
        fail_rename(p.root.join(TMP).join("files"));
        fail_rename(p.root.join(OLD).join("files"));
        assert!(finish_pending(&p).unwrap_err().contains("old data kept"));
        assert!(!p.files_dir.exists());
        assert_eq!(fs::read_to_string(p.root.join(OLD).join("files/old.txt")).unwrap(), "old", "kept, not deleted");
        // The app opened anyway and made a fresh files/; the next plain start puts the old one back.
        fs::create_dir_all(&p.files_dir).unwrap();
        unlock_renames();
        assert_eq!(finish_pending(&p), Ok(false));
        assert_eq!(fs::read_to_string(p.files_dir.join("old.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(p.core_dir.join("mimir.db")).unwrap(), "old");
        assert!(leftovers(&p).is_empty());
    }

    #[test]
    fn failed_cleanup_after_swap_keeps_the_import() {
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        fs::create_dir_all(&p.core_dir).unwrap();
        fs::write(p.core_dir.join("mimir.db"), "old").unwrap();
        let out = tempfile::tempdir().unwrap();
        let zip = out.path().join("b.zip");
        zip_of(&zip, &[("mimir.db", b"new")]);
        stage(&p, &zip).unwrap();
        // The swap finishes, then a file held open (an AV scanner, the indexer) stops the delete of the old data.
        fail_cleanup(true);
        assert_eq!(finish_pending(&p), Ok(true));
        assert_eq!(finish_pending(&p), Ok(false), "the next start");
        assert_eq!(fs::read_to_string(p.core_dir.join("mimir.db")).unwrap(), "new", "the import stays");
        // Unlocked, a later start deletes what is left.
        fail_cleanup(false);
        assert_eq!(finish_pending(&p), Ok(false));
        assert_eq!(fs::read_to_string(p.core_dir.join("mimir.db")).unwrap(), "new");
        let names: Vec<_> = fs::read_dir(&p.root).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert!(!names.iter().any(|n| n.starts_with("restore_")), "nothing left over: {names:?}");
    }

    #[test]
    fn second_instance_never_recovers_during_a_restore() {
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        leave_old(&p, "old");
        let held = lock(&p.root).unwrap();
        assert_eq!(restore(&p), None, "another instance is restoring");
        assert_eq!(fs::read_to_string(p.root.join(OLD).join("mimir/mimir.db")).unwrap(), "old", "restore_old untouched");
        assert!(!p.core_dir.exists());
        // Two handles on the same file: the second cannot lock it while the first holds it.
        let other = File::open(p.root.join(LOCK)).unwrap();
        assert!(matches!(other.try_lock(), Err(fs::TryLockError::WouldBlock)));
        drop(held);
        other.try_lock().unwrap();
        drop(other);
        assert_eq!(restore(&p), Some(Ok(false)));
        assert_eq!(fs::read_to_string(p.core_dir.join("mimir.db")).unwrap(), "old");
        assert!(p.root.join(LOCK).exists(), "the file stays, only the lock goes");
        assert_eq!(restore(&p), Some(Ok(false)), "an unlocked leftover file is no lock");
    }

    #[test]
    fn regenerate_flag_kept_until_done() {
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        let out = tempfile::tempdir().unwrap();
        let zip = out.path().join("b.zip");
        zip_of(&zip, &[("mimir.db", b"new")]);
        stage(&p, &zip).unwrap();
        fail_rename(p.root.join(TMP).join("mimir"));
        assert!(finish_pending(&p).is_err());
        assert!(!regenerate_pending(&p.root), "nothing imported");
        unlock_renames();
        stage(&p, &zip).unwrap();
        assert_eq!(finish_pending(&p), Ok(true));
        assert!(regenerate_pending(&p.root));
        // The app died before regenerating: a plain start still asks for it.
        assert_eq!(finish_pending(&p), Ok(false));
        assert!(regenerate_pending(&p.root));
        client_id_regenerated(&p.root);
        assert!(!regenerate_pending(&p.root));
    }

    #[test]
    fn failed_stage_keeps_the_staged_zip() {
        // Two picks race: the one that fails must not delete the zip the other just staged.
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        let out = tempfile::tempdir().unwrap();
        let zip = out.path().join("b.zip");
        zip_of(&zip, &[("mimir.db", b"new")]);
        stage(&p, &zip).unwrap();
        assert!(stage(&p, &out.path().join("gone.zip")).is_err());
        assert!(p.root.join(PENDING).is_file(), "the staged zip stays");
        let names: Vec<_> = fs::read_dir(&p.root).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, [PENDING], "no temp copy left");
    }

    /// A real backup exported by Mimir Android 3.3.7, given by path in `MIMIR_ANDROID_BACKUP`.
    #[test]
    #[ignore = "needs MIMIR_ANDROID_BACKUP=<path to an Android backup zip>"]
    fn phone_backup_imports() {
        let zip = std::env::var("MIMIR_ANDROID_BACKUP").expect("set MIMIR_ANDROID_BACKUP");
        let zip = Path::new(&zip);
        validate(zip).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        stage(&p, zip).unwrap();
        assert_eq!(finish_pending(&p), Ok(true));
        let core = core::open(&p).unwrap();
        assert_eq!(core.public_key().len(), 32);
        assert!(!core.get_contacts().is_empty());
        let s = Settings::load(&p.settings_file);
        assert_eq!((s.media.voice_bitrate, s.peers.len()), (64000, 10));
        assert!(fs::read_dir(&p.files_dir).unwrap().next().is_some(), "attachments restored");
    }
}
