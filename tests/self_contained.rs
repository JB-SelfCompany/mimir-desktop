//! Source comments and strings must stand on their own: no pointers to local planning docs, review rulings or
//! plan/task numbers, which readers of the public repository cannot see.
use std::path::Path;

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// True when `word` is followed by a space or dash and a digit ("Plan 3", "Task 14", "P2-R7", "PF-2").
fn numbered(line: &str, word: &str, sep: char) -> bool {
    line.match_indices(word).any(|(i, _)| {
        let before_ok = line[..i].chars().next_back().map_or(true, |c| !c.is_alphanumeric());
        let mut rest = line[i + word.len()..].chars();
        before_ok && rest.next() == Some(sep) && rest.next().is_some_and(|c| c.is_ascii_digit())
    })
}

/// True for a review tag: "(I1)", "(m3)", "C1:" or "R9", any case. A lone letter and digits next to code ("r2") need the
/// bracket or colon to count, "R<digit>" does not (round tags).
fn review_tag(line: &str) -> bool {
    let line = line.to_lowercase();
    let b = line.as_bytes();
    (0..b.len()).any(|i| {
        let word_start = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let letter = b[i];
        if !word_start || !b"icmr".contains(&letter) {
            return false;
        }
        let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        let end = i + 1 + digits;
        let framed = (i > 0 && b[i - 1] == b'(' && b.get(end) == Some(&b')')) || (b.get(end) == Some(&b':') && b.get(end + 1) != Some(&b':'));
        digits > 0 && (letter != b'i' || digits == 1) && !b.get(end).is_some_and(|c| c.is_ascii_alphanumeric()) && (framed || letter == b'r')
    })
}

#[test]
fn review_tags_are_recognised() {
    for bad in ["// (I1) x", "// m7: x", "(c1)", "round R9", "r2 done"] {
        assert!(review_tag(bad), "{bad}");
    }
    for ok in ["let m1 = 2;", "color c1 = x", "image/png", "0xC1", "// Mimir 3.2", "(i64, bool)", "i64::MAX", "Leave(i64),", "m0, m1"] {
        assert!(!review_tag(ok), "{ok}");
    }
}

#[test]
fn no_references_to_local_docs() {
    let mut files = Vec::new();
    rust_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    files.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
    let mut hits = Vec::new();
    for f in files {
        for (n, line) in std::fs::read_to_string(&f).unwrap().lines().enumerate() {
            let bad = review_tag(line)
                || line.contains('§')
                || line.contains("ANDROID_")
                || line.contains("ruling")
                || ["Plan", "Task"].iter().any(|w| numbered(line, w, ' '))
                || ["P1", "P2", "P3", "P4", "P5", "P6", "PF"].iter().any(|w| numbered(line, w, '-') || (*w == "PF" && line.contains("PF-")));
            if bad && !line.contains("MIMIR_ANDROID_BACKUP") {
                hits.push(format!("{}:{}: {}", f.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(hits.is_empty(), "comments point at local docs:\n{}", hits.join("\n"));
}
