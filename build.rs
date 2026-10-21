fn main() {
    // Embed the app icon and version info into RemoteDesk.exe (shown in Explorer and the taskbar).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "RemoteDesk");
        res.set("FileDescription", "RemoteDesk");
        // Release builds always run as administrator (Windows shows its UAC prompt on launch),
        // so remote input also reaches elevated windows such as Task Manager. Debug builds stay
        // unelevated so the headless self-test and preview run without prompts.
        if std::env::var("PROFILE").as_deref() == Ok("release") {
            res.set_manifest(
                r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>"#,
            );
        }
        res.compile().expect("compile Windows resources");
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
