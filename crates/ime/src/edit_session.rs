//! The synchronous edit session that writes the composition into the document.

use crate::{
    composer::CompositionView,
    display_attributes::{Attribute, AttributeAtoms},
    dll::DllRef,
    input_scope,
    text_service::Shared,
};
use std::{mem::ManuallyDrop, rc::Rc};
use windows::{
    Win32::{
        Foundation::*,
        System::Variant::VARIANT,
        UI::{Input::KeyboardAndMouse::GetFocus, TextServices::*},
    },
    core::*,
};

pub(crate) enum Edit {
    Show(CompositionView),
    /// Writes the final text and ends the composition. Empty text cancels it.
    Finish(String),
}

impl Edit {
    fn text(&self) -> &str {
        match self {
            Self::Show(view) => &view.text,
            Self::Finish(text) => text,
        }
    }
}

#[implement(ITfEditSession)]
pub(crate) struct EditSession {
    context: ITfContext,
    sink: ITfCompositionSink,
    atoms: AttributeAtoms,
    shared: Rc<Shared>,
    edit: Edit,
    _dll: DllRef,
}

impl EditSession {
    pub fn new(
        context: ITfContext,
        sink: ITfCompositionSink,
        atoms: AttributeAtoms,
        shared: Rc<Shared>,
        edit: Edit,
    ) -> Self {
        Self {
            context,
            sink,
            atoms,
            shared,
            edit,
            _dll: DllRef::new(),
        }
    }

    /// The active composition, or a new one at the selection. Without a
    /// composition there is nothing to replace, so empty text starts none.
    fn active_or_new_composition(&self, cookie: u32) -> Result<Option<ITfComposition>> {
        let active = self.shared.document.borrow().composition.clone();
        if active.is_some() {
            return Ok(active);
        }
        if self.edit.text().is_empty() {
            self.shared.document.borrow_mut().clear();
            return Ok(None);
        }
        let insert: ITfInsertAtSelection = self.context.cast()?;
        let range = unsafe { insert.InsertTextAtSelection(cookie, TF_IAS_QUERYONLY, &[])? };
        let context: ITfContextComposition = self.context.cast()?;
        let composition = unsafe { context.StartComposition(cookie, &range, &self.sink)? };
        self.shared.document.borrow_mut().composition = Some(composition.clone());
        Ok(Some(composition))
    }

    fn underline(
        &self,
        cookie: u32,
        attributes: &ITfProperty,
        range: &ITfRange,
        view: &CompositionView,
    ) -> Result<()> {
        let attribute = if view.converted {
            Attribute::Converted
        } else {
            Attribute::Input
        };
        let atom = |attribute| VARIANT::from(self.atoms.get(attribute) as i32);
        unsafe {
            attributes.SetValue(cookie, range, &atom(attribute))?;
            if let Some(focus) = &view.focus {
                let length = view.text.encode_utf16().count();
                let focused = range.Clone()?;
                let mut shifted = 0;
                focused.ShiftStart(cookie, focus.start as i32, &mut shifted, std::ptr::null())?;
                focused.ShiftEnd(
                    cookie,
                    focus.end as i32 - length as i32,
                    &mut shifted,
                    std::ptr::null(),
                )?;
                attributes.SetValue(cookie, &focused, &atom(Attribute::Focused))?;
            }
        }
        Ok(())
    }

    /// Places the caret `offset` UTF-16 units into `range`, or at its end.
    fn place_caret(&self, cookie: u32, range: &ITfRange, offset: Option<usize>) -> Result<()> {
        unsafe {
            let caret = range.Clone()?;
            match offset {
                Some(offset) => {
                    let mut shifted = 0;
                    caret.Collapse(cookie, TF_ANCHOR_START)?;
                    caret.ShiftEnd(cookie, offset as i32, &mut shifted, std::ptr::null())?;
                    caret.ShiftStart(cookie, offset as i32, &mut shifted, std::ptr::null())?;
                }
                None => caret.Collapse(cookie, TF_ANCHOR_END)?,
            }
            let mut selection = TF_SELECTION {
                range: ManuallyDrop::new(Some(caret)),
                style: TF_SELECTIONSTYLE {
                    ase: TF_AE_END,
                    fInterimChar: false.into(),
                },
            };
            let result = self
                .context
                .SetSelection(cookie, std::slice::from_ref(&selection));
            ManuallyDrop::drop(&mut selection.range);
            result
        }
    }

    /// Places the candidate window under the composition unless the
    /// application draws the candidates. It is left as it is when the
    /// application cannot report where the text is.
    fn show_candidates(&self, cookie: u32, range: &ITfRange, view: &CompositionView) -> Result<()> {
        unsafe {
            let active = self.context.GetActiveView()?;
            let mut rect = RECT::default();
            let mut clipped = BOOL::default();
            if active
                .GetTextExt(cookie, range, &mut rect, &mut clipped)
                .is_err()
            {
                return Ok(());
            }
            let parent = active.GetWnd().unwrap_or_else(|_| GetFocus());
            let mut document = self.shared.document.borrow_mut();
            match &view.candidates {
                Some(candidates) if !document.application_draws_candidates => {
                    document.candidate_window.show(parent, rect, candidates)
                }
                _ => {
                    document.candidate_window.hide();
                    Ok(())
                }
            }
        }
    }
}

impl ITfEditSession_Impl for EditSession_Impl {
    fn DoEditSession(&self, cookie: u32) -> Result<()> {
        let Some(composition) = self.active_or_new_composition(cookie)? else {
            return Ok(());
        };
        let text = self.edit.text().encode_utf16().collect::<Vec<_>>();
        unsafe {
            let range = composition.GetRange()?;
            let public = input_scope::is_public_field(&self.context, cookie, &range);
            self.shared.document.borrow_mut().public_field = public;
            range.SetText(cookie, 0, &text)?;
            let attributes = self.context.GetProperty(&GUID_PROP_ATTRIBUTE)?;
            attributes.Clear(cookie, &range)?;
            if let Edit::Show(view) = &self.edit {
                self.underline(cookie, &attributes, &range, view)?;
            }
            let caret = match &self.edit {
                Edit::Show(view) => Some(view.caret),
                Edit::Finish(_) => None,
            };
            self.place_caret(cookie, &range, caret)?;
            match &self.edit {
                Edit::Show(view) => self.show_candidates(cookie, &range, view)?,
                Edit::Finish(_) => {
                    composition.EndComposition(cookie)?;
                    self.shared.document.borrow_mut().clear();
                }
            }
        }
        Ok(())
    }
}
