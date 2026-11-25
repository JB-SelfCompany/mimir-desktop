//! Network start-up and re-dialling (core networking runs on the worker).
use super::{events::Bridge, worker::{WorkResult, Worker}};
use crate::{paths::DataPaths, settings::Settings};
use mimir::{AppConfig, MimirApp};

/// Android 3.3.7 ConnectionService defaults; the core's own defaults have no trackers or mediators,
/// and without trackers no contact can be found.
const TRACKERS: [&str; 2] = [
    "0000118d965a512ce8a37896957ef15b4108f89a9954ae9365448c6bf049c48d:69",
    "000044c35636ae819b55ef3f4d5008dd0125fb70baa5fc0f8a94a3671ef8c649:69",
];
const MEDIATOR: &str = "000096390fde19f30d3ba042d77f5f5ce0e6c23bd62ff0afc2e6c29a6fe96b33";

/// Core config from `<core_dir>/config.json` (or the core's defaults), with attachments in our files dir, our
/// auto-download caps (P3-R1; the core re-seeds them from here at start), priority and enabled peers (the settings
/// own them, not config.json), and Android's trackers / mediator where the config names none.
pub fn config_for(paths: &DataPaths, s: &Settings) -> AppConfig {
    let mut cfg = mimir::load_config(paths.core_dir.to_string_lossy().into_owned());
    cfg.attachments_dir = paths.files_dir.to_string_lossy().into_owned();
    let media = &s.media;
    (cfg.auto_download_contacts, cfg.auto_download_others, cfg.auto_download_groups) = (media.auto_contacts, media.auto_others, media.auto_groups);
    cfg.priority = s.priority;
    cfg.ygg_peers = enabled_peers(s);
    if cfg.trackers.is_empty() {
        cfg.trackers = TRACKERS.map(String::from).to_vec();
    }
    if cfg.default_mediators.is_empty() {
        cfg.default_mediators = vec![hex::decode(MEDIATOR).expect("valid hex")];
    }
    cfg
}

fn enabled_peers(s: &Settings) -> Vec<String> {
    s.peers.iter().filter(|p| p.enabled).map(|p| p.url.clone()).collect()
}

/// Hands the core the settings it takes live, as Android does on each change and again after every start (the
/// block flag is not in `AppConfig`): stranger requests, auto-download caps, priority, peers, then a re-announce
/// (Android's `refresh_peer`). Peers, priority and announce are no-ops before `start`; `config_for` carries them in.
/// All non-blocking (atomics and wake-ups), so callable on the UI thread.
pub fn apply_live(core: &MimirApp, s: &Settings) {
    core.set_block_stranger_contact_requests(s.block_strangers);
    let m = &s.media;
    core.set_auto_download(m.auto_contacts, m.auto_others, m.auto_groups);
    core.set_priority(s.priority);
    core.set_ygg_peers(enabled_peers(s));
    core.announce_to_trackers();
}

/// Starts the core's networking (peer, mediator, files nodes; tracker announce) on the worker.
pub fn start(w: &Worker, cfg: AppConfig, bridge: Bridge) {
    w.submit(Box::new(move |app| match app.start(cfg, Box::new(bridge)) {
        Ok(()) => WorkResult::Started,
        Err(e) => WorkResult::Failed { what: "start", err: e.to_string() },
    }));
}

/// Dials a contact on the worker (chat opened). Unsent messages are re-dialled by the core itself (every 60 s).
pub fn connect(w: &Worker, pubkey: Vec<u8>) {
    if !super::valid_key(&pubkey, "dial") {
        return;
    }
    // ponytail: a failed dial is dropped; the core's retry loop or the next chat open dials again.
    w.submit(Box::new(move |app| {
        let _ = app.connect_to_contact(pubkey);
        WorkResult::Done
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::MediaSettings;

    #[test]
    fn config_puts_attachments_in_files_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        let cfg = config_for(&p, &Settings::default());
        assert_eq!(cfg.attachments_dir, p.files_dir.to_string_lossy());
        assert_eq!(cfg.ygg_peers, mimir::default_ygg_peers());
        assert_eq!(cfg.priority, 1);
        assert_eq!(cfg.trackers.len(), 2);
        assert_eq!(cfg.default_mediators[0].len(), 32);
    }

    #[test]
    fn config_uses_media_caps() {
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        let media = MediaSettings { auto_contacts: -1, auto_others: 1024, auto_groups: 0, ..Default::default() };
        let cfg = config_for(&p, &Settings { media, ..Default::default() });
        assert_eq!((cfg.auto_download_contacts, cfg.auto_download_others, cfg.auto_download_groups), (-1, 1024, 0));
        assert_eq!(cfg.attachments_dir, p.files_dir.to_string_lossy());
    }

    #[test]
    fn config_uses_settings_peers_and_priority() {
        use crate::settings::Peer;
        let tmp = tempfile::tempdir().unwrap();
        let p = DataPaths::under(tmp.path());
        let peers = vec![Peer { url: "tcp://a:1".into(), enabled: true }, Peer { url: "tcp://b:2".into(), enabled: false }];
        let cfg = config_for(&p, &Settings { priority: 7, peers, ..Default::default() });
        assert_eq!((cfg.priority, cfg.ygg_peers), (7, vec!["tcp://a:1".to_string()]));
    }

    /// Run with: cargo test --lib -- --ignored two_nodes_exchange_text --nocapture
    #[test]
    #[ignore = "real network: two nodes over the default Yggdrasil peers, up to 120 s"]
    fn two_nodes_exchange_text() {
        use crate::{core::events::Event, test_support::test_app};
        use std::{sync::mpsc::channel, time::{Duration, Instant}};
        let ((ta, a), (tb, b)) = (test_app(), test_app());
        let ((txa, rxa), (txb, rxb)) = (channel(), channel());
        let ctx = eframe::egui::Context::default();
        a.start(config_for(&DataPaths::under(ta.path()), &Default::default()), Box::new(Bridge::new(txa, ctx.clone()))).unwrap();
        b.start(config_for(&DataPaths::under(tb.path()), &Default::default()), Box::new(Bridge::new(txb, ctx))).unwrap();
        let b_in_a = a.add_contact(b.public_key(), "B".into()).unwrap();
        b.add_contact(a.public_key(), "A".into()).unwrap();
        // Errs with "no connection" while B is not reachable yet; the row is stored and retried.
        let _ = a.send_contact_message(b.public_key(), 0, "ping".into(), None);
        let guid = a.get_last_message(b_in_a).unwrap().guid;
        let start = Instant::now();
        let (mut received, mut delivered, mut dialled) = (false, false, start);
        while !(received && delivered) && start.elapsed() < Duration::from_secs(120) {
            for e in rxb.try_iter() {
                if let Event::MessageReceived { message_id, .. } = e {
                    received |= b.get_message(message_id).and_then(|m| m.data).as_deref() == Some(b"ping".as_slice());
                }
            }
            delivered |= rxa.try_iter().any(|e| matches!(e, Event::MessageDelivered { guid: g, .. } if g == guid));
            if dialled.elapsed() > Duration::from_secs(20) {
                dialled = Instant::now(); // what the core's retry loop does, sooner
                let _ = a.connect_to_contact(b.public_key());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        println!("received={received} delivered={delivered} after {:?}", start.elapsed());
        assert!(received && delivered);
    }
}
