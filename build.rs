// Windows: put the app icon and version details into the .exe files, so Explorer, the taskbar and
// pinned shortcuts show the Kanvas logo. Other systems get their icon elsewhere (the window icon
// set in `main.rs`, and the .icns built by the release workflow for macOS).
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Kanvas");
        res.set("FileDescription", "Kanvas");
        res.compile().expect("embedding the Windows icon failed");
    }
}
