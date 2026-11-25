//! Android SharedPreferences XML (`shared_prefs/*.xml`, AS §2.1): a flat `<map>` of typed entries.
use serde::{Deserialize, Serialize};
use PrefValue::*;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PrefValue {
    Bool(bool),
    Int(i64),
    Long(i64),
    Float(f32),
    Str(String),
    StrSet(Vec<String>),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PrefEntry {
    pub name: String,
    pub value: PrefValue,
}

impl PrefValue {
    /// A whole number: an int or long, or a string holding one.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Int(v) | Long(v) => Some(*v),
            Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    /// A string, or a number written out.
    pub fn as_text(&self) -> Option<String> {
        match self {
            Str(s) => Some(s.clone()),
            Int(v) | Long(v) => Some(v.to_string()),
            _ => None,
        }
    }
}

/// Entries of an Android prefs file in file order. Unknown tags, entries without a name and unreadable values are
/// skipped; broken input never panics.
/// ponytail: a hand scanner for the flat layout Android writes (no comments, CDATA or nesting beyond `<set>`);
/// switch to an XML crate if files from other producers ever have to be read.
pub fn parse_prefs(xml: &str) -> Vec<PrefEntry> {
    let mut out = vec![];
    let mut rest = xml;
    while let Some(start) = rest.find('<') {
        let Some(len) = rest[start..].find('>') else { break };
        let tag = &rest[start + 1..start + len];
        rest = &rest[start + len + 1..];
        let (closed, tag) = match tag.strip_suffix('/') {
            Some(t) => (true, t.trim()),
            None => (false, tag.trim()),
        };
        let (kind, attrs) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
        let Some(name) = attr(attrs, "name") else { continue };
        let number = || attr(attrs, "value");
        let value = match kind {
            "boolean" => number().and_then(|v| v.parse().ok()).map(Bool),
            "int" => number().and_then(|v| v.parse().ok()).map(Int),
            "long" => number().and_then(|v| v.parse().ok()).map(Long),
            "float" => number().and_then(|v| v.parse().ok()).map(Float),
            "string" if closed => Some(Str(String::new())),
            "string" => take(&mut rest, "</string>").map(|t| Str(unescape(t))),
            "set" if closed => Some(StrSet(vec![])),
            "set" => take(&mut rest, "</set>").map(|t| StrSet(set_items(t))),
            _ => None,
        };
        if let Some(value) = value {
            out.push(PrefEntry { name, value });
        }
    }
    out
}

/// The prefs file Android writes for `entries` (its header, one element per entry, XML-escaped).
pub fn write_prefs(entries: &[PrefEntry]) -> String {
    let mut out = String::from("<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n");
    for e in entries {
        let n = escape(&e.name);
        let line = match &e.value {
            Bool(v) => format!("<boolean name=\"{n}\" value=\"{v}\" />"),
            Int(v) => format!("<int name=\"{n}\" value=\"{v}\" />"),
            Long(v) => format!("<long name=\"{n}\" value=\"{v}\" />"),
            Float(v) => format!("<float name=\"{n}\" value=\"{v:?}\" />"),
            Str(v) => format!("<string name=\"{n}\">{}</string>", escape(v)),
            StrSet(v) => format!("<set name=\"{n}\">{}</set>", v.iter().map(|s| format!("<string>{}</string>", escape(s))).collect::<String>()),
        };
        out += "    ";
        out += &line;
        out += "\n";
    }
    out + "</map>\n"
}

/// The unescaped value of attribute `key` (double or single quoted).
fn attr(mut attrs: &str, key: &str) -> Option<String> {
    loop {
        let (k, r) = attrs.split_once('=')?;
        let r = r.trim_start();
        let q = r.chars().next().filter(|q| *q == '"' || *q == '\'')?;
        let (v, r) = r[1..].split_once(q)?;
        if k.trim() == key {
            return Some(unescape(v));
        }
        attrs = r;
    }
}

/// The raw text before `end`, moving `rest` past it; `None` (and `rest` untouched) when `end` never comes.
fn take<'a>(rest: &mut &'a str, end: &str) -> Option<&'a str> {
    let i = rest.find(end)?;
    let text = &rest[..i];
    *rest = &rest[i + end.len()..];
    Some(text)
}

/// The `<string>` items inside a `<set>`.
fn set_items(mut s: &str) -> Vec<String> {
    let mut items = vec![];
    while let Some(i) = s.find("<string") {
        s = &s[i..];
        let Some(j) = s.find('>') else { break };
        let closed = s[..j].ends_with('/');
        s = &s[j + 1..];
        if closed {
            items.push(String::new());
        } else if let Some(t) = take(&mut s, "</string>") {
            items.push(unescape(t));
        } else {
            break;
        }
    }
    items
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Undoes the five named entities and numeric references; a stray `&` stays as it is.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let semi = rest.find(';').filter(|&j| j <= 10);
        let ch = semi.and_then(|j| match &rest[1..j] {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            n => n
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        });
        match (semi, ch) {
            (Some(j), Some(c)) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out + rest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, value: PrefValue) -> PrefEntry {
        PrefEntry { name: name.into(), value }
    }

    #[test]
    fn xml_roundtrip_android_sample() {
        let xml = r#"<?xml version='1.0' encoding='utf-8' standalone='yes' ?><map><boolean name="auto-updates" value="true" /><int name="images-quality" value="95" /><string name="voice-quality">32000</string></map>"#;
        let p = parse_prefs(xml);
        assert_eq!(p, vec![e("auto-updates", Bool(true)), e("images-quality", Int(95)), e("voice-quality", Str("32000".into()))]);
        let out = write_prefs(&p);
        assert!(out.starts_with("<?xml version='1.0' encoding='utf-8' standalone='yes' ?>"));
        assert_eq!(parse_prefs(&out), p);
    }

    #[test]
    fn xml_escaping_roundtrip() {
        let p = vec![
            e("k", Str("a&b<c>\"d'".into())),
            e("we\"ird&", Long(-5)),
            e("f", Float(1.5)),
            e("set", StrSet(vec!["x&y".into(), String::new()])),
            e("empty", Str(String::new())),
            e("lines", Str("+a\n-b".into())),
        ];
        assert_eq!(parse_prefs(&write_prefs(&p)), p);
        // Escapes as Android writes them, numeric references, empty elements; unknown tags are skipped.
        let xml = r#"<map><string name="s">&amp;amp; &#65;&#x42; & x</string><set name="e" /><foo name="x" value="1" /><string name="t" /></map>"#;
        assert_eq!(parse_prefs(xml), vec![e("s", Str("&amp; AB & x".into())), e("e", StrSet(vec![])), e("t", Str(String::new()))]);
        // Broken input: unreadable values and an unterminated element are dropped, nothing panics.
        assert_eq!(parse_prefs(r#"<map><int name="a" value="x" /><string name="b">open"#), vec![]);
        assert_eq!(parse_prefs("<map><int name="), vec![]);
    }
}
