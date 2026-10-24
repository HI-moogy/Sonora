//! The games installed on this PC, for the clip settings' games list and
//! for finding games launchers put somewhere unusual: Steam and Epic
//! libraries by name, the game folders of other launchers, and the games
//! Windows itself knows. [`scan`] reads files on every drive with a
//! library, so it runs on a background thread, and only at startup and
//! when the clip settings open (a periodic scan would keep hard drives
//! spinning).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetFileVersionInfoSizeW, GetFileVersionInfoW, GetLogicalDrives, VerQueryValueW,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};

use crate::games;

/// `GetDriveTypeW`'s answer for a local disk.
const DRIVE_FIXED: u32 = 3;

/// Steam apps that aren't games (by app id), so a window of theirs never
/// starts a recording: Steamworks Common Redistributables, Wallpaper
/// Engine, Lossless Scaling, SteamVR, OBS Studio, Blender, Soundpad,
/// projectM Music Visualizer.
const STEAM_TOOLS: &[&str] = &["228980", "431960", "993090", "250820", "1905180", "365670", "629520", "1358800"];

/// Names of Steam's own runtimes, which it installs like games: Proton,
/// Proton's anti-cheat runtimes, and the Steam Linux Runtimes.
const STEAM_TOOL_NAMES: &[&str] = &["Proton ", "Steam Linux Runtime", "Steamworks "];

/// Other launchers' game folders, under each drive: the launcher's name,
/// the folder, and subfolders in it that aren't games.
const LAUNCHER_LIBRARIES: &[(&str, &str, &[&str])] = &[
    ("Riot Games", "Riot Games", &["riot client"]),
    ("Xbox", "XboxGames", &["gamesave", "shaders", "minecraft launcher"]),
    ("EA", "Program Files\\EA Games", &[]),
    ("EA", "Program Files (x86)\\Origin Games", &[]),
    ("Ubisoft", "Program Files (x86)\\Ubisoft\\Ubisoft Game Launcher\\games", &[]),
    ("GOG", "Program Files (x86)\\GOG Galaxy\\Games", &[]),
];

/// Folder names inside a game's install that say nothing about which game
/// it is, skipped when naming a game after its folders.
const GENERIC_FOLDERS: &[&str] = &[
    "bin", "binaries", "win64", "win32", "x64", "x86", "x64_dx12", "game", "retail", "live", "media", "system",
];

/// A game Sonora knows is installed.
#[derive(Debug, Clone, PartialEq)]
pub struct Game {
    pub name: String,
    /// Where it was found: "Steam", "Epic Games", another launcher, or
    /// "Windows" (the games Windows itself recognizes).
    pub source: &'static str,
    /// What counts as this game (lowercase): a folder ending in `\`
    /// (every program in it) or one program's full path.
    pub path: String,
}

impl Game {
    /// What leaving it out stores in `never_record` (see
    /// [`games::covers`]): its folder, or its program's file name, which
    /// still matches after an update moves the program (Roblox's do).
    pub fn choice(&self) -> String {
        if self.path.ends_with('\\') {
            self.path.clone()
        } else {
            games::exe_name(&self.path).to_string()
        }
    }
}

/// What a scan found.
#[derive(Debug, Default)]
pub struct Library {
    /// Sorted by name.
    pub games: Vec<Game>,
    /// Install folders (lowercase, ending in `\`) of launcher apps that
    /// aren't games.
    pub tools: Vec<String>,
}

/// Scans on a background thread; the receiver gets the result once.
pub fn scan_in_background() -> Receiver<Library> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("game-library".into())
        .spawn(move || {
            let _ = sender.send(scan());
        })
        .expect("spawn game library thread");
    receiver
}

pub fn scan() -> Library {
    let mut library = Library::default();
    steam(&mut library);
    epic(&mut library);
    other_launchers(&mut library);
    windows_list(&mut library);
    library.games.sort_by_key(|game| game.name.to_lowercase());
    library.games.dedup_by(|a, b| a.path == b.path);
    library
}

/// Lowercase, with a trailing backslash: how folder choices are written.
fn folder_choice(path: &Path) -> String {
    let mut folder = path.to_string_lossy().to_lowercase();
    if !folder.ends_with('\\') {
        folder.push('\\');
    }
    folder
}

fn steam(library: &mut Library) {
    let Some(root) = read_registry_text(w!("Software\\Valve\\Steam"), w!("SteamPath")) else {
        return;
    };
    // The registry writes it with forward slashes.
    let root = PathBuf::from(root.replace('/', "\\"));
    let mut libraries = vec![root.clone()];
    if let Ok(text) = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf")) {
        let folders = vdf::parse(&text);
        if let Some(vdf::Value::Section(entries)) = folders.get("libraryfolders") {
            for (_, folder) in entries {
                if let Some(path) = folder.get("path").and_then(vdf::Value::text) {
                    let path = PathBuf::from(path);
                    if !libraries.iter().any(|known| known.as_os_str().eq_ignore_ascii_case(path.as_os_str())) {
                        libraries.push(path);
                    }
                }
            }
        }
    }
    for steamapps in libraries.iter().map(|folder| folder.join("steamapps")) {
        let Ok(entries) = std::fs::read_dir(&steamapps) else { continue };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_lowercase();
            if !file_name.starts_with("appmanifest_") || !file_name.ends_with(".acf") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
            let manifest = vdf::parse(&text);
            let Some(app) = manifest.get("AppState") else { continue };
            let (Some(id), Some(name), Some(dir)) = (
                app.get("appid").and_then(vdf::Value::text),
                app.get("name").and_then(vdf::Value::text),
                app.get("installdir").and_then(vdf::Value::text),
            ) else {
                continue;
            };
            let folder = folder_choice(&steamapps.join("common").join(dir));
            if STEAM_TOOLS.contains(&id) || STEAM_TOOL_NAMES.iter().any(|tool| name.starts_with(tool)) {
                library.tools.push(folder);
            } else {
                library.games.push(Game { name: name.to_string(), source: "Steam", path: folder });
            }
        }
    }
}

fn epic(library: &mut Library) {
    let Some(program_data) = std::env::var_os("ProgramData") else { return };
    let manifests = Path::new(&program_data).join("Epic\\EpicGamesLauncher\\Data\\Manifests");
    let Ok(entries) = std::fs::read_dir(manifests) else { return };
    for entry in entries.flatten() {
        if entry.path().extension().is_none_or(|ext| !ext.eq_ignore_ascii_case("item")) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let (Some(name), Some(location)) = (manifest["DisplayName"].as_str(), manifest["InstallLocation"].as_str()) else {
            continue;
        };
        let folder = folder_choice(Path::new(location));
        let is_game = manifest["AppCategories"]
            .as_array()
            .is_some_and(|categories| categories.iter().any(|category| category.as_str() == Some("games")));
        if is_game {
            library.games.push(Game { name: name.to_string(), source: "Epic Games", path: folder });
        } else {
            library.tools.push(folder);
        }
    }
}

fn other_launchers(library: &mut Library) {
    for drive in fixed_drives() {
        for (source, folder, not_games) in LAUNCHER_LIBRARIES {
            let Ok(entries) = std::fs::read_dir(drive.join(folder)) else { continue };
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let path = folder_choice(&entry.path());
                if not_games.contains(&name.to_lowercase().as_str()) {
                    library.tools.push(path);
                } else {
                    library.games.push(Game { name, source, path });
                }
            }
        }
    }
}

/// The games Windows recognizes that no launcher scan found (games from
/// other stores, or copied in by hand), named by their program.
fn windows_list(library: &mut Library) {
    for path in games::windows_known_games() {
        let lowercase = path.to_lowercase();
        let listed = library.games.iter().any(|game| games::covers(&game.path, &lowercase))
            || library.tools.iter().any(|folder| lowercase.starts_with(folder));
        if listed || !Path::new(&path).exists() {
            continue;
        }
        // Java runs Minecraft (and other Java games) as javaw.exe, which
        // calls itself "OpenJDK Platform" or "Java Platform".
        let name = if matches!(games::exe_name(&lowercase), "javaw.exe" | "java.exe") {
            "Java games (Minecraft: Java Edition...)".to_string()
        } else {
            product_name(&path).unwrap_or_else(|| name_from_folders(&path))
        };
        library.games.push(Game { name, source: "Windows", path: lowercase });
    }
}

/// `C:\`, `D:\` and so on for each local disk.
fn fixed_drives() -> Vec<PathBuf> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| format!("{}:\\", (b'A' + bit) as char))
        .filter(|root| unsafe { GetDriveTypeW(&HSTRING::from(root.as_str())) } == DRIVE_FIXED)
        .map(PathBuf::from)
        .collect()
}

fn read_registry_text(key: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key, value, RRF_RT_REG_SZ, None, Some(buffer.as_mut_ptr().cast()), Some(&mut size))
    }
    .ok()
    .ok()?;
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// The name a program gives itself: its product name, or its description
/// when that isn't just its file name.
fn product_name(path: &str) -> Option<String> {
    let path = HSTRING::from(path);
    let size = unsafe { GetFileVersionInfoSizeW(&path, None) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    unsafe { GetFileVersionInfoW(&path, None, size, data.as_mut_ptr().cast()) }.ok()?;
    let query = |key: &str| -> Option<(*const u16, usize)> {
        let mut found = std::ptr::null_mut();
        let mut len = 0u32;
        unsafe { VerQueryValueW(data.as_ptr().cast(), &HSTRING::from(key), &mut found, &mut len) }
            .as_bool()
            .then_some((found as *const u16, len as usize))
    };
    // The first language the program has strings in.
    let (translation, bytes) = query("\\VarFileInfo\\Translation")?;
    if bytes < 4 {
        return None;
    }
    let (language, codepage) = unsafe { (*translation, *translation.add(1)) };
    ["ProductName", "FileDescription"].into_iter().find_map(|key| {
        let (text, chars) = query(&format!("\\StringFileInfo\\{language:04x}{codepage:04x}\\{key}"))?;
        // Some programs give a length past the value's end: stop at its null.
        let text = unsafe { std::slice::from_raw_parts(text, chars) };
        let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        let text = String::from_utf16_lossy(&text[..end]);
        let text = text.trim();
        (!text.is_empty() && !text.to_lowercase().ends_with(".exe")).then(|| text.to_string())
    })
}

/// A game's name from the folders its program is in ("Watch Dogs" for
/// `...\Watch Dogs\bin\watch_dogs.exe`), else the program's own name.
fn name_from_folders(path: &str) -> String {
    let path = Path::new(path);
    path.ancestors()
        .skip(1)
        .filter_map(|folder| folder.file_name()?.to_str())
        .find(|name| !GENERIC_FOLDERS.contains(&name.to_lowercase().as_str()))
        .or_else(|| path.file_stem()?.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Valve's KeyValues text, as Steam writes its library files: `"key"
/// "value"` pairs and `"key" { ... }` sections.
mod vdf {
    pub enum Value {
        Text(String),
        Section(Vec<(String, Value)>),
    }

    impl Value {
        /// A section's entry; Steam doesn't keep keys' case consistent.
        pub fn get(&self, key: &str) -> Option<&Value> {
            match self {
                Value::Section(entries) => entries.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
                Value::Text(_) => None,
            }
        }

        pub fn text(&self) -> Option<&str> {
            match self {
                Value::Text(text) => Some(text),
                Value::Section(_) => None,
            }
        }
    }

    enum Token {
        Text(String),
        Open,
        Close,
    }

    /// Everything in `text` as one section. Reading stops at anything
    /// malformed, keeping what came before it.
    pub fn parse(text: &str) -> Value {
        Value::Section(section(&mut tokens(text).into_iter()))
    }

    fn section(tokens: &mut impl Iterator<Item = Token>) -> Vec<(String, Value)> {
        let mut entries = Vec::new();
        while let Some(Token::Text(key)) = tokens.next() {
            match tokens.next() {
                Some(Token::Text(value)) => entries.push((key, Value::Text(value))),
                Some(Token::Open) => {
                    let inner = section(tokens);
                    entries.push((key, Value::Section(inner)));
                }
                _ => break,
            }
        }
        entries
    }

    fn tokens(text: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' => tokens.push(Token::Open),
                '}' => tokens.push(Token::Close),
                '/' if chars.peek() == Some(&'/') => {
                    while chars.next_if(|&c| c != '\n').is_some() {}
                }
                '"' => {
                    let mut text = String::new();
                    while let Some(c) = chars.next() {
                        match c {
                            '"' => break,
                            '\\' => match chars.next() {
                                Some('n') => text.push('\n'),
                                Some('t') => text.push('\t'),
                                Some(other) => text.push(other),
                                None => break,
                            },
                            c => text.push(c),
                        }
                    }
                    tokens.push(Token::Text(text));
                }
                c if c.is_whitespace() => {}
                c => {
                    let mut text = c.to_string();
                    while let Some(c) = chars.next_if(|&c| !c.is_whitespace() && !matches!(c, '{' | '}' | '"')) {
                        text.push(c);
                    }
                    tokens.push(Token::Text(text));
                }
            }
        }
        tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_steam_library_folders() {
        let text = r#""libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"apps"
		{
			"105600"		"803964902"
		}
	}
	// a comment
	"1"
	{
		"path"		"D:\\SteamLibrary"
	}
}"#;
        let folders = vdf::parse(text);
        let Some(vdf::Value::Section(entries)) = folders.get("libraryfolders") else { panic!("no libraryfolders") };
        let paths: Vec<&str> = entries.iter().filter_map(|(_, folder)| folder.get("path")?.text()).collect();
        assert_eq!(paths, ["C:\\Program Files (x86)\\Steam", "D:\\SteamLibrary"]);
    }

    #[test]
    fn reads_an_app_manifest() {
        let text = "\"AppState\"\n{\n\t\"appid\"\t\t\"105600\"\n\t\"name\"\t\t\"Terraria \\\"1.4\\\"\"\n\t\"installdir\"\t\t\"Terraria\"\n\t\"UserConfig\"\n\t{\n\t\t\"name\"\t\"other\"\n\t}\n}\n";
        let manifest = vdf::parse(text);
        let app = manifest.get("appstate").expect("AppState");
        assert_eq!(app.get("name").and_then(vdf::Value::text), Some("Terraria \"1.4\""));
        assert_eq!(app.get("installdir").and_then(vdf::Value::text), Some("Terraria"));
    }

    #[test]
    fn malformed_text_keeps_what_came_first() {
        let manifest = vdf::parse("\"a\" \"1\" \"b\" { \"c\" \"2\" ");
        assert_eq!(manifest.get("a").and_then(vdf::Value::text), Some("1"));
        assert_eq!(manifest.get("b").and_then(|b| b.get("c")).and_then(vdf::Value::text), Some("2"));
    }

    #[test]
    fn names_games_after_their_folders() {
        assert_eq!(name_from_folders("C:\\Games\\Watch Dogs\\bin\\watch_dogs.exe"), "Watch Dogs");
        assert_eq!(name_from_folders("D:\\Tetris Effect\\TetrisEffect\\Binaries\\Win64\\Tetris.exe"), "TetrisEffect");
        assert_eq!(name_from_folders("C:\\bin\\sh3.exe"), "sh3");
    }

    /// Lists what this PC has: `cargo test -p app --bins scans_this_pc -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads this PC's game libraries"]
    fn scans_this_pc() {
        let started = std::time::Instant::now();
        let library = scan();
        println!("{} games, {} tools in {:?}", library.games.len(), library.tools.len(), started.elapsed());
        for game in &library.games {
            println!("{:<40} {:<12} {}", game.name, game.source, game.path);
        }
        for tool in &library.tools {
            println!("(not a game) {tool}");
        }
    }

    #[test]
    fn leaving_out_a_game_stores_its_folder_or_program_name() {
        let folder = Game { name: "Deadlock".into(), source: "Steam", path: "d:\\steam\\steamapps\\common\\deadlock\\".into() };
        assert_eq!(folder.choice(), "d:\\steam\\steamapps\\common\\deadlock\\");
        let program = Game { name: "Roblox".into(), source: "Windows", path: "c:\\roblox\\version-1\\robloxplayerbeta.exe".into() };
        assert_eq!(program.choice(), "robloxplayerbeta.exe");
    }
}
