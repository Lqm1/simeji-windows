//! TSF display attributes for the reading, converted text and the focused clause.

use crate::dll::DllRef;
use std::cell::Cell;
use windows::{
    Win32::{
        Foundation::*,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::TextServices::*,
    },
    core::*,
};

/// How a part of the composition is underlined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attribute {
    /// Typed reading, before conversion: dotted underline.
    Input,
    /// Conversion output: solid underline.
    Converted,
    /// The clause being converted: bold underline.
    Focused,
}

impl Attribute {
    pub const ALL: [Self; 3] = [Self::Input, Self::Converted, Self::Focused];

    pub fn guid(self) -> GUID {
        GUID::from_u128(match self {
            Self::Input => 0x59537f55_c622_48e9_94f7_c47eaa103e10,
            Self::Converted => 0x59537f55_c622_48e9_94f7_c47eaa103e11,
            Self::Focused => 0x59537f55_c622_48e9_94f7_c47eaa103e12,
        })
    }

    fn description(self) -> &'static str {
        match self {
            Self::Input => "Reading",
            Self::Converted => "Converted text",
            Self::Focused => "Focused clause",
        }
    }

    fn style(self) -> TF_DISPLAYATTRIBUTE {
        TF_DISPLAYATTRIBUTE {
            lsStyle: if self == Self::Input {
                TF_LS_DOT
            } else {
                TF_LS_SOLID
            },
            fBoldLine: (self == Self::Focused).into(),
            bAttr: match self {
                Self::Input => TF_ATTR_INPUT,
                Self::Converted => TF_ATTR_CONVERTED,
                Self::Focused => TF_ATTR_TARGET_CONVERTED,
            },
            ..Default::default()
        }
    }
}

/// The atoms TSF assigned to the attribute GUIDs. `GUID_PROP_ATTRIBUTE`
/// values refer to attributes by atom.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AttributeAtoms([u32; 3]);

impl AttributeAtoms {
    pub fn register() -> Result<Self> {
        let categories: ITfCategoryMgr =
            unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)? };
        let mut atoms = [0; 3];
        for (atom, attribute) in atoms.iter_mut().zip(Attribute::ALL) {
            *atom = unsafe { categories.RegisterGUID(&attribute.guid())? };
        }
        Ok(Self(atoms))
    }

    pub fn get(self, attribute: Attribute) -> u32 {
        self.0[attribute as usize]
    }
}

fn info(attribute: Attribute) -> ITfDisplayAttributeInfo {
    AttributeInfo {
        attribute,
        value: Cell::new(attribute.style()),
        _dll: DllRef::new(),
    }
    .into()
}

#[implement(ITfDisplayAttributeInfo)]
struct AttributeInfo {
    attribute: Attribute,
    value: Cell<TF_DISPLAYATTRIBUTE>,
    _dll: DllRef,
}
impl ITfDisplayAttributeInfo_Impl for AttributeInfo_Impl {
    fn GetGUID(&self) -> Result<GUID> {
        Ok(self.attribute.guid())
    }
    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from(self.attribute.description()))
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
        self.value.set(self.attribute.style());
        Ok(())
    }
}

#[implement(IEnumTfDisplayAttributeInfo)]
struct AttributeEnumerator {
    position: Cell<usize>,
    _dll: DllRef,
}
impl IEnumTfDisplayAttributeInfo_Impl for AttributeEnumerator_Impl {
    fn Clone(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(enumerate_from(self.position.get()))
    }
    fn Reset(&self) -> Result<()> {
        self.position.set(0);
        Ok(())
    }
    fn Skip(&self, count: u32) -> Result<()> {
        let next = self.position.get().saturating_add(count as usize);
        self.position.set(next.min(Attribute::ALL.len()));
        if next > Attribute::ALL.len() {
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
        let available = (Attribute::ALL.len() - position).min(count as usize);
        unsafe {
            if !fetched.is_null() {
                *fetched = available as u32;
            }
            for (offset, attribute) in Attribute::ALL[position..][..available].iter().enumerate() {
                output.add(offset).write(Some(info(*attribute)));
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

pub(crate) fn enumerate_from(position: usize) -> IEnumTfDisplayAttributeInfo {
    AttributeEnumerator {
        position: Cell::new(position),
        _dll: DllRef::new(),
    }
    .into()
}

pub(crate) fn find(guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
    if guid.is_null() {
        return Err(E_POINTER.into());
    }
    let guid = unsafe { *guid };
    Attribute::ALL
        .into_iter()
        .find(|attribute| attribute.guid() == guid)
        .map(info)
        .ok_or_else(|| E_INVALIDARG.into())
}
