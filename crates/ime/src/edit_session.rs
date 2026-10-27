//! The synchronous edit session that writes the composition into the document.

use crate::{
    composer::View,
    display::{Atoms, Attribute},
    input_scope,
    server::DllRef,
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
    /// Show the composition with its underlines and candidate window.
    Show(View),
    /// Write the final text and end the composition. Empty text cancels it.
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
    atoms: Atoms,
    shared: Rc<Shared>,
    edit: Edit,
    _dll: DllRef,
}

impl EditSession {
    pub fn new(
        context: ITfContext,
        sink: ITfCompositionSink,
        atoms: Atoms,
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
    fn composition(&self, cookie: u32) -> Result<Option<ITfComposition>> {
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

    /// Underlines the composition, with a bold line under the focused clause.
    fn underline(
        &self,
        cookie: u32,
        attributes: &ITfProperty,
        range: &ITfRange,
        view: &View,
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

    fn place_caret_at_end(&self, cookie: u32, range: &ITfRange) -> Result<()> {
        unsafe {
            let caret = range.Clone()?;
            caret.Collapse(cookie, TF_ANCHOR_END)?;
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

    /// Places the candidate window under the composition. It is left as it
    /// is when the application cannot report where the text is.
    fn show_candidates(&self, cookie: u32, range: &ITfRange, view: &View) -> Result<()> {
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
                Some(page) => document.window.show(parent, rect, page),
                None => {
                    document.window.hide();
                    Ok(())
                }
            }
        }
    }
}

impl ITfEditSession_Impl for EditSession_Impl {
    fn DoEditSession(&self, cookie: u32) -> Result<()> {
        let Some(composition) = self.composition(cookie)? else {
            return Ok(());
        };
        let text = self.edit.text().encode_utf16().collect::<Vec<_>>();
        unsafe {
            let range = composition.GetRange()?;
            let public = input_scope::is_public(&self.context, cookie, &range);
            self.shared.document.borrow_mut().public = public;
            range.SetText(cookie, 0, &text)?;
            let attributes = self.context.GetProperty(&GUID_PROP_ATTRIBUTE)?;
            attributes.Clear(cookie, &range)?;
            if let Edit::Show(view) = &self.edit {
                self.underline(cookie, &attributes, &range, view)?;
            }
            self.place_caret_at_end(cookie, &range)?;
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
