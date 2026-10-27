//! TSF display attributes for reading, converted text and the focused clause.
use crate::Lifetime;
use std::cell::Cell;
use windows::{
    Win32::{Foundation::*, UI::TextServices::*},
    core::*,
};

pub(crate) const GUIDS: [GUID; 3] = [
    GUID::from_u128(0x59537f55_c622_48e9_94f7_c47eaa103e10),
    GUID::from_u128(0x59537f55_c622_48e9_94f7_c47eaa103e11),
    GUID::from_u128(0x59537f55_c622_48e9_94f7_c47eaa103e12),
];
fn attributes(index: usize) -> TF_DISPLAYATTRIBUTE {
    TF_DISPLAYATTRIBUTE {
        lsStyle: if index == 0 { TF_LS_DOT } else { TF_LS_SOLID },
        fBoldLine: (index == 2).into(),
        bAttr: if index == 0 {
            TF_ATTR_INPUT
        } else if index == 2 {
            TF_ATTR_TARGET_CONVERTED
        } else {
            TF_ATTR_CONVERTED
        },
        ..Default::default()
    }
}
fn info(index: usize) -> ITfDisplayAttributeInfo {
    Info {
        index,
        value: Cell::new(attributes(index)),
        _lifetime: Lifetime::new(),
    }
    .into()
}
#[implement(ITfDisplayAttributeInfo)]
struct Info {
    index: usize,
    value: Cell<TF_DISPLAYATTRIBUTE>,
    _lifetime: Lifetime,
}
impl ITfDisplayAttributeInfo_Impl for Info_Impl {
    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUIDS[self.index])
    }
    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from(
            ["Reading", "Converted text", "Focused clause"][self.index],
        ))
    }
    fn GetAttributeInfo(&self, value: *mut TF_DISPLAYATTRIBUTE) -> Result<()> {
        if value.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            *value = self.value.get();
        }
        Ok(())
    }
    fn SetAttributeInfo(&self, value: *const TF_DISPLAYATTRIBUTE) -> Result<()> {
        if value.is_null() {
            return Err(E_POINTER.into());
        }
        self.value.set(unsafe { *value });
        Ok(())
    }
    fn Reset(&self) -> Result<()> {
        self.value.set(attributes(self.index));
        Ok(())
    }
}
#[implement(IEnumTfDisplayAttributeInfo)]
struct Enumerator {
    position: Cell<usize>,
    _lifetime: Lifetime,
}
impl IEnumTfDisplayAttributeInfo_Impl for Enumerator_Impl {
    fn Clone(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(enumerator(self.position.get()))
    }
    fn Reset(&self) -> Result<()> {
        self.position.set(0);
        Ok(())
    }
    fn Skip(&self, count: u32) -> Result<()> {
        let next = self.position.get().saturating_add(count as usize);
        self.position.set(next.min(GUIDS.len()));
        if next > GUIDS.len() {
            Err(S_FALSE.into())
        } else {
            Ok(())
        }
    }
    fn Next(
        &self,
        count: u32,
        output: *mut Option<ITfDisplayAttributeInfo>,
        fetched: *mut u32,
    ) -> Result<()> {
        if (count > 0 && output.is_null()) || (count != 1 && fetched.is_null()) {
            return Err(E_POINTER.into());
        }
        let position = self.position.get();
        let available = (GUIDS.len() - position).min(count as usize);
        unsafe {
            if !fetched.is_null() {
                *fetched = available as u32;
            }
            for offset in 0..available {
                output.add(offset).write(Some(info(position + offset)));
            }
        }
        self.position.set(position + available);
        if available < count as usize {
            Err(S_FALSE.into())
        } else {
            Ok(())
        }
    }
}
pub(crate) fn enumerator(position: usize) -> IEnumTfDisplayAttributeInfo {
    Enumerator {
        position: Cell::new(position),
        _lifetime: Lifetime::new(),
    }
    .into()
}
pub(crate) fn get(guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
    if guid.is_null() {
        return Err(E_POINTER.into());
    }
    let guid = unsafe { *guid };
    GUIDS
        .iter()
        .position(|candidate| *candidate == guid)
        .map(info)
        .ok_or_else(|| E_INVALIDARG.into())
}
