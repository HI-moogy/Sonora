//! The notification-area (tray) icon. sonar-rs keeps routing audio and
//! recording clips while its window is closed, so closing the window only
//! hides it; the tray icon brings it back (left-click) or offers Open,
//! Start with Windows and Quit (right-click).

use std::sync::mpsc;
use std::thread::JoinHandle;

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ExtractIconExW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow,
    DispatchMessageW, EnumWindows, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND, MB_YESNO, IsWindowVisible, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SetForegroundWindow, SetWindowLongPtrW, ShowWindow, TrackPopupMenu, TranslateMessage, CREATESTRUCTW,
    GWLP_USERDATA, HICON, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, SW_HIDE, SW_RESTORE,
    SW_SHOW, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_CREATE, WM_DESTROY,
    WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};

use crate::autostart;
use crate::single_instance::WINDOW_TITLE;

/// What the user picked from the tray icon, for the app to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Open,
    Quit,
}

const CALLBACK_MESSAGE: u32 = WM_APP + 1;
const MENU_OPEN: usize = 1;
const MENU_AUTOSTART: usize = 2;
const MENU_QUIT: usize = 3;

/// The tray icon and the thread that owns it. Removed on drop.
pub struct Tray {
    window: isize,
    thread: Option<JoinHandle<()>>,
}

struct State {
    on_command: Box<dyn Fn(TrayCommand) + Send>,
    /// Sent by Explorer when it (re)creates the taskbar, e.g. after it
    /// restarts; the icon has to be added again then.
    taskbar_created: u32,
}

impl Tray {
    /// Adds the icon; `on_command` runs on the tray's own thread.
    pub fn start(on_command: impl Fn(TrayCommand) + Send + 'static) -> Result<Self, String> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("tray".into())
            .spawn(move || run(Box::new(on_command), ready_tx))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(window)) => Ok(Self { window, thread: Some(thread) }),
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(message)
            }
            Err(_) => Err("tray thread exited unexpectedly".into()),
        }
    }

    /// Shows a notification from the tray icon.
    pub fn notify(&self, title: &str, text: &str) {
        let mut data = icon_data(HWND(self.window as _));
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO;
        copy_into(&mut data.szInfoTitle, title);
        copy_into(&mut data.szInfo, text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.window as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(on_command: Box<dyn Fn(TrayCommand) + Send>, ready_tx: mpsc::Sender<Result<isize, String>>) {
    let window = unsafe {
        let instance = GetModuleHandleW(None).ok();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.unwrap_or_default().into(),
            lpszClassName: w!("sonar-rs-tray"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let state = Box::new(State { on_command, taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")) });
        // A hidden top-level window rather than a message-only one, so it
        // receives Explorer's TaskbarCreated broadcast.
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("sonar-rs-tray"),
            w!("sonar-rs tray"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            instance.map(Into::into),
            Some(Box::into_raw(state) as *const _),
        )
    };
    let window = match window {
        Ok(window) if add_icon(window) => window,
        Ok(window) => {
            unsafe {
                let _ = DestroyWindow(window);
            }
            let _ = ready_tx.send(Err("couldn't add the tray icon".into()));
            return;
        }
        Err(error) => {
            let _ = ready_tx.send(Err(format!("couldn't create the tray window: {error}")));
            return;
        }
    };
    let _ = ready_tx.send(Ok(window.0 as isize));
    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_CREATE {
        let create = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        return LRESULT(0);
    }
    let state = GetWindowLongPtrW(window, GWLP_USERDATA) as *const State;
    if state.is_null() {
        return DefWindowProcW(window, message, wparam, lparam);
    }
    let state = &*state;
    match message {
        CALLBACK_MESSAGE => {
            match lparam.0 as u32 {
                WM_LBUTTONUP => (state.on_command)(TrayCommand::Open),
                WM_RBUTTONUP => show_menu(window, state),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(window);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(window));
            drop(Box::from_raw(state as *const State as *mut State));
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ if message == state.taskbar_created => {
            add_icon(window);
            LRESULT(0)
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

unsafe fn show_menu(window: HWND, state: &State) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let autostart_flag = if autostart::is_enabled() { MF_CHECKED } else { MF_UNCHECKED };
    let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open sonar-rs"));
    let _ = AppendMenuW(menu, MF_STRING | autostart_flag, MENU_AUTOSTART, w!("Start with Windows"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit"));
    let mut cursor = POINT::default();
    let _ = GetCursorPos(&mut cursor);
    // Without this the menu doesn't close when you click elsewhere.
    let _ = SetForegroundWindow(window);
    let picked = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, cursor.x, cursor.y, Some(0), window, None);
    let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
    match picked.0 as usize {
        MENU_OPEN => (state.on_command)(TrayCommand::Open),
        MENU_AUTOSTART => {
            let _ = autostart::set_enabled(!autostart::is_enabled());
        }
        MENU_QUIT => (state.on_command)(TrayCommand::Quit),
        _ => {}
    }
}

fn add_icon(window: HWND) -> bool {
    let mut data = icon_data(window);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = CALLBACK_MESSAGE;
    data.hIcon = app_icon();
    copy_into(&mut data.szTip, "sonar-rs");
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool();
    if !data.hIcon.is_invalid() {
        // The shell keeps its own copy.
        unsafe {
            let _ = DestroyIcon(data.hIcon);
        }
    }
    added
}

fn icon_data(window: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        ..Default::default()
    }
}

/// The small icon embedded in sonar-rs.exe.
fn app_icon() -> HICON {
    let mut small = HICON::default();
    if let Ok(exe) = std::env::current_exe() {
        let path: Vec<u16> = exe.as_os_str().encode_wide_nul();
        unsafe {
            ExtractIconExW(PCWSTR(path.as_ptr()), 0, None, Some(&mut small), 1);
        }
    }
    small
}

trait EncodeWideNul {
    fn encode_wide_nul(&self) -> Vec<u16>;
}

impl EncodeWideNul for std::ffi::OsStr {
    fn encode_wide_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(std::iter::once(0)).collect()
    }
}

/// Copies `text` into a fixed-size UTF-16 field, truncated and terminated.
fn copy_into(field: &mut [u16], text: &str) {
    let units: Vec<u16> = text.encode_utf16().take(field.len() - 1).collect();
    field[..units.len()].copy_from_slice(&units);
    field[units.len()] = 0;
}

/// Asks before quitting if that would leave apps almost silent: ones
/// sonar-rs turned down that closed before it could turn them back up.
/// Windows only lets their volume be changed while they're open, so only a
/// running sonar-rs can fix them. Returns whether to quit.
pub fn confirm_quit() -> bool {
    let apps = audio_engine::session_volume::turned_down_apps_not_running();
    if apps.is_empty() {
        return true;
    }
    let text = format!(
        "These apps closed while sonar-rs had them turned down, and Windows will start them almost silent:\n\n{}\n\n\
         sonar-rs turns them back up when it's running while they're open. Quit anyway?",
        apps.join("\n")
    );
    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let answer = unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            w!("Quit sonar-rs?"),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND,
        )
    };
    answer == IDYES
}

/// This process's main window, found by title (it can't be reached
/// through eframe from the tray thread).
pub fn main_window() -> Option<HWND> {
    struct Search(Option<HWND>);
    unsafe extern "system" fn check(window: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        let mut pid = 0;
        GetWindowThreadProcessId(window, Some(&mut pid));
        let mut title = [0u16; 64];
        let len = GetWindowTextW(window, &mut title) as usize;
        if pid == std::process::id() && String::from_utf16_lossy(&title[..len]) == WINDOW_TITLE {
            search.0 = Some(window);
            return false.into();
        }
        true.into()
    }
    let mut search = Search(None);
    unsafe {
        let _ = EnumWindows(Some(check), LPARAM(&mut search as *mut Search as isize));
    }
    search.0
}

/// Shows `window` (hidden to the tray, minimized, or behind others) and
/// brings it to the front.
pub fn show_window(window: HWND) {
    unsafe {
        let _ = ShowWindow(window, if IsIconic(window).as_bool() { SW_RESTORE } else { SW_SHOW });
        let _ = SetForegroundWindow(window);
    }
}

pub fn show_main_window() {
    if let Some(window) = main_window() {
        show_window(window);
    }
}

/// Whether `window` is shown (not hidden to the tray).
pub fn is_shown(window: HWND) -> bool {
    unsafe { IsWindowVisible(window) }.as_bool()
}

pub fn hide_window(window: HWND) {
    unsafe {
        let _ = ShowWindow(window, SW_HIDE);
    }
}

pub fn hide_main_window() {
    if let Some(window) = main_window() {
        hide_window(window);
    }
}
