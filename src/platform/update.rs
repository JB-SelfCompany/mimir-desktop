//! Self-update from the GitHub Releases of mimir-desktop. A thread checks \`releases/latest\` at start and once a day
//! (setting «Автоматическая проверка обновлений»); a newer release shows a banner over the chat list and a notification.
//! «Обновить» downloads this platform's archive next to the executable, checks it against the release's \`SHA256SUMS\`,
//! takes the executable out of it and swaps it in: the running one is renamed to \`.old\` (removed at the next start),
//! then the app relaunches. Any failure leaves the current executable in place and says why. A package install (an
//! executable under /usr or /opt: the .deb) is only told: its banner opens the release page.
//! No release yet (\`releases/latest\` answers 404) reads as «up to date».
use crate::{
    app::MimirDesktop,
    core::{events::Event, worker::WorkResult},
    i18n::{tr, trf, Key, Lang},
    platform::notify,
    settings::Settings,
};
use eframe::egui;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

const LATEST: &str = "https://api.github.com/repos/JB-SelfCompany/mimir-desktop/releases/latest";
const EVERY: Duration = Duration::from_secs(24 * 3600);
/// How often the checker re-reads the setting, so turning it on does not wait for `EVERY`.
const POLL: Duration = Duration::from_secs(60);
/// The release asset with \`sha256sum\` lines for the others.
const SUMS: &str = "SHA256SUMS";
/// The executable's name inside a release archive.
const EXE: &str = if cfg!(windows) { "mimir-desktop.exe" } else { "mimir-desktop" };

/// GitHub's release JSON, the fields we use.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

impl Release {
    /// The tag without its \`v\`.
    pub fn version(&self) -> &str {
        self.tag_name.trim_start_matches('v')
    }
}

/// How this copy was installed, which decides what an update can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    /// The zip or the per-user installer: the exe is swapped from the zip (ffmpeg next to it stays).
    Windows,
    /// The tar.gz, anywhere but /usr and /opt: the executable is swapped from it.
    LinuxArchive,
    /// The .deb (or an OS with no release archive): notify only.
    Package,
}

/// The update's state for the UI.
#[derive(Default)]
pub struct UpdateUi {
    /// The newest release found, newer than us.
    pub found: Option<Release>,
    /// «Проверить обновления» runs.
    pub checking: bool,
    /// The update downloads and is swapped in.
    pub installing: bool,
    /// The last manual check's result and whether it failed.
    pub result: Option<(String, bool)>,
}

/// Whether \`tag\` (\`v1.2.3\`) is a newer version than \`current\`; an unreadable one is not.
pub fn is_newer(tag: &str, current: &str) -> bool {
    match (semver::Version::parse(tag.trim_start_matches('v')), semver::Version::parse(current)) {
        (Ok(tag), Ok(current)) => tag > current,
        _ => false,
    }
}

/// The install kind of an executable at \`exe\` on \`os\` (\`std::env::consts::OS\`).
fn kind(os: &str, exe: &Path) -> Install {
    match os {
        "windows" => Install::Windows,
        "linux" if !(exe.starts_with("/usr") || exe.starts_with("/opt")) => Install::LinuxArchive,
        _ => Install::Package,
    }
}

/// Whether this copy writes its own `mimir://` desktop entry: a package ships one (with the icon) that a user-level copy would shadow.
pub fn registers_scheme(install: Install) -> bool {
    install != Install::Package
}

pub fn this_install() -> Install {
    kind(std::env::consts::OS, &std::env::current_exe().unwrap_or_default())
}

/// The release archive \`install\` updates from (asset names fixed with the release workflow).
fn asset(release: &Release, install: Install) -> Option<&Asset> {
    let name = match install {
        Install::Windows => format!("mimir-desktop-{}-windows-x86_64.zip", release.tag_name),
        Install::LinuxArchive => format!("mimir-desktop-{}-linux-x86_64.tar.gz", release.tag_name),
        Install::Package => return None,
    };
    release.assets.iter().find(|a| a.name == name)
}

/// Whether this copy can install \`release\` itself.
pub fn installable(release: &Release) -> bool {
    asset(release, this_install()).is_some()
}

/// The hash \`sums\` (\`sha256sum\` output: \`<hex>  <name>\`, or \`*<name>\` in binary mode) gives for \`name\`.
fn expected_hash<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let (hash, file) = (parts.next()?, parts.next()?);
        (file.trim_start_matches('*') == name).then_some(hash)
    })
}

/// The release in \`body\` (GitHub's \`releases/latest\` JSON), if it is newer than \`current\`.
fn newer(body: &[u8], current: &str) -> Result<Option<Release>, String> {
    let release: Release = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    Ok(is_newer(&release.tag_name, current).then_some(release))
}

/// The latest release, if newer than this build; \`None\` also while the repository has no release (404).
pub fn check() -> Result<Option<Release>, String> {
    let mut body = Vec::new();
    if !get(LATEST, &mut body)? {
        return Ok(None);
    }
    newer(&body, env!("CARGO_PKG_VERSION"))
}

/// GET \`url\` (redirects followed) into \`out\`; \`false\` for a 404.
#[cfg(not(test))]
fn get(url: &str, out: &mut dyn io::Write) -> Result<bool, String> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(30))).timeout_per_read(Some(Duration::from_secs(60))).build().into();
    // GitHub's API refuses requests without a User-Agent.
    match agent.get(url).header("User-Agent", concat!("mimir-desktop/", env!("CARGO_PKG_VERSION"))).call() {
        Ok(mut resp) => io::copy(&mut resp.body_mut().as_reader(), out).map(|_| true).map_err(|e| e.to_string()),
        Err(ureq::Error::StatusCode(404)) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

/// Tests never reach the network.
#[cfg(test)]
fn get(_url: &str, _out: &mut dyn io::Write) -> Result<bool, String> {
    Err("no network in tests".into())
}

/// \`path\` with \`suffix\` appended to its file name (\`mimir-desktop.exe.old\`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = OsString::from(path);
    s.push(suffix);
    s.into()
}

fn text(e: impl ToString) -> String {
    e.to_string()
}

/// Downloads the archive \`install\` updates from (next to \`exe\`: the swap is a rename on one file system), verifies it
/// and swaps its executable in for \`exe\`.
/// ponytail: the archive is downloaded whole (ffmpeg included) for the one executable in it.
pub fn install(release: &Release, install: Install, exe: &Path) -> Result<(), String> {
    let asset = asset(release, install).ok_or("no archive for this system")?;
    let sums_asset = release.assets.iter().find(|a| a.name == SUMS).ok_or(format!("no {SUMS} in the release"))?;
    let mut sums = Vec::new();
    if !get(&sums_asset.browser_download_url, &mut sums)? {
        return Err(format!("{SUMS}: 404"));
    }
    let archive = with_suffix(exe, ".download");
    let result = File::create(&archive)
        .map_err(text)
        .and_then(|mut f| get(&asset.browser_download_url, &mut f))
        .and_then(|found| if found { Ok(()) } else { Err(format!("{}: 404", asset.name)) })
        .and_then(|()| apply(&archive, &asset.name, &String::from_utf8_lossy(&sums), exe));
    let _ = fs::remove_file(&archive);
    result
}

/// Checks \`archive\` (the asset \`name\`) against \`sums\`, takes the executable out of it and swaps it in for \`exe\`.
fn apply(archive: &Path, name: &str, sums: &str, exe: &Path) -> Result<(), String> {
    let want = expected_hash(sums, name).ok_or(format!("{name} is not in {SUMS}"))?;
    // ponytail: hashed in memory (the archive is tens of MB).
    let got = hex::encode(Sha256::digest(fs::read(archive).map_err(text)?));
    if !got.eq_ignore_ascii_case(want) {
        return Err(format!("{name}: SHA-256 mismatch"));
    }
    let new = with_suffix(exe, ".new");
    let result = extract(archive, name, &new).and_then(|()| swap(exe, &new).map_err(text));
    let _ = fs::remove_file(&new);
    result
}

/// Writes the executable in \`archive\` (a .zip, else a .tar.gz) to \`to\`.
fn extract(archive: &Path, name: &str, to: &Path) -> Result<(), String> {
    let missing = || format!("{name} has no {EXE}");
    if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(File::open(archive).map_err(text)?).map_err(text)?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(text)?;
            if entry.is_file() && entry.name().map_err(text)?.rsplit('/').next() == Some(EXE) {
                io::copy(&mut entry, &mut File::create(to).map_err(text)?).map_err(text)?;
                return Ok(());
            }
        }
        return Err(missing());
    }
    // ponytail: the system tar (every Linux has one; Windows 10+ too) instead of a tar reader of our own.
    let dir = with_suffix(to, ".d");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(text)?;
    let result = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(&dir)
        .status()
        .map_err(text)
        .and_then(|s| if s.success() { Ok(()) } else { Err(format!("tar: {s}")) })
        .and_then(|()| {
            // \`<name>/mimir-desktop\` as the release workflow packs it, or at the top.
            let subdirs = fs::read_dir(&dir).map_err(text)?.flatten().map(|e| e.path().join(EXE));
            let found = subdirs.chain([dir.join(EXE)]).find(|p| p.is_file()).ok_or_else(missing)?;
            fs::rename(found, to).map_err(text)
        });
    let _ = fs::remove_dir_all(&dir);
    result
}

/// Puts \`new\` in place of \`exe\`, which becomes \`exe.old\` (a running executable can be renamed, also on Windows);
/// if \`new\` cannot be put there, \`exe\` is renamed back.
fn swap(exe: &Path, new: &Path) -> io::Result<()> {
    swap_with(exe, new, |from, to| fs::rename(from, to))
}

fn swap_with(exe: &Path, new: &Path, rename: impl Fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    let old = with_suffix(exe, ".old");
    let _ = fs::remove_file(&old);
    rename(exe, &old)?;
    // The old file's permissions (its exec bits on Linux).
    let put = fs::metadata(&old).and_then(|m| fs::set_permissions(new, m.permissions())).and_then(|()| rename(new, exe));
    if let Err(e) = put {
        if let Err(back) = rename(&old, exe) {
            tracing::error!("update rollback failed, {} is at {}: {back}", exe.display(), old.display());
            return Err(io::Error::other(format!("{e}; restoring {} failed ({back}): reinstall Mimir", exe.display())));
        }
        return Err(e);
    }
    Ok(())
}

/// Removes the executable the last update replaced. Windows keeps it locked until the old process has exited, which a
/// relaunch only just waited for: retried for a while.
pub fn remove_old() {
    let Ok(exe) = std::env::current_exe() else { return };
    let old = with_suffix(&exe, ".old");
    for _ in 0..20 {
        if !old.exists() || fs::remove_file(&old).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The daily check, on a thread of its own for the app's life. The setting and the language are read from
/// \`settings_file\` each time (every change is saved at once). A new version is notified once; an error only logged.
pub fn spawn_checker(settings_file: PathBuf, tx: Sender<Event>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let mut told: Option<String> = None;
        let mut last: Option<Instant> = None;
        loop {
            let settings = Settings::load(&settings_file);
            if !settings.check_updates {
                last = None;
            }
            if check_due(settings.check_updates, last, Instant::now()) {
                last = Some(Instant::now());
                let result = check();
                match &result {
                    Ok(Some(r)) if told.as_deref() != Some(&r.tag_name) => {
                        told = Some(r.tag_name.clone());
                        let lang = settings.lang.unwrap_or_else(Lang::from_system);
                        let body = trf(lang, Key::NewVersionAvailable, &[r.version()]);
                        notify::show(tr(lang, Key::UpdatesTitle).into(), body, false, lang, tx.clone(), ctx.clone());
                    }
                    Err(e) => tracing::warn!("update check failed: {e}"),
                    _ => {}
                }
                if tx.send(Event::WorkDone(WorkResult::UpdateChecked { manual: false, result })).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
            std::thread::sleep(POLL);
        }
    });
}

/// Whether the background check is due: on, and none yet since the setting was turned on or `EVERY` has passed.
fn check_due(on: bool, last: Option<Instant>, now: Instant) -> bool {
    on && last.is_none_or(|t| now.duration_since(t) >= EVERY)
}

/// «Проверить обновления»: the check on a thread of its own (the worker lanes are for the core).
pub fn check_now(app: &mut MimirDesktop) {
    let u = &mut app.state.update;
    (u.checking, u.result) = (true, None);
    let worker = app.worker.clone();
    std::thread::spawn(move || {
        let result = check();
        worker.submit(Box::new(move |_| WorkResult::UpdateChecked { manual: true, result }));
    });
}

pub fn checked(app: &mut MimirDesktop, manual: bool, result: Result<Option<Release>, String>) {
    let lang = app.lang;
    let u = &mut app.state.update;
    if manual {
        u.checking = false;
        u.result = Some(match &result {
            Ok(Some(r)) => (trf(lang, Key::NewVersionAvailable, &[r.version()]), false),
            Ok(None) => (tr(lang, Key::AlreadyLastVersion).into(), false),
            Err(e) => (trf(lang, Key::ErrorCheckingUpdates, &[e]), true),
        });
    }
    if let Ok(Some(r)) = result {
        u.found = Some(r);
    }
}

/// The chat list banner while a newer version is known.
pub fn banner_text(app: &MimirDesktop) -> Option<String> {
    let u = &app.state.update;
    let r = u.found.as_ref()?;
    let found = trf(app.lang, Key::NewVersionAvailable, &[r.version()]);
    Some(match () {
        _ if u.installing => tr(app.lang, Key::DownloadingUpdate).into(),
        _ if installable(r) => format!("{found} — {}", tr(app.lang, Key::UpdateNow)),
        _ => found,
    })
}

/// «Обновить» (the banner or the About row): the update on a thread of its own; a package install gets the release page.
pub fn start(app: &mut MimirDesktop, ctx: &egui::Context) {
    let Some(release) = app.state.update.found.clone().filter(|_| !app.state.update.installing) else { return };
    let kind = this_install();
    if asset(&release, kind).is_none() {
        ctx.open_url(egui::OpenUrl::new_tab(&release.html_url));
        return;
    }
    app.state.update.installing = true;
    let worker = app.worker.clone();
    std::thread::spawn(move || {
        let exe = std::env::current_exe();
        let err = exe.as_ref().map_err(text).and_then(|exe| install(&release, kind, exe)).err();
        let exe = exe.ok();
        worker.submit(Box::new(move |_| WorkResult::UpdateInstalled { exe, err }));
    });
}

pub fn installed(app: &mut MimirDesktop, ctx: &egui::Context, exe: Option<PathBuf>, err: Option<String>) {
    match err {
        Some(e) => {
            app.state.update.installing = false;
            app.error_text(trf(app.lang, Key::UpdateFailed, &[&e]));
        }
        None => crate::ui::settings::relaunch(app, ctx, exe),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_harness;
    use egui_kittest::{kittest::Queryable as _, Harness};
    use std::io::Write as _;
    use zip::{write::SimpleFileOptions, ZipWriter};

    fn release(tag: &str, names: &[&str]) -> Release {
        let assets = names.iter().map(|n| Asset { name: n.to_string(), browser_download_url: format!("https://example.invalid/{n}") }).collect();
        Release { tag_name: tag.into(), html_url: format!("https://example.invalid/{tag}"), assets }
    }

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    /// A temp dir with the installed executable (\`old\`) and a release zip whose \`dir/EXE\` is \`new\`.
    fn setup(zip_entries: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf, PathBuf, Vec<u8>) {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join(EXE);
        fs::write(&exe, b"old").unwrap();
        let archive = tmp.path().join("a.zip");
        let mut zip = ZipWriter::new(File::create(&archive).unwrap());
        for (name, bytes) in zip_entries {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        let bytes = fs::read(&archive).unwrap();
        (tmp, exe, archive, bytes)
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn only_a_newer_version_counts() {
        assert!(is_newer("v1.0.1", "1.0.0"));
        assert!(is_newer("1.10.0", "1.9.3"), "numeric, not text order");
        assert!(is_newer("v1.0.0", "1.0.0-rc.1"));
        assert!(!is_newer("v1.0.0", "1.0.0"));
        assert!(!is_newer("v0.9.9", "1.0.0"));
        assert!(!is_newer("nightly", "1.0.0"), "an unreadable tag is ignored");
    }

    #[test]
    fn latest_release_json_is_read() {
        let body = br#"{"tag_name":"v1.2.0","html_url":"https://github.com/x/releases/v1.2.0","assets":[{"name":"SHA256SUMS","browser_download_url":"https://github.com/x/SHA256SUMS","size":1}],"body":"notes"}"#;
        let r = newer(body, "1.0.0").unwrap().unwrap();
        assert_eq!((r.version(), r.assets[0].name.as_str()), ("1.2.0", "SHA256SUMS"));
        assert_eq!(newer(body, "1.2.0"), Ok(None), "not newer: nothing to do");
        assert!(newer(b"<html>", "1.0.0").is_err());
    }

    #[test]
    fn install_kind_by_system_and_place() {
        assert_eq!(kind("windows", Path::new(r"C:\Users\u\AppData\Local\Programs\Mimir\mimir-desktop.exe")), Install::Windows);
        assert_eq!(kind("linux", Path::new("/usr/bin/mimir-desktop")), Install::Package);
        assert_eq!(kind("linux", Path::new("/opt/mimir/mimir-desktop")), Install::Package);
        assert_eq!(kind("linux", Path::new("/home/u/apps/mimir-desktop/mimir-desktop")), Install::LinuxArchive);
        assert_eq!(kind("macos", Path::new("/Applications/Mimir")), Install::Package);
    }

    #[test]
    fn asset_for_each_install() {
        let r = release("v1.2.0", &[
            "mimir-desktop-v1.2.0-windows-x86_64-setup.exe",
            "mimir-desktop-v1.2.0-windows-x86_64.zip",
            "mimir-desktop-v1.2.0-linux-x86_64.tar.gz",
            "mimir-desktop_1.2.0_amd64.deb",
            "SHA256SUMS",
        ]);
        let name = |i| asset(&r, i).map(|a| a.name.as_str());
        assert_eq!(name(Install::Windows), Some("mimir-desktop-v1.2.0-windows-x86_64.zip"));
        assert_eq!(name(Install::LinuxArchive), Some("mimir-desktop-v1.2.0-linux-x86_64.tar.gz"));
        assert_eq!(name(Install::Package), None, "the .deb is only notified");
        assert!(!registers_scheme(Install::Package) && registers_scheme(Install::LinuxArchive) && registers_scheme(Install::Windows));
        assert_eq!(asset(&release("v1.2.0", &["SHA256SUMS"]), Install::Windows), None, "a release without our archive");
    }

    #[test]
    fn hash_read_from_sums() {
        let sums = "aa11  mimir-desktop-v1.2.0-linux-x86_64.tar.gz\nBB22 *mimir-desktop-v1.2.0-windows-x86_64.zip\n";
        assert_eq!(expected_hash(sums, "mimir-desktop-v1.2.0-windows-x86_64.zip"), Some("BB22"));
        assert_eq!(expected_hash(sums, "mimir-desktop-v1.2.0-linux-x86_64.tar.gz"), Some("aa11"));
        assert_eq!(expected_hash(sums, "other.zip"), None);
    }

    #[test]
    fn checksum_mismatch_keeps_the_old_exe() {
        let (tmp, exe, archive, bytes) = setup(&[(&format!("d/{EXE}"), b"new")]);
        let sums = format!("{}  a.zip\n", sha(b"something else"));
        let err = apply(&archive, "a.zip", &sums, &exe).unwrap_err();
        assert!(err.contains("SHA-256"), "{err}");
        assert_eq!(fs::read(&exe).unwrap(), b"old");
        assert_eq!(leftovers(tmp.path()), ["a.zip", EXE], "no .new or .old");
        // A sums file without the archive is refused the same way.
        assert!(apply(&archive, "a.zip", &format!("{}  b.zip", sha(&bytes)), &exe).is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"old");
    }

    #[test]
    fn verified_zip_swaps_the_exe() {
        let (tmp, exe, archive, bytes) = setup(&[("d/LICENSE", b"l"), ("d/ffmpeg.exe", b"f"), (&format!("d/{EXE}"), b"new")]);
        apply(&archive, "a.zip", &format!("{}  a.zip\n", sha(&bytes)), &exe).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"new");
        assert_eq!(fs::read(with_suffix(&exe, ".old")).unwrap(), b"old", "the running exe moved aside");
        assert_eq!(leftovers(tmp.path()), ["a.zip".to_string(), EXE.into(), format!("{EXE}.old")], "only the exe is taken");
    }

    #[test]
    fn archive_without_the_exe_keeps_the_old_one() {
        let (tmp, exe, archive, bytes) = setup(&[("d/README.md", b"r")]);
        let err = apply(&archive, "a.zip", &format!("{}  a.zip", sha(&bytes)), &exe).unwrap_err();
        assert!(err.contains(EXE), "{err}");
        assert_eq!(fs::read(&exe).unwrap(), b"old");
        assert_eq!(leftovers(tmp.path()), ["a.zip", EXE]);
    }

    #[test]
    fn verified_tar_gz_swaps_the_exe() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join(EXE);
        fs::write(&exe, b"old").unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("d")).unwrap();
        fs::write(src.join("d").join(EXE), b"new").unwrap();
        let archive = tmp.path().join("a.tar.gz");
        let status = std::process::Command::new("tar").arg("-czf").arg(&archive).arg("-C").arg(&src).arg("d").status().unwrap();
        assert!(status.success());
        let sums = format!("{}  a.tar.gz", sha(&fs::read(&archive).unwrap()));
        apply(&archive, "a.tar.gz", &sums, &exe).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"new");
        assert!(!with_suffix(&exe, ".new.d").exists(), "the scratch dir is gone");
    }

    #[test]
    fn failed_rollback_says_to_reinstall() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join(EXE);
        fs::write(&exe, b"old").unwrap();
        let err = swap_with(&exe, &tmp.path().join("missing"), |from, to| if to == exe { Err(io::Error::other("locked")) } else { fs::rename(from, to) }).unwrap_err();
        assert!(err.to_string().contains("reinstall") && err.to_string().contains("locked"), "{err}");
        assert!(!exe.exists());
    }

    #[test]
    fn check_due_when_turned_on_or_a_day_passed() {
        let now = Instant::now();
        let ago = |s| now - Duration::from_secs(s);
        assert!(!check_due(false, None, now));
        assert!(check_due(true, None, now), "just turned on");
        assert!(!check_due(true, Some(ago(60)), now));
        assert!(check_due(true, Some(ago(EVERY.as_secs())), now));
    }

    #[test]
    fn failed_swap_rolls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join(EXE);
        fs::write(&exe, b"old").unwrap();
        assert!(swap(&exe, &tmp.path().join("missing")).is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"old");
        assert!(!with_suffix(&exe, ".old").exists());
    }

    fn found(h: &mut Harness<'static, MimirDesktop>, manual: bool, result: Result<Option<Release>, String>) {
        h.state().events.send(Event::WorkDone(WorkResult::UpdateChecked { manual, result })).unwrap();
        h.run();
    }

    /// Steps until \`done\` (a thread of ours reports through the worker), at most 5 s.
    fn wait(h: &mut Harness<'static, MimirDesktop>, done: impl Fn(&MimirDesktop) -> bool) {
        let t = std::time::Instant::now();
        while !done(h.state()) {
            assert!(t.elapsed() < Duration::from_secs(5), "timed out");
            std::thread::sleep(Duration::from_millis(20));
            h.step();
        }
        h.run();
    }

    #[test]
    fn found_update_shows_a_banner_and_a_failed_install_says_why() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.run();
        assert!(h.query_by_label_contains("Доступна новая версия").is_none());
        found(&mut h, false, Ok(None));
        assert!(h.query_by_label_contains("Доступна новая версия").is_none(), "up to date (also: no releases yet)");
        let archive = if cfg!(windows) { "mimir-desktop-v9.0.0-windows-x86_64.zip" } else { "mimir-desktop-v9.0.0-linux-x86_64.tar.gz" };
        found(&mut h, false, Ok(Some(release("v9.0.0", &[archive, SUMS]))));
        h.get_by_label("Доступна новая версия 9.0.0 — Обновить").click();
        h.run();
        wait(&mut h, |a| !a.state.update.installing);
        let status = h.state().state.status.as_ref().map(|s| s.0.clone());
        assert_eq!(status.as_deref(), Some("Обновление не установлено: no network in tests"));
        assert!(!h.state().state.quitting, "the old version keeps running");
        assert!(h.query_by_label("Доступна новая версия 9.0.0 — Обновить").is_some(), "it can be retried");
    }

    #[test]
    fn about_has_the_update_rows() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        crate::ui::settings::open(h.state_mut());
        h.state_mut().state.settings_ui.section = crate::ui::settings::Section::About;
        h.run();
        h.get_by_label("Автоматическая проверка обновлений").click();
        h.run();
        assert!(!Settings::load(&h.state().paths.settings_file).check_updates, "saved at once");
        h.get_by_label("Проверить обновления").click();
        h.run();
        wait(&mut h, |a| !a.state.update.checking);
        assert!(h.query_by_label("Ошибка проверки обновлений: no network in tests").is_some());
        found(&mut h, true, Ok(None));
        assert!(h.query_by_label("Новых версий не обнаружено").is_some());
    }
}
