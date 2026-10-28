//! Machine-wide registration of the text service, used by `simeji-setup` and
//! the `DllRegisterServer` entry points. Registering needs administrator rights.

use crate::{CLSID_TEXT_SERVICE, DISPLAY_NAME, GUID_PROFILE, LANGID_JAPANESE, dll};
use std::path::{Path, PathBuf};
use windows::{
    Win32::{
        Foundation::*,
        System::{Com::*, LibraryLoader::*, Registry::*},
        UI::{TextServices::*, WindowsAndMessaging::RT_GROUP_ICON},
    },
    core::*,
};

/// A keyboard text service with display attributes that works in Windows
/// Store apps, without COM activation, shows its input mode through
/// compartments and the input indicator, and lets applications draw its
/// candidates.
const CATEGORIES: [&GUID; 7] = [
    &GUID_TFCAT_TIP_KEYBOARD,
    &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
    &GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    &GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    &GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
    &GUID_TFCAT_TIPCAP_COMLESS,
    &GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
];
/// The icon group the input indicator shows for the profile, linked by the
/// build script; the profile refers to it as the first icon of the DLL.
const BRAND_ICON: usize = 1;
/// `InstallLayoutOrTip` flag that removes the input method instead.
const ILOT_UNINSTALL: u32 = 0x1;

/// Loads the packaged DLL and checks its COM exports without registering it.
pub fn verify(path: &Path) -> Result<()> {
    unsafe {
        let module = LoadLibraryExW(
            &HSTRING::from(path.to_string_lossy().as_ref()),
            None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )?;
        let result = (|| {
            type GetClass = unsafe extern "system" fn(
                *const GUID,
                *const GUID,
                *mut *mut std::ffi::c_void,
            ) -> HRESULT;
            type CanUnload = unsafe extern "system" fn() -> HRESULT;
            let get_class: GetClass = std::mem::transmute(
                GetProcAddress(module, s!("DllGetClassObject")).ok_or_else(Error::from_thread)?,
            );
            let can_unload: CanUnload = std::mem::transmute(
                GetProcAddress(module, s!("DllCanUnloadNow")).ok_or_else(Error::from_thread)?,
            );
            let mut output = std::ptr::null_mut();
            get_class(&CLSID_TEXT_SERVICE, &IClassFactory::IID, &mut output).ok()?;
            let factory = IClassFactory::from_raw(output);
            let service: ITfTextInputProcessorEx = factory.CreateInstance(None)?;
            let _: ITfKeyEventSink = service.cast()?;
            let _: ITfCompartmentEventSink = service.cast()?;
            let _: ITfCompositionSink = service.cast()?;
            let _: ITfDisplayAttributeProvider = service.cast()?;
            if FindResourceW(
                Some(module),
                PCWSTR(BRAND_ICON as *const u16),
                RT_GROUP_ICON,
            )
            .is_invalid()
            {
                return Err(Error::new(E_FAIL, "Packaged COM DLL has no branding icon"));
            }
            drop(service);
            drop(factory);
            if can_unload() != S_OK {
                return Err(Error::new(E_FAIL, "Packaged COM DLL retained objects"));
            }
            Ok(())
        })();
        let _ = FreeLibrary(module);
        result
    }
}

/// The path of this DLL as loaded into the current process.
pub fn module_path() -> Result<PathBuf> {
    let mut path = vec![0u16; 32768];
    let length = unsafe { GetModuleFileNameW(Some(dll::module()), &mut path) } as usize;
    if length == 0 || length >= path.len() {
        return Err(Error::from_thread());
    }
    Ok(PathBuf::from(String::from_utf16_lossy(&path[..length])))
}

/// Registers the DLL at `path` as the Japanese keyboard profile for all
/// users and adds it to the current user's input methods.
pub fn register(path: &Path) -> Result<()> {
    if !path.is_absolute() || !path.is_file() {
        return Err(Error::new(
            E_INVALIDARG,
            "The IME DLL must be an existing absolute path",
        ));
    }
    with_com(|| unsafe {
        let profiles: ITfInputProcessorProfileMgr =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?;
        // The profile is registered before the COM class, so that missing
        // rights do not leave a class without a profile.
        profiles
            .RegisterProfile(
                &CLSID_TEXT_SERVICE,
                LANGID_JAPANESE,
                &GUID_PROFILE,
                &DISPLAY_NAME.encode_utf16().collect::<Vec<_>>(),
                &path.to_string_lossy().encode_utf16().collect::<Vec<_>>(),
                0,
                Default::default(),
                0,
                true,
                0,
            )
            .map_err(|error| {
                Error::new(
                    error.code(),
                    "TSF RegisterProfile failed; registration requires administrator rights",
                )
            })?;
        let categories: ITfCategoryMgr =
            CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
        for category in CATEGORIES {
            categories.RegisterCategory(&CLSID_TEXT_SERVICE, category, &CLSID_TEXT_SERVICE)?;
        }
        delete_class(HKEY_CURRENT_USER)?;
        set_class_value("", None, DISPLAY_NAME)?;
        let path = path.to_string_lossy();
        set_class_value("InprocServer32", None, &path)?;
        set_class_value("InprocServer32", Some(w!("ThreadingModel")), "Apartment")?;
        install_layout_or_tip(0)
    })
}

/// Removes everything [`register`] created.
pub fn unregister() -> Result<()> {
    with_com(|| unsafe {
        let _ = install_layout_or_tip(ILOT_UNINSTALL);
        let profiles: ITfInputProcessorProfiles =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?;
        let present = profiles
            .GetLanguageProfileDescription(&CLSID_TEXT_SERVICE, LANGID_JAPANESE, &GUID_PROFILE)
            .is_ok();
        let categories: ITfCategoryMgr =
            CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
        for category in CATEGORIES {
            let _ =
                categories.UnregisterCategory(&CLSID_TEXT_SERVICE, category, &CLSID_TEXT_SERVICE);
        }
        let profile = if present {
            profiles.Unregister(&CLSID_TEXT_SERVICE)
        } else {
            Ok(())
        };
        delete_class(HKEY_CURRENT_USER)?;
        delete_class(HKEY_LOCAL_MACHINE)?;
        profile
    })
}

fn with_com(work: impl FnOnce() -> Result<()>) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let result = work();
    unsafe {
        CoUninitialize();
    }
    result
}

/// `InstallLayoutOrTip` from `input.dll`, the supported way to add the
/// profile to the user's input methods or remove it from them.
fn install_layout_or_tip(flags: u32) -> Result<()> {
    type InstallLayoutOrTip = unsafe extern "system" fn(PCWSTR, u32) -> BOOL;
    let profile = HSTRING::from(input_method_id());
    unsafe {
        let input = LoadLibraryExW(w!("input.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)?;
        let result = match GetProcAddress(input, s!("InstallLayoutOrTip")) {
            Some(function) => {
                let install: InstallLayoutOrTip = std::mem::transmute(function);
                install(PCWSTR(profile.as_ptr()), flags).ok()
            }
            None => Err(Error::from_thread()),
        };
        let _ = FreeLibrary(input);
        result
    }
}

/// `<language>:{<text service>}{<profile>}`, as `InstallLayoutOrTip` expects.
fn input_method_id() -> String {
    format!("{LANGID_JAPANESE:04X}:{{{CLSID_TEXT_SERVICE:?}}}{{{GUID_PROFILE:?}}}")
}

fn class_key(subkey: &str) -> HSTRING {
    let class = format!(r"Software\Classes\CLSID\{{{CLSID_TEXT_SERVICE:?}}}");
    HSTRING::from(if subkey.is_empty() {
        class
    } else {
        format!(r"{class}\{subkey}")
    })
}

fn set_class_value(subkey: &str, name: Option<PCWSTR>, value: &str) -> Result<()> {
    let value = value.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let bytes = value
        .iter()
        .flat_map(|unit| unit.to_le_bytes())
        .collect::<Vec<_>>();
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &class_key(subkey),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let result = RegSetValueExW(
            key,
            name.unwrap_or(PCWSTR::null()),
            None,
            REG_SZ,
            Some(&bytes),
        )
        .ok();
        let _ = RegCloseKey(key);
        result
    }
}

/// A per-user class key would override the machine-wide one for that user.
fn delete_class(root: HKEY) -> Result<()> {
    let result = unsafe { RegDeleteTreeW(root, &class_key("")) };
    if result == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        result.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_profile_for_install_layout_or_tip() {
        assert_eq!(
            input_method_id(),
            "0411:{EAC9F5F0-91B4-4F85-93D8-7A1669E5D7BD}{549AE946-7705-4AF7-A1BD-37936D432991}"
        );
    }
}
