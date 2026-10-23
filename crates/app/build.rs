use std::path::PathBuf;

fn main() {
    embed_notices();

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

/// Builds the open-source licenses into the program, so a release (a lone
/// exe, say) carries them wherever it's copied: the release scripts point
/// `SONORA_NOTICES` at `scripts/third_party_notices.py`'s output. Other
/// builds get a pointer to the repository instead.
fn embed_notices() {
    println!("cargo:rerun-if-env-changed=SONORA_NOTICES");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("notices.txt");
    let notices = match std::env::var_os("SONORA_NOTICES") {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
            std::fs::read_to_string(&path).expect("read SONORA_NOTICES")
        }
        None => format!(
            "This build doesn't include the libraries' license texts; release builds do.\n\
             Run scripts/third_party_notices.py in the source ({}) to list them.\n",
            std::env::var("CARGO_PKG_REPOSITORY").unwrap_or_default()
        ),
    };
    std::fs::write(out, notices).expect("write notices.txt");
}
