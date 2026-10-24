//! The standard Windows open dialogs. Each blocks until something is
//! picked (`Some`) or the dialog is cancelled (`None`).

use std::path::{Path, PathBuf};

use windows::core::{w, HSTRING, PWSTR};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FILEOPENDIALOGOPTIONS,
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};

/// A folder, starting at `start`.
pub fn pick_folder(start: &Path) -> Option<PathBuf> {
    unsafe {
        let dialog = open_dialog(FOS_PICKFOLDERS)?;
        if let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None) {
            let _ = dialog.SetFolder(&folder);
        }
        picked(&dialog)
    }
}

/// A program (an .exe file).
pub fn pick_program() -> Option<PathBuf> {
    unsafe {
        let dialog = open_dialog(FOS_FILEMUSTEXIST)?;
        dialog.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: w!("Programs"), pszSpec: w!("*.exe") }]).ok()?;
        let _ = dialog.SetTitle(w!("Pick the program"));
        picked(&dialog)
    }
}

fn open_dialog(options: FILEOPENDIALOGOPTIONS) -> Option<IFileOpenDialog> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let defaults = dialog.GetOptions().ok()?;
        dialog.SetOptions(defaults | options | FOS_FORCEFILESYSTEM).ok()?;
        Some(dialog)
    }
}

fn picked(dialog: &IFileOpenDialog) -> Option<PathBuf> {
    unsafe {
        dialog.Show(None).ok()?;
        let path: PWSTR = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let picked = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        picked.map(PathBuf::from)
    }
}
