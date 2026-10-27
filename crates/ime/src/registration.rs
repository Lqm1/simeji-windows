use crate::{MODULE, PROFILE, SERVICE};
use std::path::{Path, PathBuf};
use windows::{
    Win32::{
        Foundation::*,
        System::{Com::*, LibraryLoader::*, Registry::*},
        UI::TextServices::*,
    },
    core::*,
};

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
            get_class(&SERVICE, &IClassFactory::IID, &mut output).ok()?;
            let factory = IClassFactory::from_raw(output);
            let service: ITfTextInputProcessor = factory.CreateInstance(None)?;
            let _: ITfKeyEventSink = service.cast()?;
            let _: ITfCompositionSink = service.cast()?;
            let _: ITfDisplayAttributeProvider = service.cast()?;
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
pub fn module_path() -> Result<PathBuf> {
    let mut path = vec![0u16; 32768];
    let length = unsafe {
        GetModuleFileNameW(
            Some(HMODULE(
                MODULE.load(std::sync::atomic::Ordering::SeqCst) as *mut _
            )),
            &mut path,
        )
    } as usize;
    if length == 0 || length >= path.len() {
        return Err(Error::from_thread());
    }
    Ok(PathBuf::from(String::from_utf16_lossy(&path[..length])))
}
fn key_path() -> String {
    format!("Software\\Classes\\CLSID\\{{{SERVICE:?}}}")
}
fn set_string(path: &str, name: PCWSTR, value: &str) -> Result<()> {
    let path = HSTRING::from(path);
    let value = value.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let bytes = value
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &path,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let result = RegSetValueExW(key, name, Some(0), REG_SZ, Some(&bytes)).ok();
        let _ = RegCloseKey(key);
        result
    }
}
pub fn register(path: &Path) -> Result<()> {
    if !path.is_absolute() || !path.is_file() {
        return Err(Error::new(
            E_INVALIDARG,
            "The IME DLL must be an existing absolute path",
        ));
    }
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    initialized.ok()?;
    let result = (|| unsafe {
        let profiles: ITfInputProcessorProfileMgr =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?;
        // Register through the current TSF API before creating our COM keys.
        // A privilege failure must not leave an orphaned COM registration.
        profiles
            .RegisterProfile(
                &SERVICE,
                0x0411,
                &PROFILE,
                &"Simeji Windows".encode_utf16().collect::<Vec<_>>(),
                &path.to_string_lossy().encode_utf16().collect::<Vec<_>>(),
                0,
                Default::default(),
                0,
                false,
                0,
            )
            .map_err(|error| {
                Error::new(
                    error.code(),
                    "TSF RegisterProfile failed; system registration may require administrator rights",
                )
            })?;
        let categories: ITfCategoryMgr =
            CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
        categories.RegisterCategory(&SERVICE, &GUID_TFCAT_TIP_KEYBOARD, &SERVICE)?;
        categories.RegisterCategory(&SERVICE, &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER, &SERVICE)?;
        set_string(&key_path(), PCWSTR::null(), "Simeji Windows")?;
        let server = format!("{}\\InprocServer32", key_path());
        set_string(&server, PCWSTR::null(), &path.to_string_lossy())?;
        set_string(&server, w!("ThreadingModel"), "Apartment")?;
        let user_profiles: ITfInputProcessorProfiles = profiles.cast()?;
        user_profiles.EnableLanguageProfile(&SERVICE, 0x0411, &PROFILE, true)?;
        Ok(())
    })();
    unsafe {
        CoUninitialize();
    }
    result
}
pub fn unregister() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let result = (|| unsafe {
        let profiles: ITfInputProcessorProfiles =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?;
        let present = profiles
            .GetLanguageProfileDescription(&SERVICE, 0x0411, &PROFILE)
            .is_ok();
        let categories: ITfCategoryMgr =
            CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
        let _ = categories.UnregisterCategory(&SERVICE, &GUID_TFCAT_TIP_KEYBOARD, &SERVICE);
        let _ =
            categories.UnregisterCategory(&SERVICE, &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER, &SERVICE);
        let profile_result = if present {
            profiles.Unregister(&SERVICE)
        } else {
            Ok(())
        };
        let key_result = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key_path()));
        if key_result != ERROR_FILE_NOT_FOUND {
            key_result.ok()?;
        }
        profile_result
    })();
    unsafe {
        CoUninitialize();
    }
    result
}
