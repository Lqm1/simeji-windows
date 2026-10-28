//! The input mode in TSF compartments, which the application, the Windows
//! input indicator and the touch keyboard read and change, and the mode
//! button the input indicator shows.

use crate::{CLSID_TEXT_SERVICE, composer::InputMode, dll::DllRef, mode_icon::mode_icon};
use std::cell::{Cell, RefCell};
use windows::{
    Win32::{
        Foundation::*,
        System::{
            Ole::{CONNECT_E_ADVISELIMIT, CONNECT_E_CANNOTCONNECT, CONNECT_E_NOCONNECTION},
            Variant::VARIANT,
        },
        UI::{TextServices::*, WindowsAndMessaging::HICON},
    },
    core::*,
};

const CONVERSION_MODE: i32 =
    (TF_CONVERSIONMODE_NATIVE | TF_CONVERSIONMODE_FULLSHAPE | TF_CONVERSIONMODE_ROMAN) as i32;
const BUTTON_SINK_COOKIE: u32 = 1;

fn compartment(thread: &ITfThreadMgr, guid: &GUID) -> Result<ITfCompartment> {
    unsafe { thread.cast::<ITfCompartmentMgr>()?.GetCompartment(guid) }
}

pub(crate) fn open_close(thread: &ITfThreadMgr) -> Result<ITfCompartment> {
    compartment(thread, &GUID_COMPARTMENT_KEYBOARD_OPENCLOSE)
}

pub(crate) fn read(open_close: &ITfCompartment) -> InputMode {
    let open = unsafe { open_close.GetValue() }
        .ok()
        .and_then(|value| i32::try_from(&value).ok())
        .is_some_and(|value| value != 0);
    if open {
        InputMode::Japanese
    } else {
        InputMode::Direct
    }
}

pub(crate) fn write(open_close: &ITfCompartment, client_id: u32, mode: InputMode) -> Result<()> {
    let open = i32::from(mode == InputMode::Japanese);
    unsafe { open_close.SetValue(client_id, &VARIANT::from(open)) }
}

/// Declares hiragana with romaji input for the input indicator and the touch keyboard.
pub(crate) fn write_conversion_mode(thread: &ITfThreadMgr, client_id: u32) -> Result<()> {
    let conversion = compartment(thread, &GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION)?;
    unsafe { conversion.SetValue(client_id, &VARIANT::from(CONVERSION_MODE)) }
}

#[implement(ITfLangBarItemButton, ITfSource)]
pub(crate) struct InputModeButton {
    open_close: ITfCompartment,
    client_id: u32,
    mode: Cell<InputMode>,
    sink: RefCell<Option<ITfLangBarItemSink>>,
    _dll: DllRef,
}

impl InputModeButton {
    pub fn new(open_close: ITfCompartment, client_id: u32, mode: InputMode) -> Self {
        Self {
            open_close,
            client_id,
            mode: Cell::new(mode),
            sink: RefCell::new(None),
            _dll: DllRef::new(),
        }
    }

    pub fn update(&self, mode: InputMode) {
        if self.mode.replace(mode) == mode {
            return;
        }
        let sink = self.sink.borrow().clone();
        if let Some(sink) = sink {
            unsafe {
                let _ = sink.OnUpdate(TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP);
            }
        }
    }

    fn glyph(&self) -> &'static str {
        match self.mode.get() {
            InputMode::Japanese => "あ",
            InputMode::Direct => "A",
        }
    }
}

impl ITfLangBarItem_Impl for InputModeButton_Impl {
    fn GetInfo(&self, info: *mut TF_LANGBARITEMINFO) -> Result<()> {
        if info.is_null() {
            return Err(E_INVALIDARG.into());
        }
        let mut description = [0u16; 32];
        for (slot, unit) in description.iter_mut().zip("入力モード".encode_utf16()) {
            *slot = unit;
        }
        unsafe {
            *info = TF_LANGBARITEMINFO {
                clsidService: CLSID_TEXT_SERVICE,
                guidItem: GUID_LBI_INPUTMODE,
                dwStyle: TF_LBI_STYLE_BTN_BUTTON | TF_LBI_STYLE_SHOWNINTRAY,
                ulSort: 0,
                szDescription: description,
            };
        }
        Ok(())
    }

    fn GetStatus(&self) -> Result<u32> {
        Ok(0)
    }

    fn Show(&self, _show: BOOL) -> Result<()> {
        Ok(())
    }

    fn GetTooltipString(&self) -> Result<BSTR> {
        Ok(BSTR::from(match self.mode.get() {
            InputMode::Japanese => "ひらがな",
            InputMode::Direct => "半角英数",
        }))
    }
}

impl ITfLangBarItemButton_Impl for InputModeButton_Impl {
    fn OnClick(&self, click: TfLBIClick, _point: &POINT, _area: *const RECT) -> Result<()> {
        if click != TF_LBI_CLK_LEFT {
            return Ok(());
        }
        let next = match read(&self.open_close) {
            InputMode::Japanese => InputMode::Direct,
            InputMode::Direct => InputMode::Japanese,
        };
        write(&self.open_close, self.client_id, next)
    }

    fn InitMenu(&self, _menu: Ref<ITfMenu>) -> Result<()> {
        Ok(())
    }

    fn OnMenuSelect(&self, _id: u32) -> Result<()> {
        Ok(())
    }

    fn GetIcon(&self) -> Result<HICON> {
        mode_icon(self.glyph())
    }

    fn GetText(&self) -> Result<BSTR> {
        Ok(BSTR::from(self.glyph()))
    }
}

impl ITfSource_Impl for InputModeButton_Impl {
    fn AdviseSink(&self, iid: *const GUID, sink: Ref<IUnknown>) -> Result<u32> {
        if iid.is_null() || unsafe { *iid } != ITfLangBarItemSink::IID {
            return Err(CONNECT_E_CANNOTCONNECT.into());
        }
        if self.sink.borrow().is_some() {
            return Err(CONNECT_E_ADVISELIMIT.into());
        }
        *self.sink.borrow_mut() = Some(sink.ok()?.cast()?);
        Ok(BUTTON_SINK_COOKIE)
    }

    fn UnadviseSink(&self, cookie: u32) -> Result<()> {
        if cookie != BUTTON_SINK_COOKIE || self.sink.borrow_mut().take().is_none() {
            return Err(CONNECT_E_NOCONNECTION.into());
        }
        Ok(())
    }
}
