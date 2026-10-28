//! The COM server as Windows loads it. `DllCanUnloadNow` counts every
//! object of the DLL, so this runs in a process of its own.

use simeji_ime::{CLSID_TEXT_SERVICE, DllCanUnloadNow, DllGetClassObject};
use windows::{
    Win32::{Foundation::*, System::Com::*, UI::TextServices::*},
    core::*,
};

unsafe fn class_factory() -> Result<IClassFactory> {
    let mut output = std::ptr::null_mut();
    unsafe {
        DllGetClassObject(&CLSID_TEXT_SERVICE, &IClassFactory::IID, &mut output).ok()?;
        Ok(IClassFactory::from_raw(output))
    }
}

#[test]
fn com_factory_interfaces_and_unload_lifetime() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let result = (|| {
            assert_eq!(DllCanUnloadNow(), S_OK);
            let factory = class_factory()?;
            assert_eq!(DllCanUnloadNow(), S_FALSE);
            let service: ITfTextInputProcessor = factory.CreateInstance(None)?;
            let _: ITfTextInputProcessorEx = service.cast()?;
            let _: ITfCompartmentEventSink = service.cast()?;
            let _: ITfKeyEventSink = service.cast()?;
            let _: ITfCompositionSink = service.cast()?;
            {
                let provider: ITfDisplayAttributeProvider = service.cast()?;
                let enumerator = provider.EnumDisplayAttributeInfo()?;
                let mut items: [Option<ITfDisplayAttributeInfo>; 4] = std::array::from_fn(|_| None);
                let mut count = 0;
                let result = (enumerator.vtable().Next)(
                    enumerator.as_raw(),
                    4,
                    items.as_mut_ptr().cast(),
                    &mut count,
                );
                assert_eq!((result, count), (S_FALSE, 3));
                assert!(items[3].is_none());
                let first = items[0].as_ref().unwrap().GetGUID()?;
                assert_eq!(provider.GetDisplayAttributeInfo(&first)?.GetGUID()?, first);
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
            let factory = class_factory()?;
            factory.LockServer(false)?;
            drop(factory);
            assert_eq!(DllCanUnloadNow(), S_OK);
            let mut output = std::ptr::null_mut();
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
