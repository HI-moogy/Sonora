fn main() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.set("FileDescription", "sonar-rs");
    res.set("ProductName", "sonar-rs");
    res.compile().expect("embed Windows icon and version resources");
}
