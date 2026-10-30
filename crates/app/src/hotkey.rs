use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use common::Hotkey;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_APP, WM_HOTKEY, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
};

const HOTKEY_ID: i32 = 1;
/// Posted to the hotkey thread when the key it watches is pressed.
const WM_WATCHED_KEY: u32 = WM_APP + 1;

/// A system-wide hotkey, active even while another app (a game) has focus.
/// Unregistered on drop.
pub struct GlobalHotkey {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

/// How the hotkey thread hears the hotkey.
enum Listening {
    /// A combination with Ctrl or Alt, which Windows keeps for Sonora.
    Registered,
    /// A key on its own, watched while it goes on to whatever app has focus
    /// (see [`passes_through`]).
    Watching(HHOOK),
}

impl GlobalHotkey {
    /// Registers `hotkey` and calls `on_press` on a background thread each
    /// time it's pressed.
    pub fn register(hotkey: Hotkey, on_press: impl Fn() + Send + 'static) -> Result<Self, String> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("moments-hotkey".into())
            .spawn(move || {
                // The hotkey belongs to this thread's message queue.
                let listening = match listen(hotkey) {
                    Ok(listening) => listening,
                    Err(message) => {
                        let _ = ready_tx.send(Err(message));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(unsafe { GetCurrentThreadId() }));
                let mut message = MSG::default();
                while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
                    if message.message == WM_HOTKEY || message.message == WM_WATCHED_KEY {
                        on_press();
                    }
                }
                unsafe {
                    let _ = match listening {
                        Listening::Registered => UnregisterHotKey(None, HOTKEY_ID),
                        Listening::Watching(hook) => UnhookWindowsHookEx(hook),
                    };
                }
            })
            .expect("spawn hotkey thread");
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self { thread_id, thread: Some(thread) }),
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(message)
            }
            Err(_) => Err("hotkey thread exited unexpectedly".into()),
        }
    }
}

impl Drop for GlobalHotkey {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts hearing `hotkey` on the calling thread.
fn listen(hotkey: Hotkey) -> Result<Listening, String> {
    if passes_through(hotkey) {
        WATCHED_KEY.store(hotkey.key, Ordering::Relaxed);
        WATCHED_SHIFT.store(hotkey.shift, Ordering::Relaxed);
        WATCHED_DOWN.store(false, Ordering::Relaxed);
        WATCHING_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
        let module = unsafe { GetModuleHandleW(PCWSTR::null()) }.ok().map(Into::into);
        return unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(watch_keys), module, 0) }
            .map(Listening::Watching)
            .map_err(|error| format!("couldn't watch {}: {error}", label(hotkey)));
    }
    match unsafe { RegisterHotKey(None, HOTKEY_ID, modifiers(hotkey), hotkey.key) } {
        Ok(()) => Ok(Listening::Registered),
        Err(error) if error.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() => {
            Err(format!("{} is already used by another app; pick a different hotkey", label(hotkey)))
        }
        Err(error) => Err(format!("couldn't register {}: {error}", label(hotkey))),
    }
}

/// Whether `hotkey` is a key on its own (Shift aside). Windows would keep a
/// registered key from every other app, so Home would stop working where
/// you type; instead it's watched, and still does its usual thing there.
pub fn passes_through(hotkey: Hotkey) -> bool {
    !hotkey.ctrl && !hotkey.alt
}

/// The key on its own being watched, and the thread to tell. One hotkey
/// is listened to at a time.
static WATCHED_KEY: AtomicU32 = AtomicU32::new(0);
static WATCHED_SHIFT: AtomicBool = AtomicBool::new(false);
static WATCHING_THREAD: AtomicU32 = AtomicU32::new(0);
/// Whether it's held down, so holding it (which repeats) counts once.
static WATCHED_DOWN: AtomicBool = AtomicBool::new(false);

/// Sees every key press, in any app, and passes it on. Windows waits for
/// this, so it only posts a message to the hotkey thread.
unsafe extern "system" fn watch_keys(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if key.vkCode == WATCHED_KEY.load(Ordering::Relaxed) {
            let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let was_down = WATCHED_DOWN.swap(down, Ordering::Relaxed);
            if down && !was_down && modifiers_allow(WATCHED_SHIFT.load(Ordering::Relaxed)) {
                let thread = WATCHING_THREAD.load(Ordering::Relaxed);
                let _ = unsafe { PostThreadMessageW(thread, WM_WATCHED_KEY, WPARAM(0), LPARAM(0)) };
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// A key on its own counts unless Ctrl or Alt is held (Ctrl+Home in an
/// editor isn't a clip), whether or not Shift is (sprinting, say); one set
/// with Shift needs Shift.
fn modifiers_allow(with_shift: bool) -> bool {
    let held = |key: VIRTUAL_KEY| unsafe { GetAsyncKeyState(i32::from(key.0)) } < 0;
    !held(VK_CONTROL) && !held(VK_MENU) && (!with_shift || held(VK_SHIFT))
}

fn modifiers(hotkey: Hotkey) -> HOT_KEY_MODIFIERS {
    let mut modifiers = MOD_NOREPEAT;
    if hotkey.ctrl {
        modifiers |= MOD_CONTROL;
    }
    if hotkey.alt {
        modifiers |= MOD_ALT;
    }
    if hotkey.shift {
        modifiers |= MOD_SHIFT;
    }
    modifiers
}

/// "Ctrl+Alt+F10" style text.
pub fn label(hotkey: Hotkey) -> String {
    let mut text = String::new();
    for (held, name) in [(hotkey.ctrl, "Ctrl+"), (hotkey.alt, "Alt+"), (hotkey.shift, "Shift+")] {
        if held {
            text.push_str(name);
        }
    }
    text.push_str(&key_name(hotkey.key));
    text
}

fn key_name(vk: u32) -> String {
    match vk {
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        _ => format!("Key {vk:#04x}"),
    }
}

/// Builds a hotkey from a key pressed in the app window, or `None` if that
/// key can't be a hotkey. F-keys and Home, End, Page Up/Down, Insert and
/// Delete work on their own; letters and digits need Ctrl or Alt, so
/// typing doesn't save clips.
pub fn from_key_press(key: eframe::egui::Key, modifiers: eframe::egui::Modifiers) -> Option<Hotkey> {
    use eframe::egui::Key;
    let name = key.name();
    let function_key = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=24).contains(n));
    let (vk, allowed_alone) = if let Some(n) = function_key {
        (0x6F + n, true)
    } else {
        match key {
            Key::Insert => (0x2D, true),
            Key::Delete => (0x2E, true),
            Key::Home => (0x24, true),
            Key::End => (0x23, true),
            Key::PageUp => (0x21, true),
            Key::PageDown => (0x22, true),
            _ => {
                let mut chars = name.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_alphanumeric() => (c.to_ascii_uppercase() as u32, false),
                    _ => return None,
                }
            }
        }
    };
    let hotkey = Hotkey { ctrl: modifiers.ctrl, alt: modifiers.alt, shift: modifiers.shift, key: vk };
    (allowed_alone || hotkey.ctrl || hotkey.alt).then_some(hotkey)
}

#[cfg(test)]
mod tests {
    use eframe::egui::{Key, Modifiers};

    use super::*;

    #[test]
    fn labels_read_naturally() {
        assert_eq!(label(Hotkey::default()), "Alt+F10");
        assert_eq!(label(Hotkey { ctrl: true, alt: false, shift: true, key: 0x53 }), "Ctrl+Shift+S");
    }

    #[test]
    fn function_and_navigation_keys_can_be_used_alone() {
        assert_eq!(from_key_press(Key::F9, Modifiers::NONE), Some(Hotkey { ctrl: false, alt: false, shift: false, key: 0x78 }));
        let home = from_key_press(Key::Home, Modifiers::NONE).unwrap();
        assert_eq!(label(home), "Home");
        assert!(passes_through(home), "Home still works in other apps");
        assert!(!passes_through(from_key_press(Key::Home, Modifiers::CTRL).unwrap()));
    }

    /// Presses F24 (which nothing uses) for real: on its own it counts,
    /// once however long it's held; with Ctrl held it doesn't.
    #[test]
    #[ignore = "presses keys on this computer"]
    fn a_key_on_its_own_is_heard_while_it_passes_through() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;
        use std::time::Duration;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VK_F24,
        };

        let press = |key: VIRTUAL_KEY, up: bool| {
            let flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: key, dwFlags: flags, ..Default::default() } },
            };
            unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
            std::thread::sleep(Duration::from_millis(50));
        };
        let presses = Arc::new(AtomicUsize::new(0));
        let counted = presses.clone();
        let hotkey = Hotkey { ctrl: false, alt: false, shift: false, key: u32::from(VK_F24.0) };
        let _listening = GlobalHotkey::register(hotkey, move || {
            counted.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();

        press(VK_F24, false);
        press(VK_F24, false); // held: it repeats
        press(VK_F24, true);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(presses.load(Ordering::Relaxed), 1);

        press(VK_CONTROL, false);
        press(VK_F24, false);
        press(VK_F24, true);
        press(VK_CONTROL, true);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(presses.load(Ordering::Relaxed), 1, "Ctrl+F24 isn't the hotkey");
    }

    #[test]
    fn letters_need_ctrl_or_alt() {
        assert_eq!(from_key_press(Key::S, Modifiers::NONE), None);
        assert_eq!(from_key_press(Key::S, Modifiers::SHIFT), None);
        assert_eq!(from_key_press(Key::S, Modifiers::ALT).map(|h| h.key), Some(0x53));
        assert_eq!(from_key_press(Key::Num4, Modifiers::CTRL).map(|h| h.key), Some(0x34));
    }
}
