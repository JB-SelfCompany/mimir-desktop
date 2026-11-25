//! One running client per data dir: the first launch holds `<root>/.instance.lock` and listens on a loopback port
//! (written to `<root>/.instance.port`); later launches send it their link line (empty: «show yourself») and exit.
//! A relaunch (`RELAUNCHED` set: backup import) first waits for the quitting instance to let go of the lock.
use std::{
    fs::{self, File, TryLockError},
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::Path,
    time::{Duration, Instant},
};

const LOCK: &str = ".instance.lock";
const PORT: &str = ".instance.port";
/// The primary's first line on each connection: a stale port file may name some other local service by now.
const HELLO: &str = "mimir-desktop";

/// Set in the environment of the process a backup import relaunches.
pub const RELAUNCHED: &str = "MIMIR_RELAUNCHED";
/// How long a relaunch waits for the lock.
pub const RELAUNCH_WAIT: Duration = Duration::from_secs(10);

pub enum Acquire {
    Primary(Instance),
    /// Another instance runs on this data dir and got our link: exit.
    Forwarded,
}

/// The lock and the listener of the primary instance; both live as long as the process once `listen` runs.
pub struct Instance {
    lock: File,
    listener: TcpListener,
}

impl Instance {
    /// Calls `on_msg` on its own thread for each forwarded launch: `Some(link)`, or `None` for a bare launch.
    pub fn listen(self, on_msg: impl Fn(Option<String>) + Send + 'static) {
        let Instance { lock, listener } = self;
        std::thread::spawn(move || {
            let _lock = lock;
            for stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = writeln!(&stream, "{HELLO}");
                let mut line = String::new();
                // A link is short: a stray client cannot make us buffer more.
                if BufReader::new(stream.take(4096)).read_line(&mut line).is_ok_and(|n| n > 0) {
                    let line = line.trim();
                    on_msg((!line.is_empty()).then(|| line.to_string()));
                }
            }
        });
    }
}

/// `wait`: how long a held lock is retried before the link is forwarded to its holder.
pub fn acquire(root: &Path, link: Option<&str>, wait: Duration) -> Acquire {
    // ponytail: an unwritable data dir panics here, as core::open would a moment later.
    try_acquire(root, link, wait).expect("cannot take the single-instance lock")
}

fn try_acquire(root: &Path, link: Option<&str>, wait: Duration) -> io::Result<Acquire> {
    fs::create_dir_all(root)?;
    let lock = File::options().create(true).truncate(false).write(true).open(root.join(LOCK))?;
    let deadline = Instant::now() + wait;
    loop {
        match lock.try_lock() {
            Ok(()) => {
                // Any port file left by a crashed instance is overwritten.
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
                fs::write(root.join(PORT), listener.local_addr()?.port().to_string())?;
                return Ok(Acquire::Primary(Instance { lock, listener }));
            }
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(TryLockError::WouldBlock) => {
                forward(root, link.unwrap_or(""), 30);
                return Ok(Acquire::Forwarded);
            }
            Err(TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// The primary may still be between taking the lock and writing its port (the file then names a crashed run's port):
/// `tries` times, 100 ms apart. Whether the line reached a Mimir instance.
fn forward(root: &Path, line: &str, tries: u32) -> bool {
    for _ in 0..tries {
        let port = fs::read_to_string(root.join(PORT)).ok().and_then(|p| p.trim().parse::<u16>().ok());
        if port.and_then(|p| TcpStream::connect((Ipv4Addr::LOCALHOST, p)).ok()).is_some_and(|s| send_to_mimir(s, line)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    tracing::warn!("could not reach the running Mimir instance; link dropped");
    false
}

/// Sends `line` once the listener has greeted with `HELLO`: nothing goes to a service that is not Mimir.
fn send_to_mimir(mut s: TcpStream, line: &str) -> bool {
    let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
    let mut hello = String::new();
    let greeted = BufReader::new((&s).take(64)).read_line(&mut hello).is_ok() && hello.trim_end() == HELLO;
    greeted && writeln!(s, "{line}").is_ok()
}

/// The first argument that is a Mimir link (`mimir://…` or `https://go.mimir.im/…`).
pub fn link_arg(mut args: impl Iterator<Item = String>) -> Option<String> {
    args.find(|a| a.starts_with("mimir://") || a.starts_with("https://go.mimir.im/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn link_arg_picks_scheme() {
        let pick = |v: &[&str]| link_arg(v.iter().map(|s| s.to_string()));
        assert_eq!(pick(&["mimir.exe", "--flag", "mimir://mm/u/ab"]), Some("mimir://mm/u/ab".into()));
        assert_eq!(pick(&["mimir.exe", "https://go.mimir.im/g/x/1#k"]), Some("https://go.mimir.im/g/x/1#k".into()));
        assert_eq!(pick(&["mimir.exe", "https://example.com/mimir://x"]), None);
        assert_eq!(pick(&["mimir.exe"]), None);
    }

    // ponytail: the listener thread keeps the lock file open, so the temp dir outlives the test on Windows.
    #[test]
    fn second_instance_forwards_link() {
        let tmp = tempfile::tempdir().unwrap();
        let Acquire::Primary(first) = acquire(tmp.path(), None, Duration::ZERO) else { panic!("the first launch is the primary") };
        let (tx, rx) = std::sync::mpsc::channel();
        first.listen(move |m| {
            let _ = tx.send(m);
        });
        assert!(matches!(acquire(tmp.path(), Some("mimir://mm/u/ab"), Duration::ZERO), Acquire::Forwarded));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Some("mimir://mm/u/ab".into()));
        assert!(matches!(acquire(tmp.path(), None, Duration::ZERO), Acquire::Forwarded));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), None, "a bare launch asks to be shown");
    }

    #[test]
    fn relaunch_waits_for_the_quitting_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let Acquire::Primary(old) = acquire(tmp.path(), None, Duration::ZERO) else { panic!("the first launch is the primary") };
        // The old instance quits shortly after the relaunch starts waiting.
        let quit = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(old);
        });
        assert!(matches!(acquire(tmp.path(), None, RELAUNCH_WAIT), Acquire::Primary(_)), "the relaunch takes over");
        quit.join().unwrap();
    }

    #[test]
    fn stale_port_of_another_service_gets_no_link() {
        // The file names a port some other local service took after a crash; it answers, but not as Mimir.
        let tmp = tempfile::tempdir().unwrap();
        let other = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        fs::write(tmp.path().join(PORT), other.local_addr().unwrap().port().to_string()).unwrap();
        let service = std::thread::spawn(move || {
            let mut got = vec![];
            for mut s in other.incoming().take(3).flatten() {
                s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").unwrap();
                s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let _ = s.read_to_end(&mut got);
            }
            got
        });
        assert!(!forward(tmp.path(), "mimir://mm/u/ab", 3));
        assert!(service.join().unwrap().is_empty(), "the link went nowhere");
    }

    #[test]
    fn stale_port_file_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(PORT), "garbage").unwrap();
        let Acquire::Primary(i) = acquire(tmp.path(), Some("mimir://mm/u/ab"), Duration::ZERO) else { panic!("no lock holder: primary") };
        let port: u16 = fs::read_to_string(tmp.path().join(PORT)).unwrap().parse().unwrap();
        assert_eq!(port, i.listener.local_addr().unwrap().port());
    }
}
