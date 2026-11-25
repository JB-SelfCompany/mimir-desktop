//! Decoding of stored message bodies (types as in Android MainActivity.getStoredMessageText).
use crate::{i18n::{tr, Key, Lang}, model::attachment::{kind, AttachmentKind, AttachmentMeta}};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub enum MsgBody {
    Text(String),
    Image(AttachmentMeta),
    /// A file, or an `audio/*` file that is not a voice message.
    File(AttachmentMeta),
    Voice(AttachmentMeta),
    VideoMessage(AttachmentMeta),
    Call,
    Reaction { emoji: String, target_guid: i64 },
    Unsupported,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reaction {
    emoji: String,
    reply_to: i64,
}

pub fn decode(msg_type: i32, data: Option<&[u8]>) -> MsgBody {
    // A call row carries no meaningful body (Android getStoredMessageText ignores it).
    if msg_type == 2 {
        return MsgBody::Call;
    }
    let Some(text) = data.and_then(|d| std::str::from_utf8(d).ok()) else {
        return MsgBody::Unsupported;
    };
    match msg_type {
        0 => MsgBody::Text(text.to_string()),
        1 | 3 | 4 => AttachmentMeta::parse(text.as_bytes()).map_or(MsgBody::Unsupported, |a| match kind(msg_type, &a) {
            AttachmentKind::Image => MsgBody::Image(a),
            AttachmentKind::Voice => MsgBody::Voice(a),
            AttachmentKind::Video => MsgBody::VideoMessage(a),
            AttachmentKind::File | AttachmentKind::Audio => MsgBody::File(a),
        }),
        10 => serde_json::from_str::<Reaction>(text)
            .map_or(MsgBody::Unsupported, |r| MsgBody::Reaction { emoji: r.emoji, target_guid: r.reply_to }),
        _ => MsgBody::Unsupported,
    }
}

/// Chat-list preview, prefixes as in Android getChatList.
pub fn preview(body: &MsgBody, lang: Lang) -> String {
    let pick = |caption: &str, name: &str, fallback: Key| {
        if !caption.is_empty() { caption.to_string() }
        else if !name.is_empty() { name.to_string() }
        else { tr(lang, fallback).to_string() }
    };
    match body {
        MsgBody::Text(t) => t.clone(),
        MsgBody::Image(a) => format!("🖼️ {}", pick(&a.caption, a.display_name(), Key::Image)),
        MsgBody::File(a) => format!("📄 {}", pick(&a.caption, a.display_name(), Key::File)),
        MsgBody::Voice(_) => tr(lang, Key::VoiceMessage).to_string(),
        MsgBody::VideoMessage(_) => format!("📹 {}", tr(lang, Key::Video)),
        MsgBody::Call => tr(lang, Key::Call).to_string(),
        // ponytail: reactions are not shown as a last-message preview; empty string.
        MsgBody::Reaction { .. } => String::new(),
        MsgBody::Unsupported => format!("📦 {}", tr(lang, Key::Unsupported)),
    }
}

/// JSON `{"emoji":..,"replyTo":..}` in Android's org.json insertion order.
pub fn reaction_payload(emoji: &str, target_guid: i64) -> Vec<u8> {
    format!(r#"{{"emoji":{},"replyTo":{}}}"#, serde_json::to_string(emoji).unwrap(), target_guid).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn decode_text() { assert_eq!(decode(0, Some("привет".as_bytes())), MsgBody::Text("привет".into())); }
    #[test] fn decode_voice_by_mime() {
        let d = br#"{"name":"a1b2","originalName":"voice_message.m4a","size":11776,"mimeType":"audio/mp4"}"#;
        let MsgBody::Voice(a) = decode(3, Some(d)) else { panic!() };
        assert_eq!((a.display_name(), a.size), ("voice_message.m4a", 11776));
        // Other audio is a file (it plays from its file bubble).
        let song = br#"{"name":"a1b2.mp3","originalName":"song.mp3","size":5,"mimeType":"audio/mpeg"}"#;
        assert!(matches!(decode(3, Some(song)), MsgBody::File(a) if a.display_name() == "song.mp3"));
    }
    #[test] fn decode_reaction() {
        assert_eq!(decode(10, Some(&reaction_payload("👍", 42))), MsgBody::Reaction{emoji:"👍".into(), target_guid:42});
        assert_eq!(std::str::from_utf8(&reaction_payload("👍", 42)).unwrap(), r#"{"emoji":"👍","replyTo":42}"#);
    }
    #[test] fn decode_invalid_utf8() { assert_eq!(decode(0, Some(&[0xff,0xfe])), MsgBody::Unsupported); }
    #[test] fn decode_bad_json()    { assert_eq!(decode(1, Some(b"{oops")), MsgBody::Unsupported); }
    #[test] fn decode_unknown_type() { assert_eq!(decode(77, Some(b"x")), MsgBody::Unsupported); }
    #[test] fn preview_file_prefix() {
        let pdf = AttachmentMeta { name: "h.pdf".into(), original_name: "a.pdf".into(), size: 1, ..Default::default() };
        assert_eq!(preview(&MsgBody::File(pdf), Lang::Ru), "📄 a.pdf");
    }
    #[test] fn decode_file_and_image() {
        let f = br#"{"name":"h","originalName":"a.pdf","size":5,"text":"cap"}"#;
        let MsgBody::File(a) = decode(3, Some(f)) else { panic!() };
        assert_eq!((a.display_name(), a.size, a.caption.as_str()), ("a.pdf", 5, "cap"));
        assert!(matches!(decode(1, Some(br#"{"name":"h"}"#)), MsgBody::Image(a) if a.display_name() == "h" && a.caption.is_empty()));
        assert!(matches!(decode(4, Some(br#"{"name":"v"}"#)), MsgBody::VideoMessage(a) if a.name == "v"));
        assert_eq!(decode(2, Some(b"")), MsgBody::Call);
        assert_eq!(decode(2, None), MsgBody::Call, "a call row without a body is still a call");
        assert_eq!(decode(2, Some(&[0xff])), MsgBody::Call);
    }
    #[test] fn preview_variants() {
        let img = MsgBody::Image(AttachmentMeta { name: "p.png".into(), caption: "hi".into(), ..Default::default() });
        assert_eq!(preview(&img, Lang::En), "🖼️ hi");
        let empty = MsgBody::Image(AttachmentMeta::default());
        assert_eq!(preview(&empty, Lang::En), "🖼️ Image");
        let v = MsgBody::Voice(AttachmentMeta { name: "x".into(), size: 1, ..Default::default() });
        assert_eq!(preview(&v, Lang::Ru), "🎤 Голосовое сообщение");
        assert_eq!(preview(&MsgBody::Text("t".into()), Lang::En), "t");
        assert_eq!(preview(&MsgBody::Unsupported, Lang::En), "📦 Unsupported message — update Mimir");
    }
}
