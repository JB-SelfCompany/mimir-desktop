//! Group and channel invite links: `mimir://<g|c>/<mediator hex>/<chat id>#<key base64url>`, and the same
//! path under `https://go.mimir.im/`.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkKind {
    Group,
    Channel,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GroupLink {
    pub kind: LinkKind,
    pub mediator: [u8; 32],
    pub chat_id: i64,
    pub key: [u8; 32],
}

/// Accepts `mimir://` and `https://go.mimir.im/` links of both kinds, surrounding whitespace trimmed; `None` for bad
/// hex, a non-decimal id, or a key that is not 32 bytes.
pub fn parse_group_link(s: &str) -> Option<GroupLink> {
    let s = s.trim();
    let rest = s.strip_prefix("mimir://").or_else(|| s.strip_prefix("https://go.mimir.im/"))?;
    let (kind, rest) = match rest.split_at_checked(2)? {
        ("g/", r) => (LinkKind::Group, r),
        ("c/", r) => (LinkKind::Channel, r),
        _ => return None,
    };
    let (path, key) = rest.split_once('#')?;
    let (mediator_hex, id) = path.split_once('/')?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut mediator = [0; 32];
    hex::decode_to_slice(mediator_hex, &mut mediator).ok()?;
    let key: [u8; 32] = URL_SAFE_NO_PAD.decode(key).ok()?.try_into().ok()?;
    Some(GroupLink { kind, mediator, chat_id: id.parse().ok()?, key })
}

pub fn encode_group_link(l: &GroupLink) -> String {
    let kind = match l.kind {
        LinkKind::Group => 'g',
        LinkKind::Channel => 'c',
    };
    format!("mimir://{kind}/{}/{}#{}", hex::encode(l.mediator), l.chat_id, URL_SAFE_NO_PAD.encode(l.key))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEDIATOR: &str = "000096390fde19f30d3ba042d77f5f5ce0e6c23bd62ff0afc2e6c29a6fe96b33";
    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

    fn sample() -> GroupLink {
        let mut mediator = [0; 32];
        hex::decode_to_slice(MEDIATOR, &mut mediator).unwrap();
        GroupLink { kind: LinkKind::Group, mediator, chat_id: 123456, key: std::array::from_fn(|i| i as u8) }
    }

    #[test]
    fn roundtrip() {
        let l = sample();
        let s = encode_group_link(&l);
        assert_eq!(s, format!("mimir://g/{MEDIATOR}/123456#{KEY}"));
        assert_eq!(parse_group_link(&s), Some(l));
        let c = GroupLink { kind: LinkKind::Channel, ..sample() };
        assert_eq!(parse_group_link(&encode_group_link(&c)), Some(c));
    }

    #[test]
    fn parses_android_sample() {
        let l = parse_group_link(&format!("  mimir://g/{MEDIATOR}/123456#{KEY}\n")).unwrap();
        assert_eq!((l.kind, l.chat_id), (LinkKind::Group, 123456));
        assert_eq!(l.key, std::array::from_fn::<u8, 32, _>(|i| i as u8));
        assert_eq!(hex::encode(l.mediator), MEDIATOR);
        let c = parse_group_link(&format!("mimir://c/{}/7#{KEY}", MEDIATOR.to_uppercase())).unwrap();
        assert_eq!((c.kind, c.chat_id, c.mediator), (LinkKind::Channel, 7, l.mediator), "upper-case hex accepted");
    }

    #[test]
    fn https_group_link() {
        // Android routes only https `c` links; `g` works here.
        assert_eq!(parse_group_link(&format!("https://go.mimir.im/g/{MEDIATOR}/123456#{KEY}")), Some(sample()));
        let c = parse_group_link(&format!("https://go.mimir.im/c/{MEDIATOR}/123456#{KEY}")).unwrap();
        assert_eq!(c.kind, LinkKind::Channel);
    }

    #[test]
    fn bad_group_link_rejected() {
        let bad = [
            format!("mimir://g/{}/123456#{KEY}", &MEDIATOR[1..]),
            format!("mimir://g/{MEDIATOR}/123456#{}", &KEY[1..]),
            format!("mimir://g/{MEDIATOR}/123456"),
            format!("mimir://g/{MEDIATOR}/12a456#{KEY}"),
            format!("mimir://g/{MEDIATOR}/-5#{KEY}"),
            format!("mimir://g/{MEDIATOR}/#{KEY}"),
            format!("mimir://x/{MEDIATOR}/123456#{KEY}"),
            format!("mimir://g/{}zz/123456#{KEY}", &MEDIATOR[..62]),
            format!("mimir://g/{MEDIATOR}/123456/9#{KEY}"),
            format!("http://go.mimir.im/g/{MEDIATOR}/123456#{KEY}"),
            format!("mimir://u/{MEDIATOR}"),
            String::new(),
        ];
        for s in bad {
            assert_eq!(parse_group_link(&s), None, "{s}");
        }
    }
}
