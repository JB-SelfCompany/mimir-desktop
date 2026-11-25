//! Contact links: raw hex keys, mimir://mm/u/<hex>[/<name>] and https://go.mimir.im/u/<hex>[/<name>].
use mimir::MimirApp;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AddContactError {
    WrongKey,
    Yourself,
    AlreadyAdded,
}

/// Public key (32 bytes) and the name carried by the link, if any.
pub fn parse_contact(input: &str) -> Option<(Vec<u8>, Option<String>)> {
    let s = input.trim();
    let rest = ["mimir://mm/u/", "https://go.mimir.im/u/"].iter().find_map(|p| s.strip_prefix(p)).unwrap_or(s);
    let (key, name) = match rest.split_once('/') {
        Some((k, n)) => (k, Some(n)),
        None => (rest, None),
    };
    let key = hex::decode(key).ok().filter(|k| k.len() == 32)?;
    let name = name.and_then(|n| urlencoding::decode(n).ok()).map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    Some((key, name))
}

pub fn contact_link(pubkey: &[u8], name: &str) -> String {
    format!("mimir://mm/u/{}/{}", hex::encode(pubkey), urlencoding::encode(name))
}

pub fn validate_new_contact(app: &MimirApp, input: &str) -> Result<(Vec<u8>, Option<String>), AddContactError> {
    let (key, name) = parse_contact(input).ok_or(AddContactError::WrongKey)?;
    if key == app.public_key() {
        return Err(AddContactError::Yourself);
    }
    if app.get_contact_id(key.clone()) > 0 {
        return Err(AddContactError::AlreadyAdded);
    }
    Ok((key, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_app;

    const HEX: &str = "f9a4e8d77c34ce192aebc553d9815709c28d35c0a773160508de91a0dac43c22";

    #[test] fn parse_all_forms() {
        assert_eq!(parse_contact(HEX).unwrap().0.len(), 32);
        assert_eq!(parse_contact(&HEX.to_uppercase()).unwrap().0, parse_contact(HEX).unwrap().0);
        assert_eq!(parse_contact(&format!("mimir://mm/u/{HEX}/Spec%20Bot")).unwrap().1.as_deref(), Some("Spec Bot"));
        assert!(parse_contact(&format!("https://go.mimir.im/u/{HEX}")).is_some());
        assert!(parse_contact("0123").is_none());
    }

    #[test] fn link_roundtrip() {
        let k = [7u8; 32];
        assert_eq!(parse_contact(&contact_link(&k, "Аня")).unwrap(), (k.to_vec(), Some("Аня".into())));
    }

    #[test] fn validation_errors() {
        let (_t, app) = test_app();
        let me = hex::encode(app.public_key());
        assert_eq!(validate_new_contact(&app, &me), Err(AddContactError::Yourself));
        assert_eq!(validate_new_contact(&app, "zz"), Err(AddContactError::WrongKey));
        assert!(validate_new_contact(&app, HEX).is_ok());
        app.add_contact(hex::decode(HEX).unwrap(), "x".into()).unwrap();
        assert_eq!(validate_new_contact(&app, HEX), Err(AddContactError::AlreadyAdded));
    }
}
