fn main() {
    // Embed the app icon and version info into RemoteDesk.exe (shown in Explorer and the taskbar).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "RemoteDesk");
        res.set("FileDescription", "RemoteDesk");
        res.compile().expect("compile Windows resources");
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
