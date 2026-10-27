//! The TSF text input processor. It passes keys to the [`Composer`] and
//! applies the composer's effects to the document through edit sessions.

use crate::{
    candidate_window::CandidateWindow,
    cloud_timer::CloudTimer,
    composer::{Composer, Effect},
    display::{self, Atoms},
    edit_session::{Edit, EditSession},
    keys::{self, Modifiers},
    server::DllRef,
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
    pub public: bool,
    pub window: CandidateWindow,
}

impl Document {
    /// Forgets an ended composition and hides the candidate window.
    pub fn clear(&mut self) {
        self.composition = None;
        self.public = false;
        self.window.hide();
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
    thread: RefCell<Option<ITfThreadMgr>>,
    client: Cell<u32>,
    atoms: Cell<Atoms>,
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
            thread: RefCell::new(None),
            client: Cell::new(0),
            atoms: Cell::new(Atoms::default()),
            cloud_timer: RefCell::new(None),
            _dll: DllRef::new(),
        }
    }

    /// Clears both the composer and the document state.
    fn reset(&self) {
        self.shared.document.borrow_mut().clear();
        self.shared.composer.borrow_mut().reset();
    }
}

impl TextService_Impl {
    /// Whether the composer handles the key in the current state.
    fn wants(&self, vk: u32) -> bool {
        keys::classify(vk, Modifiers::current())
            .is_some_and(|key| self.shared.composer.borrow().accepts(key))
    }

    fn key_down(&self, context: &ITfContext, vk: u32, lparam: LPARAM) -> Result<BOOL> {
        let Some(key) = keys::classify(vk, Modifiers::current()) else {
            return Ok(false.into());
        };
        let handled = self.shared.composer.borrow_mut().press(
            key,
            || keys::typed_text(vk, lparam),
            &mut Worker,
        );
        // A worker failure can follow a commit made earlier in the same key;
        // that commit still has to reach the document.
        self.apply(context)?;
        Ok(handled?.into())
    }

    fn apply(&self, context: &ITfContext) -> Result<()> {
        let effects = self.shared.composer.borrow_mut().take_effects();
        for effect in effects {
            match effect {
                Effect::Show(view) => self.edit(context, Edit::Show(view))?,
                Effect::Commit { text, accepted } => {
                    let learned = accepted.filter(|_| self.shared.document.borrow().public);
                    self.edit(context, Edit::Finish(text))?;
                    if let Some(candidate) = learned {
                        // Stay on the COM apartment thread: a detached thread
                        // could still run this DLL after COM unloaded it.
                        Worker.learn(candidate);
                    }
                }
                Effect::Cancel => self.edit(context, Edit::Finish(String::new()))?,
                Effect::QueueCloud(reading) => {
                    let reading = if self.shared.document.borrow().public {
                        reading
                    } else {
                        String::new()
                    };
                    let enabled = Worker.queue_cloud(reading);
                    self.shared.composer.borrow_mut().set_cloud_enabled(enabled);
                }
            }
        }
        Ok(())
    }

    /// Finishes a composition left in another document before handling a
    /// key for `context`. Nothing is learned from it.
    fn finish_elsewhere(&self, context: &ITfContext) -> Result<()> {
        let composition = self.shared.document.borrow().composition.clone();
        let Some(composition) = composition else {
            return Ok(());
        };
        let previous = unsafe { composition.GetRange()?.GetContext()? };
        if previous != *context {
            let text = self.shared.composer.borrow().text();
            self.edit(&previous, Edit::Finish(text))?;
            self.shared.composer.borrow_mut().reset();
        }
        Ok(())
    }

    fn edit(&self, context: &ITfContext, edit: Edit) -> Result<()> {
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
                .RequestEditSession(self.client.get(), &session, TF_ES_SYNC | TF_ES_READWRITE)?
                .ok()
        }
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, thread: Ref<ITfThreadMgr>, client: u32) -> Result<()> {
        let thread = thread.ok()?;
        let keystrokes: ITfKeystrokeMgr = thread.cast()?;
        let sink: ITfKeyEventSink = self.to_interface();
        self.atoms.set(Atoms::register()?);
        unsafe {
            keystrokes.AdviseKeyEventSink(client, &sink, true)?;
        }
        self.client.set(client);
        *self.thread.borrow_mut() = Some(thread.clone());
        *self.cloud_timer.borrow_mut() = CloudTimer::start(&self.shared);
        Ok(())
    }

    fn Deactivate(&self) -> Result<()> {
        self.cloud_timer.borrow_mut().take();
        let thread = self.thread.borrow_mut().take();
        if let Some(thread) = thread {
            let keystrokes: ITfKeystrokeMgr = thread.cast()?;
            unsafe {
                keystrokes.UnadviseKeyEventSink(self.client.get())?;
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
        Ok(display::enumerator(0))
    }
    fn GetDisplayAttributeInfo(&self, guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        display::get(guid)
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, foreground: BOOL) -> Result<()> {
        if !foreground.as_bool() {
            {
                let mut document = self.shared.document.borrow_mut();
                document.window.hide();
                document.public = false;
            }
            self.shared.composer.borrow_mut().discard_cloud_words();
        }
        Ok(())
    }
    fn OnTestKeyDown(&self, context: Ref<ITfContext>, key: WPARAM, _flags: LPARAM) -> Result<BOOL> {
        let editable = context.as_ref().is_some_and(|context| unsafe {
            context
                .GetStatus()
                .is_ok_and(|status| status.dwStaticFlags & TF_SD_READONLY == 0)
        });
        Ok((editable && self.wants(key.0 as u32)).into())
    }
    fn OnTestKeyUp(&self, _context: Ref<ITfContext>, _key: WPARAM, _flags: LPARAM) -> Result<BOOL> {
        Ok(false.into())
    }
    fn OnKeyDown(&self, context: Ref<ITfContext>, key: WPARAM, flags: LPARAM) -> Result<BOOL> {
        let context = context.ok()?;
        self.finish_elsewhere(context)?;
        self.key_down(context, key.0 as u32, flags)
    }
    fn OnKeyUp(&self, _context: Ref<ITfContext>, _key: WPARAM, _flags: LPARAM) -> Result<BOOL> {
        Ok(false.into())
    }
    fn OnPreservedKey(&self, _context: Ref<ITfContext>, _key: *const GUID) -> Result<BOOL> {
        Ok(false.into())
    }
}
