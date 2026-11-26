// The exe's own icon (Explorer, taskbar pins, Alt+Tab before the window sets one).
fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=packaging/windows/mimir.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/windows/mimir.ico");
        res.compile().expect("embed the Windows icon");
    }
}
