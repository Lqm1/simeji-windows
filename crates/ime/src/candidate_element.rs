//! The candidate list as a TSF UI element. Applications that draw IME
//! candidates themselves, such as full-screen games and the search box,
//! read the list here, can hide the candidate window, and choose a
//! candidate through the element.

use crate::{
    candidates::{CandidateView, PAGE_SIZE},
    composer::CandidateAction,
    dll::DllRef,
};
use std::cell::{Cell, RefCell};
use windows::{
    Win32::{Foundation::*, UI::TextServices::*},
    core::*,
};

const GUID_CANDIDATE_ELEMENT: GUID = GUID::from_u128(0x7e13fe14_4d83_44db_b7f2_ebd9cc1aa19d);
const DESCRIPTION: &str = "Candidate list";
const ALL_CHANGES: u32 = TF_CLUIE_DOCUMENTMGR
    | TF_CLUIE_COUNT
    | TF_CLUIE_SELECTION
    | TF_CLUIE_STRING
    | TF_CLUIE_PAGEINDEX
    | TF_CLUIE_CURRENTPAGE;

/// What the element asks of the text service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Act(CandidateAction),
    ShowCandidateWindow(bool),
}

pub(crate) type Handler = Box<dyn Fn(Request) -> Result<()>>;

#[implement(
    ITfCandidateListUIElementBehavior,
    ITfCandidateListUIElement,
    ITfUIElement,
    ITfIntegratableCandidateListUIElement
)]
pub(crate) struct CandidateElement {
    document_manager: Option<ITfDocumentMgr>,
    view: RefCell<CandidateView>,
    /// `TF_CLUIE_*` flags for what changed since the application last asked.
    changes: Cell<u32>,
    shown: Cell<bool>,
    handler: Handler,
    _dll: DllRef,
}

impl CandidateElement {
    pub fn new(
        document_manager: Option<ITfDocumentMgr>,
        view: CandidateView,
        handler: Handler,
    ) -> Self {
        Self {
            document_manager,
            view: RefCell::new(view),
            changes: Cell::new(ALL_CHANGES),
            shown: Cell::new(true),
            handler,
            _dll: DllRef::new(),
        }
    }

    /// Replaces the candidates and returns whether anything changed.
    pub fn update(&self, view: &CandidateView) -> bool {
        let mut current = self.view.borrow_mut();
        let mut changes = 0;
        if current.texts.len() != view.texts.len() {
            changes |= TF_CLUIE_COUNT | TF_CLUIE_PAGEINDEX;
        }
        if current.texts != view.texts {
            changes |= TF_CLUIE_STRING;
        }
        if current.selected != view.selected {
            changes |= TF_CLUIE_SELECTION;
        }
        if current.page_start() != view.page_start() {
            changes |= TF_CLUIE_CURRENTPAGE;
        }
        current.clone_from(view);
        self.changes.set(self.changes.get() | changes);
        changes != 0
    }

    pub fn is_shown(&self) -> bool {
        self.shown.get()
    }

    pub fn set_shown(&self, shown: bool) {
        self.shown.set(shown);
    }
}

impl ITfUIElement_Impl for CandidateElement_Impl {
    fn GetDescription(&self) -> Result<BSTR> {
        Ok(DESCRIPTION.into())
    }

    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID_CANDIDATE_ELEMENT)
    }

    fn Show(&self, show: BOOL) -> Result<()> {
        self.shown.set(show.as_bool());
        (self.handler)(Request::ShowCandidateWindow(show.as_bool()))
    }

    fn IsShown(&self) -> Result<BOOL> {
        Ok(self.shown.get().into())
    }
}

impl ITfCandidateListUIElement_Impl for CandidateElement_Impl {
    fn GetUpdatedFlags(&self) -> Result<u32> {
        Ok(self.changes.replace(0))
    }

    fn GetDocumentMgr(&self) -> Result<ITfDocumentMgr> {
        self.document_manager.clone().ok_or_else(|| E_FAIL.into())
    }

    fn GetCount(&self) -> Result<u32> {
        Ok(self.view.borrow().texts.len() as u32)
    }

    fn GetSelection(&self) -> Result<u32> {
        Ok(self.view.borrow().selected as u32)
    }

    fn GetString(&self, index: u32) -> Result<BSTR> {
        self.view
            .borrow()
            .texts
            .get(index as usize)
            .map(|text| BSTR::from(text.as_str()))
            .ok_or_else(|| E_INVALIDARG.into())
    }

    /// Writes the first candidate index of each page, as many as `size`
    /// allows, and the page count.
    fn GetPageIndex(&self, starts: *mut u32, size: u32, page_count: *mut u32) -> Result<()> {
        if page_count.is_null() {
            return Err(E_INVALIDARG.into());
        }
        let count = self.view.borrow().page_count();
        unsafe {
            *page_count = count as u32;
            if !starts.is_null() {
                for page in 0..count.min(size as usize) {
                    *starts.add(page) = (page * PAGE_SIZE) as u32;
                }
            }
        }
        Ok(())
    }

    /// Pages always hold [`PAGE_SIZE`] candidates, as the number keys expect.
    fn SetPageIndex(&self, _starts: *const u32, _page_count: u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn GetCurrentPage(&self) -> Result<u32> {
        Ok((self.view.borrow().selected / PAGE_SIZE) as u32)
    }
}

impl ITfCandidateListUIElementBehavior_Impl for CandidateElement_Impl {
    fn SetSelection(&self, index: u32) -> Result<()> {
        (self.handler)(Request::Act(CandidateAction::Select(index as usize)))
    }

    fn Finalize(&self) -> Result<()> {
        (self.handler)(Request::Act(CandidateAction::Commit))
    }

    fn Abort(&self) -> Result<()> {
        (self.handler)(Request::Act(CandidateAction::Cancel))
    }
}

impl ITfIntegratableCandidateListUIElement_Impl for CandidateElement_Impl {
    fn SetIntegrationStyle(&self, style: &GUID) -> Result<()> {
        if *style == GUID_INTEGRATIONSTYLE_SEARCHBOX {
            Ok(())
        } else {
            Err(E_NOTIMPL.into())
        }
    }

    fn GetSelectionStyle(&self) -> Result<TfIntegratableCandidateListSelectionStyle> {
        Ok(STYLE_ACTIVE_SELECTION)
    }

    /// The key event sink already took every key this service handles, so
    /// the keys that reach the application here are left to it.
    fn OnKeyDown(&self, _key: WPARAM, _flags: LPARAM) -> Result<BOOL> {
        Ok(false.into())
    }

    fn ShowCandidateNumbers(&self) -> Result<BOOL> {
        Ok(true.into())
    }

    fn FinalizeExactCompositionString(&self) -> Result<()> {
        (self.handler)(Request::Act(CandidateAction::Commit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn view(count: usize, selected: usize) -> CandidateView {
        CandidateView {
            texts: (0..count).map(|n| n.to_string()).collect(),
            selected,
        }
    }

    fn element() -> (ComObject<CandidateElement>, Rc<RefCell<Vec<Request>>>) {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let handler = {
            let requests = requests.clone();
            Box::new(move |request| {
                requests.borrow_mut().push(request);
                Ok(())
            })
        };
        let element = CandidateElement::new(None, view(12, 10), handler);
        (ComObject::new(element), requests)
    }

    #[test]
    fn describes_the_candidates_and_their_pages() -> Result<()> {
        let (element, _) = element();
        let list: ITfCandidateListUIElement = element.to_interface();
        unsafe {
            assert_eq!(list.GetUpdatedFlags()?, ALL_CHANGES);
            assert_eq!(list.GetUpdatedFlags()?, 0);
            assert_eq!(list.GetCount()?, 12);
            assert_eq!(list.GetSelection()?, 10);
            assert_eq!(list.GetString(11)?, "11");
            assert!(list.GetString(12).is_err());
            assert_eq!(list.GetCurrentPage()?, 1);
            let mut page_count = 0;
            list.GetPageIndex(&mut [], &mut page_count)?;
            assert_eq!(page_count, 2);
            let mut starts = [u32::MAX; 3];
            list.GetPageIndex(&mut starts, &mut page_count)?;
            assert_eq!(starts, [0, 9, u32::MAX]);
        }
        Ok(())
    }

    #[test]
    fn reports_what_changed() -> Result<()> {
        let (element, _) = element();
        let list: ITfCandidateListUIElement = element.to_interface();
        unsafe {
            list.GetUpdatedFlags()?;
            assert!(!element.update(&view(12, 10)));
            assert!(element.update(&view(12, 2)));
            assert_eq!(
                list.GetUpdatedFlags()?,
                TF_CLUIE_SELECTION | TF_CLUIE_CURRENTPAGE
            );
            assert!(element.update(&view(3, 2)));
            assert_eq!(
                list.GetUpdatedFlags()?,
                TF_CLUIE_COUNT | TF_CLUIE_PAGEINDEX | TF_CLUIE_STRING
            );
        }
        Ok(())
    }

    #[test]
    fn passes_the_application_choices_to_the_text_service() -> Result<()> {
        let (element, requests) = element();
        let behavior: ITfCandidateListUIElementBehavior = element.to_interface();
        let integratable: ITfIntegratableCandidateListUIElement = element.to_interface();
        unsafe {
            behavior.SetSelection(3)?;
            behavior.Finalize()?;
            behavior.Abort()?;
            behavior.Show(false)?;
            assert!(!behavior.IsShown()?.as_bool());
            integratable.FinalizeExactCompositionString()?;
            integratable.SetIntegrationStyle(GUID_INTEGRATIONSTYLE_SEARCHBOX)?;
            assert!(integratable.SetIntegrationStyle(GUID::zeroed()).is_err());
            assert!(!integratable.OnKeyDown(WPARAM(0x28), LPARAM(0))?.as_bool());
            assert!(integratable.ShowCandidateNumbers()?.as_bool());
        }
        assert_eq!(
            *requests.borrow(),
            [
                Request::Act(CandidateAction::Select(3)),
                Request::Act(CandidateAction::Commit),
                Request::Act(CandidateAction::Cancel),
                Request::ShowCandidateWindow(false),
                Request::Act(CandidateAction::Commit),
            ]
        );
        Ok(())
    }
}
