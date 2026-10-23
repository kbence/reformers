//! The Windows .exe's icon in Explorer, from `icon/reformers.ico` (the windows' own icon, on every
//! platform, comes from `src/icon.rs`). Nothing to do elsewhere.
fn main() {
    println!("cargo:rerun-if-changed=icon/reformers.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon/reformers.ico");
        res.compile().expect("the Windows icon resource");
    }
}
