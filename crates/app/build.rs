fn main() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.set("FileDescription", "Sonora");
    res.set("ProductName", "Sonora");
    res.compile().expect("embed Windows icon and version resources");
}
