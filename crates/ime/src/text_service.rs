//! The TSF text input processor. It passes keys to the [`Composer`] and
//! applies the composer's effects to the document through edit sessions.

use crate::{
    candidate_window::CandidateWindow,
    cloud_timer::CloudTimer,
    composer::{Composer, Effect, Learning},
    display_attributes::{self, AttributeAtoms},
    dll::DllRef,
    edit_session::{Edit, EditSession},
    keys::{self, Modifiers},
    worker::Worker,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use windows::{
    Win32::{Foundation::*, UI::TextServices::*},
    core::*,
};

/// State shared by the text service, its edit sessions and the cloud timer.
///
/// TSF calls back into the service while it applies edits, so neither cell
/// is borrowed across a call into TSF or the worker.
pub(crate) struct Shared {
    pub composer: RefCell<Composer>,
    pub document: RefCell<Document>,
}

/// The composition as it exists in the focused document.
#[derive(Default)]
pub(crate) struct Document {
    pub composition: Option<ITfComposition>,
    /// Whether the field's input scope allows cloud lookups and learning,
    /// as last read by an edit session.
    pub public_field: bool,
    pub candidate_window: CandidateWindow,
}

impl Document {
    pub fn clear(&mut self) {
        self.composition = None;
        self.public_field = false;
        self.candidate_window.hide();
    }
}

#[implement(
    ITfTextInputProcessor,
    ITfKeyEventSink,
    ITfCompositionSink,
    ITfDisplayAttributeProvider
)]
pub(crate) struct TextService {
    shared: Rc<Shared>,
    thread_manager: RefCell<Option<ITfThreadMgr>>,
    client_id: Cell<u32>,
    atoms: Cell<AttributeAtoms>,
    cloud_timer: RefCell<Option<CloudTimer>>,
    _dll: DllRef,
}

impl TextService {
    pub fn new() -> Self {
        Self {
            shared: Rc::new(Shared {
                composer: RefCell::new(Composer::new()),
                document: RefCell::new(Document::default()),
            }),
            thread_manager: RefCell::new(None),
            client_id: Cell::new(0),
            atoms: Cell::new(AttributeAtoms::default()),
            cloud_timer: RefCell::new(None),
            _dll: DllRef::new(),
        }
    }

    fn reset(&self) {
        self.shared.document.borrow_mut().clear();
        self.shared.composer.borrow_mut().reset();
    }
}

impl TextService_Impl {
    fn wants_key(&self, vk: u32) -> bool {
        keys::classify(vk, Modifiers::current())
            .is_some_and(|key| self.shared.composer.borrow().accepts(key))
    }

    fn handle_key(&self, context: &ITfContext, vk: u32, lparam: LPARAM) -> Result<BOOL> {
        let Some(key) = keys::classify(vk, Modifiers::current()) else {
            return Ok(false.into());
        };
        let before = self.shared.composer.borrow().clone();
        let handled = self.shared.composer.borrow_mut().press(
            key,
            || keys::typed_text(vk, lparam),
            &mut Worker,
        );
        // Effects queued before a worker failure, such as a commit, still
        // have to reach the document.
        let (applied, result) = self.apply_effects(context);
        if applied == 0 && (handled.is_err() || result.is_err()) {
            // The document did not change, so neither does the composition.
            *self.shared.composer.borrow_mut() = before;
        }
        result?;
        Ok(handled?.into())
    }

    /// Applies the queued effects in order and returns how many succeeded.
    fn apply_effects(&self, context: &ITfContext) -> (usize, Result<()>) {
        let effects = self.shared.composer.borrow_mut().take_effects();
        let count = effects.len();
        for (applied, effect) in effects.into_iter().enumerate() {
            if let Err(error) = self.apply_effect(context, effect) {
                return (applied, Err(error));
            }
        }
        (count, Ok(()))
    }

    fn apply_effect(&self, context: &ITfContext, effect: Effect) -> Result<()> {
        match effect {
            Effect::Show(view) => self.request_edit(context, Edit::Show(view)),
            Effect::Commit { text, learn } => {
                let public = self.shared.document.borrow().public_field;
                self.request_edit(context, Edit::Finish(text))?;
                if public {
                    // Stay on the COM apartment thread: a detached thread
                    // could still run this DLL after COM unloaded it.
                    for Learning {
                        candidate,
                        previous,
                    } in learn
                    {
                        Worker.learn(candidate, previous);
                    }
                }
                Ok(())
            }
            Effect::Cancel => self.request_edit(context, Edit::Finish(String::new())),
            Effect::QueueCloud(reading) => {
                let reading = if self.shared.document.borrow().public_field {
                    reading
                } else {
                    String::new()
                };
                let enabled = Worker.queue_cloud(reading);
                self.shared.composer.borrow_mut().set_cloud_enabled(enabled);
                Ok(())
            }
        }
    }

    /// Finishes a composition left in another document before handling a
    /// key for `context`. Nothing is learned from it.
    fn finish_in_previous_context(&self, context: &ITfContext) -> Result<()> {
        let composition = self.shared.document.borrow().composition.clone();
        let Some(composition) = composition else {
            return Ok(());
        };
        let previous = unsafe { composition.GetRange()?.GetContext()? };
        if previous != *context {
            let text = self.shared.composer.borrow().text();
            self.request_edit(&previous, Edit::Finish(text))?;
            self.shared.composer.borrow_mut().reset();
        }
        Ok(())
    }

    fn request_edit(&self, context: &ITfContext, edit: Edit) -> Result<()> {
        let session: ITfEditSession = EditSession::new(
            context.clone(),
            self.to_interface(),
            self.atoms.get(),
            self.shared.clone(),
            edit,
        )
        .into();
        unsafe {
            context
                .RequestEditSession(self.client_id.get(), &session, TF_ES_SYNC | TF_ES_READWRITE)?
                .ok()
        }
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, thread: Ref<ITfThreadMgr>, client: u32) -> Result<()> {
        let thread = thread.ok()?;
        let keystrokes: ITfKeystrokeMgr = thread.cast()?;
        let sink: ITfKeyEventSink = self.to_interface();
        self.atoms.set(AttributeAtoms::register()?);
        unsafe {
            keystrokes.AdviseKeyEventSink(client, &sink, true)?;
        }
        self.client_id.set(client);
        *self.thread_manager.borrow_mut() = Some(thread.clone());
        *self.cloud_timer.borrow_mut() = CloudTimer::start(&self.shared);
        Ok(())
    }

    fn Deactivate(&self) -> Result<()> {
        self.cloud_timer.borrow_mut().take();
        let thread = self.thread_manager.borrow_mut().take();
        if let Some(thread) = thread {
            let keystrokes: ITfKeystrokeMgr = thread.cast()?;
            unsafe {
                keystrokes.UnadviseKeyEventSink(self.client_id.get())?;
            }
        }
        self.reset();
        Ok(())
    }
}

impl ITfCompositionSink_Impl for TextService_Impl {
    /// The application ended the composition, keeping whatever text it shows.
    fn OnCompositionTerminated(
        &self,
        _cookie: u32,
        _composition: Ref<ITfComposition>,
    ) -> Result<()> {
        self.reset();
        Ok(())
    }
}

impl ITfDisplayAttributeProvider_Impl for TextService_Impl {
    fn EnumDisplayAttributeInfo(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(display_attributes::enumerate_from(0))
    }
    fn GetDisplayAttributeInfo(&self, guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        display_attributes::find(guid)
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, foreground: BOOL) -> Result<()> {
        if !foreground.as_bool() {
            {
                let mut document = self.shared.document.borrow_mut();
                document.candidate_window.hide();
                document.public_field = false;
            }
            self.shared.composer.borrow_mut().discard_cloud_candidates();
        }
        Ok(())
    }
    fn OnTestKeyDown(&self, context: Ref<ITfContext>, key: WPARAM, _flags: LPARAM) -> Result<BOOL> {
        let editable = context.as_ref().is_some_and(|context| unsafe {
            context
                .GetStatus()
                .is_ok_and(|status| status.dwStaticFlags & TF_SD_READONLY == 0)
        });
        Ok((editable && self.wants_key(key.0 as u32)).into())
    }
    fn OnTestKeyUp(&self, _context: Ref<ITfContext>, _key: WPARAM, _flags: LPARAM) -> Result<BOOL> {
        Ok(false.into())
    }
    fn OnKeyDown(&self, context: Ref<ITfContext>, key: WPARAM, flags: LPARAM) -> Result<BOOL> {
        let context = context.ok()?;
        self.finish_in_previous_context(context)?;
        self.handle_key(context, key.0 as u32, flags)
    }
    fn OnKeyUp(&self, _context: Ref<ITfContext>, _key: WPARAM, _flags: LPARAM) -> Result<BOOL> {
        Ok(false.into())
    }
    fn OnPreservedKey(&self, _context: Ref<ITfContext>, _key: *const GUID) -> Result<BOOL> {
        Ok(false.into())
    }
}
