fn main() {
    // The icon and version resources are a Windows executable's; Linux
    // gets its icon from the desktop entry.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.set("FileDescription", "Sonora");
    res.set("ProductName", "Sonora");
    res.compile().expect("embed Windows icon and version resources");
}
