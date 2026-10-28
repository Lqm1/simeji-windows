//! The TSF text input processor. It passes keys to the [`Composer`] and
//! applies the composer's effects to the document through edit sessions.

use crate::{
    candidate_element::{CandidateElement, Handler, Request},
    candidate_window::CandidateWindow,
    candidates::CandidateView,
    composer::{Composer, Effect, InputMode, Learning},
    display_attributes::{self, AttributeAtoms},
    dll::DllRef,
    edit_session::{Edit, EditSession},
    functions::Functions,
    input_mode::{self, InputModeButton},
    keys::{self, Key, Modifiers},
    lookup_timer::LookupTimer,
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

/// State shared by the text service, its edit sessions and the lookup timer.
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
    pub application_draws_candidates: bool,
}

impl Document {
    pub fn clear(&mut self) {
        self.composition = None;
        self.public_field = false;
        self.candidate_window.hide();
        self.candidate_window.clear_anchor();
    }

    /// Shows the candidates in the window unless the application draws them.
    pub fn show_candidates(&mut self, view: Option<&CandidateView>) -> Result<()> {
        match view {
            Some(view) if !self.application_draws_candidates => self.candidate_window.show(view),
            _ => {
                self.candidate_window.hide();
                Ok(())
            }
        }
    }
}

/// The input mode compartment this service follows and the indicator
/// button that shows it.
struct InputModeState {
    open_close: ITfCompartment,
    sink_cookie: u32,
    button: ComObject<InputModeButton>,
    /// Whether the input indicator accepted the button.
    shown: bool,
}

struct CandidateElementState {
    element: ComObject<CandidateElement>,
    id: u32,
}

#[implement(
    ITfTextInputProcessorEx,
    ITfKeyEventSink,
    ITfCompositionSink,
    ITfDisplayAttributeProvider,
    ITfCompartmentEventSink
)]
pub(crate) struct TextService {
    shared: Rc<Shared>,
    thread_manager: RefCell<Option<ITfThreadMgr>>,
    client_id: Cell<u32>,
    atoms: Cell<AttributeAtoms>,
    lookup_timer: RefCell<Option<LookupTimer>>,
    input_mode: RefCell<Option<InputModeState>>,
    ui_elements: RefCell<Option<ITfUIElementMgr>>,
    candidate_element: RefCell<Option<CandidateElementState>>,
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
            lookup_timer: RefCell::new(None),
            input_mode: RefCell::new(None),
            ui_elements: RefCell::new(None),
            candidate_element: RefCell::new(None),
            _dll: DllRef::new(),
        }
    }

    fn reset(&self) {
        self.end_candidate_element();
        self.shared.document.borrow_mut().clear();
        self.shared.composer.borrow_mut().reset();
    }

    fn end_candidate_element(&self) {
        let state = self.candidate_element.borrow_mut().take();
        let manager = self.ui_elements.borrow().clone();
        if let (Some(state), Some(manager)) = (state, manager) {
            unsafe {
                let _ = manager.EndUIElement(state.id);
            }
        }
        self.shared
            .document
            .borrow_mut()
            .application_draws_candidates = false;
    }
}

impl TextService_Impl {
    fn wants_key(&self, vk: u32, lparam: LPARAM) -> bool {
        keys::read(vk, lparam, Modifiers::current())
            .is_some_and(|key| self.shared.composer.borrow().accepts(key))
    }

    fn handle_key(&self, context: &ITfContext, vk: u32, lparam: LPARAM) -> Result<BOOL> {
        let Some(key) = keys::read(vk, lparam, Modifiers::current()) else {
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
        if key == Key::ToggleInputMode {
            self.publish_mode()?;
        }
        result?;
        Ok(handled?.into())
    }

    /// Writes the composer's mode to the compartment, which updates the
    /// indicator through [`ITfCompartmentEventSink_Impl::OnChange`].
    fn publish_mode(&self) -> Result<()> {
        let open_close = self
            .input_mode
            .borrow()
            .as_ref()
            .map(|state| state.open_close.clone());
        match open_close {
            Some(open_close) => {
                let mode = self.shared.composer.borrow().mode();
                input_mode::write(&open_close, self.client_id.get(), mode)
            }
            None => Ok(()),
        }
    }

    /// Follows the compartment, adds the mode button to the input indicator
    /// and switches Japanese input on, as a newly activated IME should be.
    fn activate_input_mode(&self, thread: &ITfThreadMgr, client_id: u32) -> Result<()> {
        let open_close = input_mode::open_close(thread)?;
        let sink: ITfCompartmentEventSink = self.to_interface();
        let sink_cookie = unsafe {
            open_close
                .cast::<ITfSource>()?
                .AdviseSink(&ITfCompartmentEventSink::IID, &sink)?
        };
        let button = ComObject::new(InputModeButton::new(
            open_close.clone(),
            client_id,
            InputMode::Japanese,
        ));
        // Without the indicator button the IME still works, so a failure to
        // add it does not fail the activation.
        let shown = thread
            .cast::<ITfLangBarItemMgr>()
            .and_then(|items| unsafe {
                items.AddItem(&button.to_interface::<ITfLangBarItemButton>())
            })
            .is_ok();
        *self.input_mode.borrow_mut() = Some(InputModeState {
            open_close: open_close.clone(),
            sink_cookie,
            button,
            shown,
        });
        let _ = input_mode::write_conversion_mode(thread, client_id);
        input_mode::write(&open_close, client_id, InputMode::Japanese)
    }

    fn deactivate_input_mode(&self, thread: &ITfThreadMgr) {
        let Some(state) = self.input_mode.borrow_mut().take() else {
            return;
        };
        unsafe {
            if let Ok(source) = state.open_close.cast::<ITfSource>() {
                let _ = source.UnadviseSink(state.sink_cookie);
            }
            if let Ok(items) = thread.cast::<ITfLangBarItemMgr>()
                && state.shown
            {
                let _ = items.RemoveItem(&state.button.to_interface::<ITfLangBarItemButton>());
            }
        }
    }

    /// The IME works without the function provider, so a failure does not
    /// fail the activation.
    fn advise_functions(thread: &ITfThreadMgr, client_id: u32) {
        let functions: ITfFunctionProvider = Functions::new().into();
        if let Ok(source) = thread.cast::<ITfSourceSingle>() {
            unsafe {
                let _ = source.AdviseSingleSink(client_id, &ITfFunctionProvider::IID, &functions);
            }
        }
    }

    fn unadvise_functions(thread: &ITfThreadMgr, client_id: u32) {
        if let Ok(source) = thread.cast::<ITfSourceSingle>() {
            unsafe {
                let _ = source.UnadviseSingleSink(client_id, &ITfFunctionProvider::IID);
            }
        }
    }

    fn present_candidates(&self, context: &ITfContext, view: Option<&CandidateView>) {
        let Some(view) = view else {
            self.end_candidate_element();
            return;
        };
        let Some(manager) = self.ui_elements.borrow().clone() else {
            return;
        };
        let active = self
            .candidate_element
            .borrow()
            .as_ref()
            .map(|state| (state.element.clone(), state.id));
        if let Some((element, id)) = active {
            if element.update(view) {
                unsafe {
                    let _ = manager.UpdateUIElement(id);
                }
            }
            return;
        }
        let document_manager = unsafe { context.GetDocumentMgr() }.ok();
        let element = ComObject::new(CandidateElement::new(
            document_manager,
            view.clone(),
            self.element_handler(),
        ));
        let mut show = BOOL::from(true);
        let mut id = 0;
        let begun = unsafe {
            manager.BeginUIElement(&element.to_interface::<ITfUIElement>(), &mut show, &mut id)
        };
        if begun.is_err() {
            return;
        }
        let shown = show.as_bool() && element.is_shown();
        element.set_shown(shown);
        self.shared
            .document
            .borrow_mut()
            .application_draws_candidates = !shown;
        *self.candidate_element.borrow_mut() = Some(CandidateElementState { element, id });
    }

    /// The service owns the element, so the element holds it weakly.
    fn element_handler(&self) -> Handler {
        let service = self.to_interface::<ITfKeyEventSink>().downgrade().ok();
        Box::new(move |request| {
            let Some(service) = service.as_ref().and_then(Weak::upgrade) else {
                return Ok(());
            };
            ComObject::<TextService>::cast_from(&service)?.handle_element_request(request)
        })
    }

    fn handle_element_request(&self, request: Request) -> Result<()> {
        let context = self.composition_context();
        match request {
            Request::Act(action) => {
                let Some(context) = context else {
                    return Ok(());
                };
                let before = self.shared.composer.borrow().clone();
                self.shared.composer.borrow_mut().act(action);
                let (applied, result) = self.apply_effects(&context);
                if applied == 0 && result.is_err() {
                    *self.shared.composer.borrow_mut() = before;
                }
                result
            }
            Request::ShowCandidateWindow(show) => {
                {
                    let mut document = self.shared.document.borrow_mut();
                    document.application_draws_candidates = !show;
                    if !show {
                        document.candidate_window.hide();
                    }
                }
                match context {
                    Some(context) if show => {
                        let view = self.shared.composer.borrow().view();
                        self.request_edit(&context, Edit::Show(view))
                    }
                    _ => Ok(()),
                }
            }
        }
    }

    fn composition_context(&self) -> Option<ITfContext> {
        let composition = self.shared.document.borrow().composition.clone()?;
        unsafe { composition.GetRange().and_then(|range| range.GetContext()) }.ok()
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
            Effect::Show(view) => {
                self.present_candidates(context, view.candidates.as_ref());
                self.request_edit(context, Edit::Show(view))
            }
            // The composition is unchanged, so no edit session is needed.
            Effect::Candidates(view) => {
                self.present_candidates(context, view.as_ref());
                self.shared
                    .document
                    .borrow_mut()
                    .show_candidates(view.as_ref())
            }
            Effect::Commit { text, learn } => {
                self.end_candidate_element();
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
            Effect::Cancel => {
                self.end_candidate_element();
                self.request_edit(context, Edit::Finish(String::new()))
            }
            Effect::Lookup(reading) => {
                let public = self.shared.document.borrow().public_field;
                let enabled = Worker.start_lookup(reading, public);
                self.shared
                    .composer
                    .borrow_mut()
                    .set_cloud_enabled(enabled && public);
                if let Some(timer) = self.lookup_timer.borrow().as_ref() {
                    timer.start();
                }
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
            self.end_candidate_element();
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
        self.ActivateEx(thread, client, 0)
    }

    fn Deactivate(&self) -> Result<()> {
        self.lookup_timer.borrow_mut().take();
        let thread = self.thread_manager.borrow_mut().take();
        self.reset();
        self.ui_elements.borrow_mut().take();
        if let Some(thread) = thread {
            self.deactivate_input_mode(&thread);
            Self::unadvise_functions(&thread, self.client_id.get());
            let keystrokes: ITfKeystrokeMgr = thread.cast()?;
            unsafe {
                keystrokes.UnadviseKeyEventSink(self.client_id.get())?;
            }
        }
        Ok(())
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    fn ActivateEx(&self, thread: Ref<ITfThreadMgr>, client: u32, _flags: u32) -> Result<()> {
        let thread = thread.ok()?;
        let keystrokes: ITfKeystrokeMgr = thread.cast()?;
        let sink: ITfKeyEventSink = self.to_interface();
        self.atoms.set(AttributeAtoms::register()?);
        unsafe {
            keystrokes.AdviseKeyEventSink(client, &sink, true)?;
        }
        self.client_id.set(client);
        *self.thread_manager.borrow_mut() = Some(thread.clone());
        *self.ui_elements.borrow_mut() = thread.cast().ok();
        Self::advise_functions(thread, client);
        let handler = self.element_handler();
        self.shared
            .document
            .borrow_mut()
            .candidate_window
            .set_action(Rc::new(move |action| {
                let _ = handler(Request::Act(action));
            }));
        let service = self.to_interface::<ITfKeyEventSink>().downgrade().ok();
        *self.lookup_timer.borrow_mut() = Some(LookupTimer::new(
            &self.shared,
            Box::new(move || {
                if let Some(service) = service.as_ref().and_then(Weak::upgrade)
                    && let Ok(service) = ComObject::<TextService>::cast_from(&service)
                    && let Some(context) = service.composition_context()
                {
                    let _ = service.apply_effects(&context);
                }
            }),
        ));
        self.activate_input_mode(thread, client)
    }
}

impl ITfCompartmentEventSink_Impl for TextService_Impl {
    /// Follows input mode changes from the application, the input indicator
    /// or this service; leaving Japanese input commits the composition.
    fn OnChange(&self, guid: *const GUID) -> Result<()> {
        if guid.is_null() || unsafe { *guid } != GUID_COMPARTMENT_KEYBOARD_OPENCLOSE {
            return Ok(());
        }
        let state = self
            .input_mode
            .borrow()
            .as_ref()
            .map(|state| (state.open_close.clone(), state.button.clone()));
        let Some((open_close, button)) = state else {
            return Ok(());
        };
        let mode = input_mode::read(&open_close);
        self.shared.composer.borrow_mut().set_mode(mode);
        let result = match self.composition_context() {
            Some(context) => self.apply_effects(&context).1,
            None => {
                self.shared.composer.borrow_mut().take_effects();
                Ok(())
            }
        };
        button.update(mode);
        result
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
            self.end_candidate_element();
            self.shared.composer.borrow_mut().forget_lookups();
        }
        Ok(())
    }
    fn OnTestKeyDown(&self, context: Ref<ITfContext>, key: WPARAM, flags: LPARAM) -> Result<BOOL> {
        let editable = context.as_ref().is_some_and(|context| unsafe {
            context
                .GetStatus()
                .is_ok_and(|status| status.dwStaticFlags & TF_SD_READONLY == 0)
        });
        Ok((editable && self.wants_key(key.0 as u32, flags)).into())
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };

    /// A text service needs a TIP client ID to watch keys, which an
    /// unregistered DLL does not get, so this drives the input mode part.
    #[test]
    fn the_input_mode_follows_the_compartment_and_the_indicator_button() -> Result<()> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            let result = (|| {
                let thread: ITfThreadMgr =
                    CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER)?;
                let client = thread.Activate()?;
                let service = ComObject::new(TextService::new());
                service.client_id.set(client);
                service.activate_input_mode(&thread, client)?;
                let open_close = input_mode::open_close(&thread)?;
                assert_eq!(input_mode::read(&open_close), InputMode::Japanese);
                assert_eq!(service.shared.composer.borrow().mode(), InputMode::Japanese);
                input_mode::write(&open_close, client, InputMode::Direct)?;
                assert_eq!(service.shared.composer.borrow().mode(), InputMode::Direct);
                let button = service.input_mode.borrow().as_ref().unwrap().button.clone();
                let button: ITfLangBarItemButton = button.to_interface();
                assert_eq!(button.GetText()?, "A");
                button.OnClick(TF_LBI_CLK_LEFT, POINT::default(), std::ptr::null())?;
                assert_eq!(service.shared.composer.borrow().mode(), InputMode::Japanese);
                assert_eq!(button.GetText()?, "あ");
                service.deactivate_input_mode(&thread);
                assert!(service.input_mode.borrow().is_none());
                thread.Deactivate()?;
                Ok(())
            })();
            CoUninitialize();
            result
        }
    }

    #[test]
    fn applications_can_draw_the_candidates_themselves() -> Result<()> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            let result = (|| {
                let thread: ITfThreadMgr =
                    CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER)?;
                let client = thread.Activate()?;
                let manager: ITfUIElementMgr = thread.cast()?;
                let service = ComObject::new(TextService::new());
                *service.ui_elements.borrow_mut() = Some(manager.clone());
                let documents = thread.CreateDocumentMgr()?;
                let mut context = None;
                let mut cookie = 0;
                documents.CreateContext(client, 0, None, &mut context, &mut cookie)?;
                let context = context.unwrap();
                let view = CandidateView {
                    texts: vec!["今日".into(), "京".into()],
                    selected: 0,
                    cloud: Vec::new(),
                    selecting: true,
                };
                service.present_candidates(&context, Some(&view));
                let id = service.candidate_element.borrow().as_ref().unwrap().id;
                let list: ITfCandidateListUIElement = manager.GetUIElement(id)?.cast()?;
                assert_eq!(list.GetCount()?, 2);
                assert!(
                    !service
                        .shared
                        .document
                        .borrow()
                        .application_draws_candidates
                );
                list.Show(false)?;
                assert!(
                    service
                        .shared
                        .document
                        .borrow()
                        .application_draws_candidates
                );
                service.present_candidates(
                    &context,
                    Some(&CandidateView {
                        selected: 1,
                        ..view
                    }),
                );
                assert_eq!(list.GetSelection()?, 1);
                service.present_candidates(&context, None);
                assert!(service.candidate_element.borrow().is_none());
                assert!(manager.GetUIElement(id).is_err());
                assert!(
                    !service
                        .shared
                        .document
                        .borrow()
                        .application_draws_candidates
                );
                thread.Deactivate()?;
                Ok(())
            })();
            CoUninitialize();
            result
        }
    }
}
