//! Windows' per-app output device: the "Output device" each app can be
//! given in Settings > System > Sound > Volume mixer. An app pointed at its
//! channel's device plays there itself, with no delay added, instead of
//! being captured and played again by the mixer (about 45 ms of Windows'
//! own capture delay, plus the mixer's).
//!
//! The interface is undocumented: Windows' Settings app (and EarTrumpet)
//! use it, through `Windows.Media.Internal.AudioPolicyConfig` (Windows 10
//! 21H2 and later). Windows keeps the choice per app, but only takes it
//! for a process that has an audio session, and an app's audio already
//! playing moves only when the app opens it again.

use windows::core::{HRESULT, HSTRING};
use windows::Win32::Media::Audio::{eConsole, eMultimedia, eRender, EDataFlow, ERole};
use windows::Win32::System::WinRT::RoGetActivationFactory;

/// How the policy names an output: by its device interface path.
const PATH_PREFIX: &str = "\\\\?\\SWD#MMDEVAPI#";
const RENDER_INTERFACE: &str = "#{e6327cad-dcec-4949-ae8a-991e976a79d2}";

/// The policy's activation factory. A WinRT interface: its first three
/// slots are IInspectable's, and only the last three methods are used.
#[windows_core::interface("ab3d4648-e242-459f-b02f-541c70306324")]
unsafe trait IAudioPolicyConfigFactory: windows_core::IUnknown {
    fn get_iids(&self) -> HRESULT;
    fn get_runtime_class_name(&self) -> HRESULT;
    fn get_trust_level(&self) -> HRESULT;
    fn add_ctx_volume_change(&self) -> HRESULT;
    fn remove_ctx_volume_changed(&self) -> HRESULT;
    fn add_ringer_vibrate_state_changed(&self) -> HRESULT;
    fn remove_ringer_vibrate_state_change(&self) -> HRESULT;
    fn set_volume_group_gain_for_id(&self) -> HRESULT;
    fn get_volume_group_gain_for_id(&self) -> HRESULT;
    fn get_active_volume_group_for_endpoint_id(&self) -> HRESULT;
    fn get_volume_groups_for_endpoint(&self) -> HRESULT;
    fn get_current_volume_context(&self) -> HRESULT;
    fn set_volume_group_mute_for_id(&self) -> HRESULT;
    fn get_volume_group_mute_for_id(&self) -> HRESULT;
    fn set_ringer_vibrate_state(&self) -> HRESULT;
    fn get_ringer_vibrate_state(&self) -> HRESULT;
    fn set_preferred_chat_application(&self) -> HRESULT;
    fn reset_preferred_chat_application(&self) -> HRESULT;
    fn get_preferred_chat_application(&self) -> HRESULT;
    fn get_current_chat_applications(&self) -> HRESULT;
    fn add_chat_context_changed(&self) -> HRESULT;
    fn remove_chat_context_changed(&self) -> HRESULT;
    fn set_persisted_default_audio_endpoint(
        &self,
        process: u32,
        flow: EDataFlow,
        role: ERole,
        device: *mut core::ffi::c_void,
    ) -> HRESULT;
    fn get_persisted_default_audio_endpoint(
        &self,
        process: u32,
        flow: EDataFlow,
        role: ERole,
        device: *mut *mut core::ffi::c_void,
    ) -> HRESULT;
    fn clear_all_persisted_application_default_endpoints(&self) -> HRESULT;
}

fn policy() -> windows::core::Result<IAudioPolicyConfigFactory> {
    unsafe { RoGetActivationFactory(&HSTRING::from("Windows.Media.Internal.AudioPolicyConfig")) }
}

/// Points process `pid` (one with an audio session) at output `device_id`
/// (an id from the device lists), or back at Windows' default with `None`.
pub fn set(pid: u32, device_id: Option<&str>) -> windows::core::Result<()> {
    let policy = policy()?;
    let device = device_id.map(|id| HSTRING::from(format!("{PATH_PREFIX}{id}{RENDER_INTERFACE}"))).unwrap_or_default();
    // Apps open "the default device" for either role; both follow the choice.
    for role in [eConsole, eMultimedia] {
        unsafe {
            // Borrowed: the policy copies the string.
            policy.set_persisted_default_audio_endpoint(pid, eRender, role, std::mem::transmute_copy(&device)).ok()?;
        }
    }
    Ok(())
}

/// The output process `pid` is pointed at (an id from the device lists),
/// or `None` if it plays on Windows' default.
pub fn get(pid: u32) -> windows::core::Result<Option<String>> {
    let policy = policy()?;
    let mut device = HSTRING::new();
    unsafe {
        policy
            .get_persisted_default_audio_endpoint(pid, eRender, eMultimedia, &mut device as *mut HSTRING as *mut _)
            .ok()?;
    }
    Ok(device_id_from_path(&device.to_string()))
}

/// The device id in a policy path; `None` for an empty one (the default).
fn device_id_from_path(path: &str) -> Option<String> {
    let id = path.strip_prefix(PATH_PREFIX).unwrap_or(path);
    let id = id.strip_suffix(RENDER_INTERFACE).unwrap_or(id);
    (!id.is_empty()).then(|| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_device_ids_from_policy_paths() {
        let id = "{0.0.0.00000000}.{7640f246-75fd-4be2-b5ba-35c7ee17b58e}";
        assert_eq!(device_id_from_path(&format!("{PATH_PREFIX}{id}{RENDER_INTERFACE}")).as_deref(), Some(id));
        assert_eq!(device_id_from_path(""), None);
    }
}
