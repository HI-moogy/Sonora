use std::sync::mpsc;
use std::thread::JoinHandle;

use common::Hotkey;
use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, PostThreadMessageW, MSG, WM_HOTKEY, WM_QUIT};

const HOTKEY_ID: i32 = 1;

/// A system-wide hotkey, active even while another app (a game) has focus.
/// Unregistered on drop.
pub struct GlobalHotkey {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
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
                if let Err(error) = unsafe { RegisterHotKey(None, HOTKEY_ID, modifiers(hotkey), hotkey.key) } {
                    let message = if error.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() {
                        format!("{} is already used by another app; pick a different hotkey", label(hotkey))
                    } else {
                        format!("couldn't register {}: {error}", label(hotkey))
                    };
                    let _ = ready_tx.send(Err(message));
                    return;
                }
                let _ = ready_tx.send(Ok(unsafe { GetCurrentThreadId() }));
                let mut message = MSG::default();
                while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
                    if message.message == WM_HOTKEY {
                        on_press();
                    }
                }
                unsafe {
                    let _ = UnregisterHotKey(None, HOTKEY_ID);
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
/// key can't be a hotkey. Letters, digits and navigation keys need a
/// modifier so typing elsewhere doesn't trigger clips; F-keys work alone.
pub fn from_key_press(key: eframe::egui::Key, modifiers: eframe::egui::Modifiers) -> Option<Hotkey> {
    use eframe::egui::Key;
    let name = key.name();
    let function_key = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=24).contains(n));
    let (vk, allowed_alone) = if let Some(n) = function_key {
        (0x6F + n, true)
    } else {
        match key {
            Key::Insert => (0x2D, false),
            Key::Delete => (0x2E, false),
            Key::Home => (0x24, false),
            Key::End => (0x23, false),
            Key::PageUp => (0x21, false),
            Key::PageDown => (0x22, false),
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
    fn function_keys_can_be_used_alone() {
        assert_eq!(from_key_press(Key::F9, Modifiers::NONE), Some(Hotkey { ctrl: false, alt: false, shift: false, key: 0x78 }));
    }

    #[test]
    fn letters_need_ctrl_or_alt() {
        assert_eq!(from_key_press(Key::S, Modifiers::NONE), None);
        assert_eq!(from_key_press(Key::S, Modifiers::SHIFT), None);
        assert_eq!(from_key_press(Key::S, Modifiers::ALT).map(|h| h.key), Some(0x53));
        assert_eq!(from_key_press(Key::Num4, Modifiers::CTRL).map(|h| h.key), Some(0x34));
    }
}
