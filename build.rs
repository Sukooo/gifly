// Embeds the app icon, so the exe, Start menu shortcut and Installed apps entry show it.
fn main() {
    println!("cargo:rerun-if-changed=assets/gifly.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new().set_icon("assets/gifly.ico").compile().expect("embedding the icon");
    }
}
