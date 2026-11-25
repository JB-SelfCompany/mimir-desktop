//! Persisted UI settings (settings.json).
use crate::{i18n::Lang, model::prefs_xml::{PrefEntry, PrefValue}};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub lang: Option<Lang>,
    pub dark: Option<bool>,
    pub intro_done: bool,
    pub message_font: f32,
    /// Per group, the newest ChatInfoChanged row already written into the local chat (P2-R7). Kept across restarts:
    /// re-applying an old rename would undo a newer one of our own, which the mediator does not echo back.
    pub chat_info_applied: HashMap<i64, i64>,
    pub media: MediaSettings,
    /// «Кто может отправлять вам запросы контакта» = «Никто» (`contact-requests-from` = `nobody`).
    pub block_strangers: bool,
    /// Announce priority among our own devices, 1..=100 (`client-priority`).
    pub priority: u8,
    /// Yggdrasil bootstrap peers in order (`peers`); only the enabled ones are dialled.
    pub peers: Vec<Peer>,
    /// Group call relay, 64 lowercase hex chars or empty (`call-mediator-pubkey`).
    pub call_mediator: String,
    /// Keys of the last imported Android prefs file we do not use, written back on export (P4-R2).
    pub android_extra: Vec<PrefEntry>,
}

/// A Yggdrasil peer as Android's PeerProvider stores it (`+url` enabled, `-url` disabled).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Peer {
    pub url: String,
    pub enabled: bool,
}

/// Android's message font sizes (AS §1.6).
pub const FONT_SIZES: [i64; 5] = [11, 13, 15, 18, 21];

/// PeerProvider.DEFAULT_PEERS (the core keeps the same list), all enabled.
fn default_peers() -> Vec<Peer> {
    mimir::default_ygg_peers().into_iter().map(|url| Peer { url, enabled: true }).collect()
}

/// PeerProvider.getAllPeers: `+url`/`-url` lines; empty gives the defaults; a legacy list of bare URLs is appended to
/// the defaults (enabled unless the old `defaultPeers` flag was off).
fn parse_peers(raw: &str, legacy_defaults: bool) -> Vec<Peer> {
    let lines: Vec<&str> = raw.lines().filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return default_peers();
    }
    if lines.iter().any(|l| !l.starts_with(['+', '-'])) {
        let defaults = default_peers().into_iter().map(|p| Peer { enabled: legacy_defaults, ..p });
        return defaults.chain(lines.iter().map(|l| Peer { url: l.to_string(), enabled: true })).collect();
    }
    lines.iter().map(|l| Peer { url: l[1..].into(), enabled: l.starts_with('+') }).collect()
}

/// Media preferences (P3-R1), Android 3.3.7 defaults; edited in Plan 4's settings.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct MediaSettings {
    /// Index into Android's ImageSize list (`images-format`).
    pub image_format: u8,
    /// JPEG quality for re-encoded images (`images-quality`).
    pub image_quality: u8,
    /// Auto-download caps in bytes: 0 = never, -1 = always (CA §1).
    pub auto_contacts: i64,
    pub auto_others: i64,
    pub auto_groups: i64,
    /// Voice message bitrate: 32000, 64000 or 128000.
    pub voice_bitrate: u32,
    /// File server for group attachments, hex public key (`file-server-pubkey`).
    pub file_server: String,
}

impl Default for MediaSettings {
    fn default() -> Self {
        MediaSettings {
            image_format: 0,
            image_quality: 95,
            auto_contacts: 10 << 20,
            auto_others: 0,
            auto_groups: 10 << 20,
            voice_bitrate: 32000,
            file_server: "0000128c1179f819faaa07fcada9f9803cb288bc68d278da82ca23d8b4b0ea9e".into(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            lang: None,
            dark: None,
            intro_done: false,
            message_font: 15.0,
            chat_info_applied: HashMap::new(),
            media: MediaSettings::default(),
            block_strangers: false,
            priority: 1,
            peers: default_peers(),
            call_mediator: String::new(),
            android_extra: vec![],
        }
    }
}

impl Settings {
    /// The Android-owned settings as `com.revertron.mimir_preferences.xml` entries (keys and types of AS §1, §2.1),
    /// then the kept unknown ones.
    pub fn to_android_prefs(&self) -> Vec<PrefEntry> {
        use PrefValue::{Int, Str};
        let m = &self.media;
        let peers: Vec<String> = self.peers.iter().map(|p| format!("{}{}", if p.enabled { '+' } else { '-' }, p.url)).collect();
        let mut out = vec![
            ("contact-requests-from", Str(if self.block_strangers { "nobody" } else { "everyone" }.into())),
            ("auto-download-contacts", Str(m.auto_contacts.to_string())),
            ("auto-download-others", Str(m.auto_others.to_string())),
            ("auto-download-groups", Str(m.auto_groups.to_string())),
            ("images-format", Int(m.image_format.into())),
            ("images-quality", Int(m.image_quality.into())),
            ("message-font-size", Int(self.message_font.round() as i64)),
            ("client-priority", Int(self.priority.into())),
            ("voice-quality", Str(m.voice_bitrate.to_string())),
            ("file-server-pubkey", Str(m.file_server.clone())),
            ("peers", Str(peers.join("\n"))),
        ];
        if !self.call_mediator.is_empty() {
            out.push(("call-mediator-pubkey", Str(self.call_mediator.clone())));
        }
        out.into_iter().map(|(name, value)| PrefEntry { name: name.into(), value }).chain(self.android_extra.iter().cloned()).collect()
    }

    /// Takes an imported Android prefs file with Android's replace semantics (AS §2.2): every Android-owned field gets
    /// the file's value or, when missing or unreadable, its default. Out-of-range numbers are clamped (the font
    /// snaps to the nearest size, ties to the larger); unknown keys are kept for export (P4-R2).
    pub fn apply_android_prefs(&mut self, prefs: &[PrefEntry]) {
        let d = Settings::default();
        (self.media, self.block_strangers, self.priority, self.message_font) = (d.media, d.block_strangers, d.priority, d.message_font);
        (self.peers, self.call_mediator) = (d.peers, d.call_mediator);
        self.android_extra.clear();
        let (mut peers, mut legacy_defaults) = (None, true);
        let m = &mut self.media;
        for e in prefs {
            let (num, text) = (e.value.as_int(), e.value.as_text());
            match e.name.as_str() {
                "contact-requests-from" => self.block_strangers = text.as_deref() == Some("nobody"),
                "auto-download-contacts" => m.auto_contacts = num.unwrap_or(m.auto_contacts),
                "auto-download-others" => m.auto_others = num.unwrap_or(m.auto_others),
                "auto-download-groups" => m.auto_groups = num.unwrap_or(m.auto_groups),
                "images-format" => m.image_format = num.filter(|v| (0..=3).contains(v)).map_or(m.image_format, |v| v as u8),
                "images-quality" => m.image_quality = num.map_or(m.image_quality, |v| v.clamp(60, 99) as u8),
                "voice-quality" => m.voice_bitrate = num.filter(|v| [32000, 64000, 128000].contains(v)).map_or(m.voice_bitrate, |v| v as u32),
                "file-server-pubkey" => m.file_server = text.map(|k| k.trim().to_string()).filter(|k| k.len() == 64).unwrap_or(m.file_server.clone()),
                "message-font-size" => {
                    if let Some(v) = num {
                        // Reversed so that a tie (14) picks the larger size.
                        self.message_font = FONT_SIZES.into_iter().rev().min_by_key(|s| (s - v).abs()).unwrap_or(15) as f32;
                    }
                }
                "client-priority" => self.priority = num.map_or(self.priority, |v| v.clamp(1, 100) as u8),
                "call-mediator-pubkey" => {
                    let key = text.map(|k| k.trim().to_lowercase()).filter(|k| k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit()));
                    self.call_mediator = key.unwrap_or_default();
                }
                "peers" => peers = text,
                // Read once by PeerProvider's legacy migration, then removed.
                "defaultPeers" => legacy_defaults = e.value != PrefValue::Bool(false),
                _ => self.android_extra.push(e.clone()),
            }
        }
        if let Some(raw) = peers {
            self.peers = parse_peers(&raw, legacy_defaults);
        }
    }

    /// Missing or unreadable file gives defaults. ponytail: a corrupt file is silently replaced on next save.
    pub fn load(path: &Path) -> Settings {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::prefs_xml::{parse_prefs, write_prefs};

    #[test]
    fn settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("settings.json");
        assert_eq!(Settings::load(&path), Settings::default());
        let media = MediaSettings { voice_bitrate: 64000, ..Default::default() };
        let s = Settings { lang: Some(Lang::En), dark: Some(false), intro_done: true, message_font: 18.0, chat_info_applied: HashMap::from([(7, 42)]), media, ..Default::default() };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        assert_eq!(Settings::default().message_font, 15.0);
    }

    #[test]
    fn media_defaults() {
        // P3-R1: Android 3.3.7 defaults (AM §2, §4, §7; CA §1).
        let m = Settings::default().media;
        assert_eq!((m.image_format, m.image_quality, m.voice_bitrate), (0, 95, 32000));
        assert_eq!((m.auto_contacts, m.auto_others, m.auto_groups), (10 << 20, 0, 10 << 20));
        assert_eq!(m.file_server, "0000128c1179f819faaa07fcada9f9803cb288bc68d278da82ca23d8b4b0ea9e");
        // A settings.json from before Plan 3 (no "media") and a partial "media" both fill in the defaults.
        let old: Settings = serde_json::from_str(r#"{"intro_done":true}"#).unwrap();
        assert_eq!(old.media, m);
        let partial: Settings = serde_json::from_str(r#"{"media":{"voice_bitrate":128000}}"#).unwrap();
        assert_eq!(partial.media, MediaSettings { voice_bitrate: 128000, ..m });
    }

    fn get(p: &[PrefEntry], name: &str) -> Option<PrefValue> {
        p.iter().find(|e| e.name == name).map(|e| e.value.clone())
    }

    fn entry(name: &str, value: PrefValue) -> PrefEntry {
        PrefEntry { name: name.into(), value }
    }

    #[test]
    fn settings_to_android_prefs() {
        use PrefValue::{Int, Str};
        let p = Settings::default().to_android_prefs();
        let s = |v: &str| Some(Str(v.into()));
        assert_eq!(get(&p, "contact-requests-from"), s("everyone"));
        assert_eq!(get(&p, "auto-download-contacts"), s("10485760"));
        assert_eq!(get(&p, "auto-download-others"), s("0"));
        assert_eq!(get(&p, "auto-download-groups"), s("10485760"));
        assert_eq!(get(&p, "images-format"), Some(Int(0)));
        assert_eq!(get(&p, "images-quality"), Some(Int(95)));
        assert_eq!(get(&p, "message-font-size"), Some(Int(15)));
        assert_eq!(get(&p, "client-priority"), Some(Int(1)));
        assert_eq!(get(&p, "voice-quality"), s("32000"));
        assert_eq!(get(&p, "file-server-pubkey"), s("0000128c1179f819faaa07fcada9f9803cb288bc68d278da82ca23d8b4b0ea9e"));
        let Some(Str(peers)) = get(&p, "peers") else { panic!("no peers") };
        assert_eq!(peers.lines().count(), 10);
        assert!(peers.lines().all(|l| l.starts_with('+')), "{peers}");
        assert_eq!(get(&p, "call-mediator-pubkey"), None, "no key while empty");
        // A settings.json from before Plan 4 gets the Android defaults for the new fields.
        let old: Settings = serde_json::from_str(r#"{"intro_done":true}"#).unwrap();
        assert_eq!(old, Settings { intro_done: true, ..Default::default() });
        assert_eq!((old.block_strangers, old.priority, old.peers.len(), old.call_mediator.as_str()), (false, 1, 10, ""));
    }

    #[test]
    fn android_prefs_apply() {
        let mediator = "ab".repeat(32);
        let xml = format!(
            "<map><string name=\"contact-requests-from\">nobody</string><string name=\"auto-download-others\">-1</string>\
             <string name=\"voice-quality\">128000</string><string name=\"peers\">+tcp://a:1\n-tcp://b:2</string>\
             <string name=\"call-mediator-pubkey\">{}</string><boolean name=\"enabled\" value=\"true\" /></map>",
            mediator.to_uppercase()
        );
        let mut st = Settings::default();
        st.apply_android_prefs(&parse_prefs(&xml));
        assert!(st.block_strangers);
        assert_eq!(st.media.auto_others, -1);
        assert_eq!(st.media.voice_bitrate, 128000);
        assert_eq!(st.peers, vec![Peer { url: "tcp://a:1".into(), enabled: true }, Peer { url: "tcp://b:2".into(), enabled: false }]);
        assert_eq!(st.call_mediator, mediator, "lowercased");
        assert_eq!(st.android_extra, vec![entry("enabled", PrefValue::Bool(true))]);
        let out = st.to_android_prefs();
        assert!(out.contains(&st.android_extra[0]), "unknown key re-emitted");
        assert_eq!(get(&out, "peers"), Some(PrefValue::Str("+tcp://a:1\n-tcp://b:2".into())));
        assert_eq!(get(&out, "call-mediator-pubkey"), Some(PrefValue::Str(mediator)));
        // Our own export reads back to the same settings.
        let mut again = Settings::default();
        again.apply_android_prefs(&parse_prefs(&write_prefs(&out)));
        assert_eq!(again, st);
        // Replace semantics (AS §2.2): keys missing from the file go back to their defaults.
        st.apply_android_prefs(&[]);
        assert_eq!(st, Settings::default());
    }

    #[test]
    fn odd_prefs_values_are_clamped() {
        use PrefValue::{Bool, Int, Str};
        let mut st = Settings::default();
        st.apply_android_prefs(&[
            entry("message-font-size", Int(14)),
            entry("client-priority", Int(0)),
            entry("images-quality", Int(10)),
            entry("images-format", Int(9)),
            entry("auto-download-contacts", Str("abc".into())),
            entry("voice-quality", Str("999".into())),
            entry("file-server-pubkey", Str("short".into())),
            entry("call-mediator-pubkey", Str("zz".repeat(32))),
        ]);
        assert_eq!(st.message_font, 15.0, "nearest size");
        assert_eq!(st.priority, 1);
        assert_eq!(st.media.image_quality, 60);
        assert_eq!(st.media.image_format, 0);
        assert_eq!(st.media.auto_contacts, 10 << 20);
        assert_eq!(st.media.voice_bitrate, 32000);
        assert_eq!(st.media.file_server, Settings::default().media.file_server);
        assert_eq!(st.call_mediator, "");
        // Out of range high, a number stored as a string, a wrong type.
        st.apply_android_prefs(&[entry("client-priority", Int(500)), entry("message-font-size", Str("20".into())), entry("images-quality", Bool(true))]);
        assert_eq!((st.priority, st.message_font, st.media.image_quality), (100, 21.0, 95));
    }

    #[test]
    fn legacy_peers_migrate() {
        use PrefValue::{Bool, Str};
        let defaults = mimir::default_ygg_peers();
        let mut st = Settings::default();
        st.apply_android_prefs(&[entry("peers", Str("tcp://x:1\ntcp://y:2".into()))]);
        let urls: Vec<_> = st.peers.iter().map(|p| p.url.as_str()).collect();
        assert_eq!(urls[..10], defaults.iter().map(String::as_str).collect::<Vec<_>>()[..]);
        assert_eq!(urls[10..], ["tcp://x:1", "tcp://y:2"]);
        assert!(st.peers.iter().all(|p| p.enabled));
        // PeerProvider: a legacy defaultPeers=false leaves the defaults disabled; the key is consumed.
        st.apply_android_prefs(&[entry("peers", Str("tcp://x:1".into())), entry("defaultPeers", Bool(false))]);
        assert!(st.peers[..10].iter().all(|p| !p.enabled) && st.peers[10].enabled);
        assert!(st.android_extra.is_empty());
        // Empty: the defaults.
        st.apply_android_prefs(&[entry("peers", Str(String::new()))]);
        assert_eq!(st.peers, Settings::default().peers);
    }
}
