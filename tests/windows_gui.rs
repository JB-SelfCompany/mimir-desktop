//! The release exe must start without a console window on Windows.
#[test]
fn release_build_is_a_windows_gui_app() {
    let main = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs")).unwrap();
    assert!(main.contains(r#"#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]"#));
}
