//! COM in-process server: the DLL entry points, the class factory and the
//! object count that tells COM when the DLL may be unloaded.

use crate::{CLSID_TEXT_SERVICE, registration, text_service::TextService};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};
use windows::{
    Win32::{Foundation::*, System::Com::*},
    core::*,
};

/// `DllMain` reason for the DLL being loaded into a process.
const DLL_PROCESS_ATTACH: u32 = 1;

static MODULE: AtomicUsize = AtomicUsize::new(0);
static OBJECTS: AtomicU32 = AtomicU32::new(0);
static LOCKS: AtomicU32 = AtomicU32::new(0);

/// Held by every COM object of this DLL, so that `DllCanUnloadNow` keeps the
/// DLL loaded while Windows still references one of them.
pub(crate) struct DllRef;
impl DllRef {
    pub fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}
impl Drop for DllRef {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// This DLL's module handle, recorded when the process loaded it.
pub(crate) fn module() -> HMODULE {
    HMODULE(MODULE.load(Ordering::SeqCst) as *mut _)
}

#[implement(IClassFactory)]
struct ClassFactory {
    _dll: DllRef,
}
impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> Result<()> {
        if out.is_null() || iid.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            *out = std::ptr::null_mut();
        }
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let object: IUnknown = TextService::new().into();
        unsafe { object.query(iid, out).ok() }
    }
    fn LockServer(&self, lock: BOOL) -> Result<()> {
        if lock.as_bool() {
            LOCKS.fetch_add(1, Ordering::SeqCst);
        } else {
            let _ = LOCKS.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
        }
        Ok(())
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllMain(module: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(module.0 as usize, Ordering::SeqCst);
    }
    true.into()
}

#[unsafe(no_mangle)]
/// # Safety
/// COM supplies readable GUID pointers and a writable interface output pointer.
pub unsafe extern "system" fn DllGetClassObject(
    class: *const GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if class.is_null() || iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    unsafe {
        *out = std::ptr::null_mut();
    }
    if unsafe { *class } != CLSID_TEXT_SERVICE {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = ClassFactory {
        _dll: DllRef::new(),
    }
    .into();
    unsafe { factory.query(iid, out) }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::SeqCst) == 0 && LOCKS.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    registration::module_path()
        .and_then(|path| registration::register(&path))
        .map_or_else(|e| e.code(), |_| S_OK)
}

#[unsafe(no_mangle)]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    registration::unregister().map_or_else(|e| e.code(), |_| S_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::Attribute;
    use windows::Win32::UI::TextServices::*;

    #[test]
    fn com_factory_interfaces_and_unload_lifetime() -> Result<()> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            let result = (|| {
                let mut output = std::ptr::null_mut();
                assert_eq!(DllCanUnloadNow(), S_OK);
                DllGetClassObject(&CLSID_TEXT_SERVICE, &IClassFactory::IID, &mut output).ok()?;
                let factory = IClassFactory::from_raw(output);
                assert_eq!(DllCanUnloadNow(), S_FALSE);
                let service: ITfTextInputProcessor = factory.CreateInstance(None)?;
                let _: ITfKeyEventSink = service.cast()?;
                let _: ITfCompositionSink = service.cast()?;
                {
                    let provider: ITfDisplayAttributeProvider = service.cast()?;
                    let enumerator = provider.EnumDisplayAttributeInfo()?;
                    let mut items: [Option<ITfDisplayAttributeInfo>; 4] =
                        std::array::from_fn(|_| None);
                    let mut count = 0;
                    let result = (enumerator.vtable().Next)(
                        enumerator.as_raw(),
                        4,
                        items.as_mut_ptr().cast(),
                        &mut count,
                    );
                    assert_eq!(result, S_FALSE);
                    assert_eq!(count, 3);
                    assert!(items[3].is_none());
                    assert_eq!(
                        items[0].as_ref().unwrap().GetGUID()?,
                        Attribute::Input.guid()
                    );
                    let cloned = enumerator.Clone()?;
                    assert_eq!(
                        (cloned.vtable().Next)(
                            cloned.as_raw(),
                            1,
                            items[3..].as_mut_ptr().cast(),
                            &mut count
                        ),
                        S_FALSE
                    );
                    cloned.Reset()?;
                    assert!(provider.GetDisplayAttributeInfo(&GUID::zeroed()).is_err());
                }
                drop(service);
                factory.LockServer(true)?;
                drop(factory);
                assert_eq!(DllCanUnloadNow(), S_FALSE);
                let factory: IClassFactory = ClassFactory {
                    _dll: DllRef::new(),
                }
                .into();
                factory.LockServer(false)?;
                drop(factory);
                assert_eq!(DllCanUnloadNow(), S_OK);
                assert_eq!(
                    DllGetClassObject(std::ptr::null(), &IClassFactory::IID, &mut output),
                    E_POINTER
                );
                Ok(())
            })();
            CoUninitialize();
            result
        }
    }
}
