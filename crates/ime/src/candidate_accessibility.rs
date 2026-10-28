//! UI Automation for the candidate window, following Microsoft's IME
//! accessibility convention: a list with the automation ID
//! `IME_Candidate_Window` whose items are the candidates.
#![allow(non_upper_case_globals)]

use crate::dll::DllRef;
use std::{cell::RefCell, rc::Rc};
use windows::{
    Win32::{
        Foundation::*,
        System::{
            Com::SAFEARRAY,
            Ole::{SafeArrayCreateVector, SafeArrayPutElement},
            Variant::{VARIANT, VT_I4, VT_UNKNOWN},
        },
        UI::{Accessibility::*, WindowsAndMessaging::GetWindowRect},
    },
    core::*,
};

const AUTOMATION_ID: &str = "IME_Candidate_Window";
const LIST_NAME: &str = "変換候補";

/// What the window shows, read by the window procedure and the providers.
#[derive(Debug, Default)]
pub(crate) struct ListState {
    pub items: Vec<String>,
    pub cloud: Vec<bool>,
    pub selected: usize,
    pub row_height: i32,
    pub padding: i32,

    pub selecting: bool,
}

pub(crate) type SharedList = Rc<RefCell<ListState>>;

/// UIA's way of returning no element from a method that returns one.
fn no_element<T>() -> Result<T> {
    Err(Error::from_hresult(S_OK))
}

/// The user selects candidates with the keyboard through the composition.
fn selection_is_read_only() -> Error {
    Error::from_hresult(HRESULT(UIA_E_INVALIDOPERATION as i32))
}

fn screen_rect(rect: RECT) -> UiaRect {
    UiaRect {
        left: f64::from(rect.left),
        top: f64::from(rect.top),
        width: f64::from(rect.right - rect.left),
        height: f64::from(rect.bottom - rect.top),
    }
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot,
    ISelectionProvider
)]
pub(crate) struct CandidateList {
    window: HWND,
    list: SharedList,
    _dll: DllRef,
}

impl CandidateList {
    pub fn new(window: HWND, list: SharedList) -> Self {
        Self {
            window,
            list,
            _dll: DllRef::new(),
        }
    }
}

impl CandidateList_Impl {
    fn item(&self, index: usize) -> Result<IRawElementProviderFragment> {
        if index >= self.list.borrow().items.len() {
            return no_element();
        }
        Ok(CandidateItem {
            root: self.to_interface(),
            window: self.window,
            list: self.list.clone(),
            index,
            _dll: DllRef::new(),
        }
        .into())
    }

    pub fn selected_item(&self) -> Result<IRawElementProviderSimple> {
        let selected = self.list.borrow().selected;
        self.item(selected)?.cast()
    }
}

impl IRawElementProviderSimple_Impl for CandidateList_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        if pattern == UIA_SelectionPatternId {
            Ok(self.to_interface::<ISelectionProvider>().cast()?)
        } else {
            no_element()
        }
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        Ok(match property {
            UIA_AutomationIdPropertyId => VARIANT::from(BSTR::from(AUTOMATION_ID)),
            UIA_NamePropertyId => VARIANT::from(BSTR::from(LIST_NAME)),
            UIA_ControlTypePropertyId => VARIANT::from(UIA_ListControlTypeId.0),
            UIA_IsKeyboardFocusablePropertyId => VARIANT::from(false),
            _ => VARIANT::default(),
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        unsafe { UiaHostProviderFromHwnd(self.window) }
    }
}

impl IRawElementProviderFragment_Impl for CandidateList_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let count = self.list.borrow().items.len();
        match direction {
            NavigateDirection_FirstChild => self.item(0),
            NavigateDirection_LastChild if count > 0 => self.item(count - 1),
            _ => no_element(),
        }
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(self.window, &mut rect)? };
        Ok(screen_rect(rect))
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.to_interface())
    }
}

impl IRawElementProviderFragmentRoot_Impl for CandidateList_Impl {
    fn ElementProviderFromPoint(&self, _x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(self.window, &mut rect)? };
        let (row_height, padding) = {
            let list = self.list.borrow();
            (list.row_height.max(1), list.padding)
        };
        let offset = y as i32 - rect.top - padding;
        match usize::try_from(offset / row_height) {
            Ok(index) if offset >= 0 => self.item(index),
            _ => Ok(self.to_interface()),
        }
    }

    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        no_element()
    }
}

impl ISelectionProvider_Impl for CandidateList_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        if !self.list.borrow().selecting {
            return Ok(unsafe { SafeArrayCreateVector(VT_UNKNOWN, 0, 0) });
        }
        let selected = self.selected_item()?;
        unsafe {
            let array = SafeArrayCreateVector(VT_UNKNOWN, 0, 1);
            if array.is_null() {
                return Err(E_OUTOFMEMORY.into());
            }
            let unknown: IUnknown = selected.cast()?;
            SafeArrayPutElement(array, &0, unknown.as_raw())?;
            Ok(array)
        }
    }

    fn CanSelectMultiple(&self) -> Result<BOOL> {
        Ok(false.into())
    }

    fn IsSelectionRequired(&self) -> Result<BOOL> {
        Ok(false.into())
    }
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    ISelectionItemProvider
)]
struct CandidateItem {
    root: IRawElementProviderFragmentRoot,
    window: HWND,
    list: SharedList,
    index: usize,
    _dll: DllRef,
}

impl CandidateItem_Impl {
    fn sibling(&self, index: usize) -> Result<IRawElementProviderFragment> {
        if index >= self.list.borrow().items.len() {
            return no_element();
        }
        Ok(CandidateItem {
            root: self.root.clone(),
            window: self.window,
            list: self.list.clone(),
            index,
            _dll: DllRef::new(),
        }
        .into())
    }
}

impl IRawElementProviderSimple_Impl for CandidateItem_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        if pattern == UIA_SelectionItemPatternId {
            Ok(self.to_interface::<ISelectionItemProvider>().cast()?)
        } else {
            no_element()
        }
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let list = self.list.borrow();
        Ok(match property {
            UIA_NamePropertyId => {
                list.items
                    .get(self.index)
                    .map_or_else(VARIANT::default, |item| {
                        let name = if list.cloud.get(self.index) == Some(&true) {
                            format!("{item}、クラウド候補")
                        } else {
                            item.clone()
                        };
                        VARIANT::from(BSTR::from(name))
                    })
            }
            UIA_ControlTypePropertyId => VARIANT::from(UIA_ListItemControlTypeId.0),
            UIA_SelectionItemIsSelectedPropertyId => {
                VARIANT::from(list.selecting && list.selected == self.index)
            }
            _ => VARIANT::default(),
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        no_element()
    }
}

impl IRawElementProviderFragment_Impl for CandidateItem_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        match direction {
            NavigateDirection_Parent => self.root.cast(),
            NavigateDirection_NextSibling => self.sibling(self.index + 1),
            NavigateDirection_PreviousSibling if self.index > 0 => self.sibling(self.index - 1),
            _ => no_element(),
        }
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        let parts = [UiaAppendRuntimeId as i32, self.index as i32 + 1];
        unsafe {
            let array = SafeArrayCreateVector(VT_I4, 0, parts.len() as u32);
            if array.is_null() {
                return Err(E_OUTOFMEMORY.into());
            }
            for (index, part) in parts.iter().enumerate() {
                SafeArrayPutElement(array, &(index as i32), std::ptr::from_ref(part).cast())?;
            }
            Ok(array)
        }
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(self.window, &mut rect)? };
        let (row_height, padding) = {
            let list = self.list.borrow();
            (list.row_height, list.padding)
        };
        let top = rect.top + padding + row_height * self.index as i32;
        Ok(screen_rect(RECT {
            top,
            bottom: top + row_height,
            ..rect
        }))
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.root.clone())
    }
}

impl ISelectionItemProvider_Impl for CandidateItem_Impl {
    fn Select(&self) -> Result<()> {
        Err(selection_is_read_only())
    }

    fn AddToSelection(&self) -> Result<()> {
        Err(selection_is_read_only())
    }

    fn RemoveFromSelection(&self) -> Result<()> {
        Err(selection_is_read_only())
    }

    fn IsSelected(&self) -> Result<BOOL> {
        let list = self.list.borrow();
        Ok((list.selecting && list.selected == self.index).into())
    }

    fn SelectionContainer(&self) -> Result<IRawElementProviderSimple> {
        self.root.cast()
    }
}
