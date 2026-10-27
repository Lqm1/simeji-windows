use crate::Lifetime;
use simeji_protocol::{Candidate, Clause, CloudWord, Request, Response};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    mem::ManuallyDrop,
    rc::{Rc, Weak},
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
            Variant::VARIANT,
        },
        UI::{Input::KeyboardAndMouse::*, TextServices::*, WindowsAndMessaging::*},
    },
    core::*,
};

fn worker(request: &Request) -> Result<Response> {
    simeji_transport::exchange(request, Duration::from_secs(2))
        .map_err(|e| Error::new(E_FAIL, e.to_string()))
}

#[derive(Clone)]
struct Choice {
    candidate: Candidate,
    whole_sentence: bool,
}

thread_local! {
    static TIMERS: RefCell<BTreeMap<usize, Weak<RefCell<State>>>> = const { RefCell::new(BTreeMap::new()) };
}
struct CloudTimer(usize);
impl CloudTimer {
    fn start(state: &Rc<RefCell<State>>) -> Option<Self> {
        let id = unsafe { SetTimer(None, 0, 150, Some(poll_cloud)) };
        if id == 0 {
            return None;
        }
        TIMERS.with(|timers| timers.borrow_mut().insert(id, Rc::downgrade(state)));
        Some(Self(id))
    }
}
impl Drop for CloudTimer {
    fn drop(&mut self) {
        unsafe {
            let _ = KillTimer(None, self.0);
        }
        TIMERS.with(|timers| timers.borrow_mut().remove(&self.0));
    }
}
unsafe extern "system" fn poll_cloud(_window: HWND, _message: u32, id: usize, _time: u32) {
    let state = TIMERS.with(|timers| timers.borrow().get(&id).and_then(Weak::upgrade));
    let Some(state) = state else {
        return;
    };
    let reading = {
        let Ok(state) = state.try_borrow() else {
            return;
        };
        if !state.cloud_enabled
            || !state.public_input
            || state.raw.is_empty()
            || !state.clauses.is_empty()
            || !state.cloud_words.is_empty()
        {
            return;
        }
        state.reading.clone()
    };
    // Polling never performs HTTP and cannot stall the host for an engine call.
    let response = simeji_transport::exchange::<_, Response>(
        &Request::PollCloud {
            reading: reading.clone(),
        },
        Duration::from_millis(20),
    );
    if let Ok(Response::CloudCandidates(words)) = response
        && let Ok(mut state) = state.try_borrow_mut()
        && state.public_input
        && state.reading == reading
        && state.clauses.is_empty()
    {
        state.cloud_words = words;
    }
}

fn public_input(context: &ITfContext, cookie: u32, range: &ITfRange) -> Result<bool> {
    unsafe {
        let value = context
            .GetProperty(&GUID_PROP_INPUTSCOPE)?
            .GetValue(cookie, range)?;
        if value.is_empty() {
            return Ok(true);
        }
        let unknown = IUnknown::try_from(&value)?;
        let scope: ITfInputScope = unknown.cast()?;
        let mut scopes = std::ptr::null_mut();
        let mut count = 0;
        scope.GetInputScopes(&mut scopes, &mut count)?;
        let allowed = if count == 0 {
            true
        } else if scopes.is_null() || count > 64 {
            false
        } else {
            !std::slice::from_raw_parts(scopes, count as usize)
                .iter()
                .any(|scope| matches!(scope.0, 31 | 61 | 63..=66))
        };
        CoTaskMemFree(Some(scopes.cast()));
        Ok(allowed)
    }
}

#[derive(Default)]
struct Popup {
    window: Option<HWND>,
}
impl Popup {
    fn hide(&self) {
        if let Some(window) = self.window {
            unsafe {
                let _ = ShowWindow(window, SW_HIDE);
            }
        }
    }
    fn show(
        &mut self,
        parent: HWND,
        rect: RECT,
        choices: &[Choice],
        selected: usize,
    ) -> Result<()> {
        if choices.is_empty() {
            self.hide();
            return Ok(());
        }
        let first = selected / 7 * 7;
        let text = choices
            .iter()
            .enumerate()
            .skip(first)
            .take(7)
            .map(|(i, c)| {
                format!(
                    "{} {}  {}",
                    if i == selected { "▶" } else { "  " },
                    i - first + 1,
                    c.candidate
                        .text
                        .chars()
                        .take(36)
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\r\n");
        let wide = HSTRING::from(text);
        unsafe {
            let window = if let Some(window) = self.window {
                window
            } else {
                let window = CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                    w!("STATIC"),
                    w!(""),
                    WS_POPUP | WS_BORDER,
                    0,
                    0,
                    350,
                    200,
                    Some(parent),
                    None,
                    None,
                    None,
                )?;
                let font = GetStockObject(DEFAULT_GUI_FONT);
                SendMessageW(
                    window,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(0)),
                );
                self.window = Some(window);
                window
            };
            SetWindowTextW(window, &wide)?;
            SetWindowPos(
                window,
                Some(HWND_TOPMOST),
                rect.left,
                rect.bottom + 2,
                350,
                (choices.len().min(7) * 22 + 10) as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
        }
        Ok(())
    }
}
impl Drop for Popup {
    fn drop(&mut self) {
        if let Some(window) = self.window {
            unsafe {
                let _ = DestroyWindow(window);
            }
        }
    }
}

#[derive(Default)]
struct State {
    raw: String,
    reading: String,
    clauses: Vec<Clause>,
    focus: usize,
    choices: Vec<Choice>,
    cloud_text: Option<String>,
    public_input: bool,
    cloud_words: Vec<CloudWord>,
    cloud_enabled: bool,
    selected: usize,
    accepted: Option<Candidate>,
    composition: Option<ITfComposition>,
    popup: Popup,
}
impl State {
    fn clear(&mut self) {
        self.raw.clear();
        self.reading.clear();
        self.clauses.clear();
        self.focus = 0;
        self.choices.clear();
        self.selected = 0;
        self.accepted = None;
        self.cloud_text = None;
        self.public_input = false;
        self.cloud_words.clear();
        self.composition = None;
        self.popup.hide();
    }
    fn text(&self) -> String {
        if let Some(text) = &self.cloud_text {
            return text.clone();
        }
        if self.clauses.is_empty() {
            self.reading.clone()
        } else {
            self.clauses.iter().map(|c| c.text.as_str()).collect()
        }
    }
}

#[implement(
    ITfTextInputProcessor,
    ITfKeyEventSink,
    ITfCompositionSink,
    ITfDisplayAttributeProvider
)]
pub(crate) struct TextService {
    state: Rc<RefCell<State>>,
    manager: RefCell<Option<ITfThreadMgr>>,
    client: Cell<u32>,
    enabled: Cell<bool>,
    cloud_timer: RefCell<Option<CloudTimer>>,
    atoms: Cell<[u32; 3]>,
    _lifetime: Lifetime,
}
impl TextService {
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(State::default())),
            manager: RefCell::new(None),
            client: Cell::new(0),
            enabled: Cell::new(true),
            cloud_timer: RefCell::new(None),
            atoms: Cell::new([0; 3]),
            _lifetime: Lifetime::new(),
        }
    }
}
impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, manager: Ref<ITfThreadMgr>, client: u32) -> Result<()> {
        let manager = manager.ok()?;
        let keys: ITfKeystrokeMgr = manager.cast()?;
        let sink: ITfKeyEventSink = self.to_interface();
        unsafe {
            let categories: ITfCategoryMgr =
                CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
            self.atoms.set([
                categories.RegisterGUID(&crate::display::GUIDS[0])?,
                categories.RegisterGUID(&crate::display::GUIDS[1])?,
                categories.RegisterGUID(&crate::display::GUIDS[2])?,
            ]);
            keys.AdviseKeyEventSink(client, &sink, true)?;
        }
        self.client.set(client);
        *self.manager.borrow_mut() = Some(manager.clone());
        *self.cloud_timer.borrow_mut() = CloudTimer::start(&self.state);
        Ok(())
    }
    fn Deactivate(&self) -> Result<()> {
        self.cloud_timer.borrow_mut().take();
        if let Some(manager) = self.manager.borrow_mut().take() {
            let keys: ITfKeystrokeMgr = manager.cast()?;
            unsafe {
                keys.UnadviseKeyEventSink(self.client.get())?;
            }
        }
        self.state.borrow_mut().clear();
        Ok(())
    }
}
impl ITfCompositionSink_Impl for TextService_Impl {
    fn OnCompositionTerminated(
        &self,
        _cookie: u32,
        _composition: Ref<ITfComposition>,
    ) -> Result<()> {
        self.state.borrow_mut().clear();
        Ok(())
    }
}
impl ITfDisplayAttributeProvider_Impl for TextService_Impl {
    fn EnumDisplayAttributeInfo(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(crate::display::enumerator(0))
    }
    fn GetDisplayAttributeInfo(&self, guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        crate::display::get(guid)
    }
}
impl TextService_Impl {
    fn edit(&self, context: &ITfContext, finish: bool, cancel: bool) -> Result<()> {
        let text = if cancel {
            String::new()
        } else {
            self.state.borrow().text()
        };
        let session: ITfEditSession = Edit {
            context: context.clone(),
            state: self.state.clone(),
            sink: self.to_interface(),
            text,
            finish,
            atoms: self.atoms.get(),
            _lifetime: Lifetime::new(),
        }
        .into();
        unsafe {
            context
                .RequestEditSession(self.client.get(), &session, TF_ES_SYNC | TF_ES_READWRITE)?
                .ok()
        }
    }
    fn choices(&self) -> Result<()> {
        let (reading, context) = {
            let state = self.state.borrow();
            let Some(clause) = state.clauses.get(state.focus) else {
                return Ok(());
            };
            (
                clause.reading.clone(),
                state.clauses[..state.focus]
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<String>(),
            )
        };
        match worker(&Request::Candidates { reading, context })? {
            Response::Candidates(choices) => {
                let reading = self.state.borrow().reading.clone();
                let cloud = self.state.borrow().cloud_words.clone();
                let mut choices = choices
                    .into_iter()
                    .map(|candidate| Choice {
                        candidate,
                        whole_sentence: false,
                    })
                    .collect::<Vec<_>>();
                let mut tail = Vec::new();
                for word in cloud.into_iter().filter(|w| w.scene.is_empty()) {
                    if choices
                        .iter()
                        .chain(tail.iter())
                        .any(|c: &Choice| c.candidate.text == word.text)
                    {
                        continue;
                    }
                    tail.push(Choice {
                        candidate: Candidate {
                            text: word.text,
                            reading: reading.clone(),
                            property: word.property,
                            left_id: 0,
                            right_id: 0,
                        },
                        whole_sentence: true,
                    });
                }
                let rest = tail.split_off(tail.len().min(6));
                let position = choices.len().min(1);
                choices.splice(position..position, tail);
                choices.extend(rest);
                let mut state = self.state.borrow_mut();
                state.selected = choices
                    .iter()
                    .position(|c| {
                        !c.whole_sentence && c.candidate.text == state.clauses[state.focus].text
                    })
                    .unwrap_or(0);
                state.choices = choices;
                Ok(())
            }
            Response::Error(error) => Err(Error::new(E_FAIL, error)),
            _ => Err(E_UNEXPECTED.into()),
        }
    }
    fn commit(&self, context: &ITfContext) -> Result<()> {
        let candidate = {
            let state = self.state.borrow();
            if state.public_input {
                state.accepted.clone()
            } else {
                None
            }
        };
        self.edit(context, true, false)?;
        if let Some(candidate) = candidate {
            // Keep execution on the COM apartment: a detached thread could
            // still execute this DLL after COM has unloaded it.
            let _ = worker(&Request::Learn {
                candidate,
                previous: None,
            });
        }
        Ok(())
    }
    fn handles(&self, key: u32) -> bool {
        if matches!(key, 0x19 | 0xf3 | 0xf4) {
            return true;
        }
        if !self.enabled.get() {
            return false;
        }
        unsafe {
            if GetKeyState(VK_CONTROL.0 as i32) < 0
                || GetKeyState(VK_MENU.0 as i32) < 0
                || GetKeyState(VK_LWIN.0 as i32) < 0
                || GetKeyState(VK_RWIN.0 as i32) < 0
            {
                return false;
            }
        }
        if !self.state.borrow().raw.is_empty()
            && matches!(key, 0x08 | 0x0d | 0x1b | 0x20 | 0x25..=0x28)
        {
            return true;
        }
        matches!(key,0x30..=0x39|0x41..=0x5a|0xba..=0xc0|0xdb..=0xde)
    }
    fn down(&self, context: &ITfContext, key: u32, lparam: LPARAM) -> Result<BOOL> {
        if !self.handles(key) {
            return Ok(false.into());
        }
        if matches!(key, 0x19 | 0xf3 | 0xf4) {
            if !self.state.borrow().raw.is_empty() {
                self.commit(context)?;
            }
            self.enabled.set(!self.enabled.get());
            return Ok(true.into());
        }
        if key == 0x0d {
            self.commit(context)?;
            return Ok(true.into());
        }
        if key == 0x1b {
            if self.state.borrow().clauses.is_empty() {
                self.edit(context, true, true)?;
            } else {
                let mut state = self.state.borrow_mut();
                state.clauses.clear();
                state.choices.clear();
                state.accepted = None;
                state.cloud_text = None;
                drop(state);
                self.edit(context, false, false)?;
            }
            return Ok(true.into());
        }
        if key == 0x20 {
            if self.state.borrow().clauses.is_empty() {
                let reading = self.state.borrow().reading.clone();
                match worker(&Request::Convert { reading })? {
                    Response::Conversion(conversion) => {
                        let mut state = self.state.borrow_mut();
                        state.accepted = conversion.candidates.first().cloned();
                        state.clauses = conversion.clauses;
                        state.focus = 0;
                    }
                    Response::Error(error) => return Err(Error::new(E_FAIL, error)),
                    _ => return Err(E_UNEXPECTED.into()),
                }
                self.choices()?;
            } else {
                self.select(if unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0 {
                    -1
                } else {
                    1
                })?;
            }
            self.edit(context, false, false)?;
            return Ok(true.into());
        }
        if matches!(key, 0x25 | 0x27) && !self.state.borrow().clauses.is_empty() {
            {
                let mut state = self.state.borrow_mut();
                if key == 0x25 {
                    state.focus = state.focus.saturating_sub(1);
                } else {
                    state.focus = (state.focus + 1).min(state.clauses.len() - 1);
                }
            }
            self.choices()?;
            self.edit(context, false, false)?;
            return Ok(true.into());
        }
        if matches!(key, 0x26 | 0x28) && !self.state.borrow().clauses.is_empty() {
            self.select(if key == 0x26 { -1 } else { 1 })?;
            self.edit(context, false, false)?;
            return Ok(true.into());
        }
        if (0x31..=0x37).contains(&key) && !self.state.borrow().choices.is_empty() {
            let (selected, index, count) = {
                let state = self.state.borrow();
                (
                    state.selected,
                    state.selected / 7 * 7 + (key - 0x31) as usize,
                    state.choices.len(),
                )
            };
            if index < count {
                self.select(index as isize - selected as isize)?;
                self.commit(context)?;
            }
            return Ok(true.into());
        }
        if key == 0x08 {
            let mut state = self.state.borrow_mut();
            state.raw.pop();
            state.clauses.clear();
            state.choices.clear();
            state.accepted = None;
            state.cloud_text = None;
        } else {
            if !self.state.borrow().clauses.is_empty() {
                self.commit(context)?;
            }
            let mut keyboard = [0; 256];
            let mut output = [0u16; 8];
            let count = unsafe {
                GetKeyboardState(&mut keyboard)?;
                ToUnicodeEx(
                    key,
                    (lparam.0 as u32 >> 16) & 255,
                    &keyboard,
                    &mut output,
                    4,
                    Some(GetKeyboardLayout(0)),
                )
            };
            if count <= 0 {
                return Ok(false.into());
            }
            let text = String::from_utf16(&output[..count as usize])
                .map_err(|_| Error::from_hresult(E_INVALIDARG))?;
            if self.state.borrow().raw.len() + text.len() > 512 {
                self.commit(context)?;
            }
            self.state.borrow_mut().raw.push_str(&text);
        }
        let raw = self.state.borrow().raw.clone();
        if raw.is_empty() {
            self.edit(context, true, true)?;
            return Ok(true.into());
        }
        match worker(&Request::Transliterate { romaji: raw })? {
            Response::Text(text) => self.state.borrow_mut().reading = text,
            Response::Error(error) => return Err(Error::new(E_FAIL, error)),
            _ => return Err(E_UNEXPECTED.into()),
        }
        self.state.borrow_mut().cloud_words.clear();
        self.edit(context, false, false)?;
        let reading = {
            let state = self.state.borrow();
            if state.public_input {
                state.reading.clone()
            } else {
                String::new()
            }
        };
        self.state.borrow_mut().cloud_enabled = matches!(
            worker(&Request::QueueCloud { reading }),
            Ok(Response::CloudEnabled(true))
        );
        Ok(true.into())
    }
    fn select(&self, step: isize) -> Result<()> {
        let mut state = self.state.borrow_mut();
        if state.choices.is_empty() {
            return Ok(());
        }
        state.selected =
            (state.selected as isize + step).rem_euclid(state.choices.len() as isize) as usize;
        let choice = state.choices[state.selected].clone();
        let candidate = choice.candidate;
        if choice.whole_sentence {
            state.cloud_text = Some(candidate.text);
            state.accepted = None;
            return Ok(());
        }
        state.cloud_text = None;
        let focus = state.focus;
        state.clauses[focus].text = candidate.text.clone();
        state.accepted = Some(candidate);
        Ok(())
    }
}
impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, foreground: BOOL) -> Result<()> {
        if !foreground.as_bool() {
            let mut state = self.state.borrow_mut();
            state.popup.hide();
            state.public_input = false;
            state.cloud_words.clear();
        }
        Ok(())
    }
    fn OnTestKeyDown(&self, context: Ref<ITfContext>, key: WPARAM, _flags: LPARAM) -> Result<BOOL> {
        let editable = context.as_ref().is_some_and(|context| unsafe {
            context
                .GetStatus()
                .is_ok_and(|status| status.dwStaticFlags & TF_SD_READONLY == 0)
        });
        Ok((editable && self.handles(key.0 as u32)).into())
    }
    fn OnTestKeyUp(&self, _context: Ref<ITfContext>, _key: WPARAM, _flags: LPARAM) -> Result<BOOL> {
        Ok(false.into())
    }
    fn OnKeyDown(&self, context: Ref<ITfContext>, key: WPARAM, flags: LPARAM) -> Result<BOOL> {
        let context = context.ok()?;
        let composition = self.state.borrow().composition.clone();
        if let Some(composition) = composition {
            let range = unsafe { composition.GetRange()? };
            let previous = unsafe { range.GetContext()? };
            if previous != *context {
                self.edit(&previous, true, false)?;
            }
        }
        self.down(context, key.0 as u32, flags)
    }
    fn OnKeyUp(&self, _context: Ref<ITfContext>, _key: WPARAM, _flags: LPARAM) -> Result<BOOL> {
        Ok(false.into())
    }
    fn OnPreservedKey(&self, _context: Ref<ITfContext>, _key: *const GUID) -> Result<BOOL> {
        Ok(false.into())
    }
}

#[implement(ITfEditSession)]
struct Edit {
    context: ITfContext,
    state: Rc<RefCell<State>>,
    sink: ITfCompositionSink,
    text: String,
    finish: bool,
    atoms: [u32; 3],
    _lifetime: Lifetime,
}
impl ITfEditSession_Impl for Edit_Impl {
    fn DoEditSession(&self, cookie: u32) -> Result<()> {
        let composition = self.state.borrow().composition.clone();
        let composition = if let Some(composition) = composition {
            composition
        } else {
            if self.text.is_empty() {
                self.state.borrow_mut().clear();
                return Ok(());
            }
            let insert: ITfInsertAtSelection = self.context.cast()?;
            let range = unsafe { insert.InsertTextAtSelection(cookie, TF_IAS_QUERYONLY, &[])? };
            let context: ITfContextComposition = self.context.cast()?;
            let composition = unsafe { context.StartComposition(cookie, &range, &self.sink)? };
            self.state.borrow_mut().composition = Some(composition.clone());
            composition
        };
        unsafe {
            let range = composition.GetRange()?;
            let public = public_input(&self.context, cookie, &range).unwrap_or(false);
            self.state.borrow_mut().public_input = public;
            range.SetText(cookie, 0, &self.text.encode_utf16().collect::<Vec<_>>())?;
            let property = self.context.GetProperty(&GUID_PROP_ATTRIBUTE)?;
            property.Clear(cookie, &range)?;
            if !self.finish {
                let (converted, focus) = {
                    let state = self.state.borrow();
                    let focus = if state.cloud_text.is_none() {
                        state.clauses.get(state.focus).map(|clause| {
                            (
                                state.clauses[..state.focus]
                                    .iter()
                                    .map(|c| c.text.encode_utf16().count())
                                    .sum::<usize>(),
                                clause.text.encode_utf16().count(),
                            )
                        })
                    } else {
                        None
                    };
                    (!state.clauses.is_empty(), focus)
                };
                property.SetValue(
                    cookie,
                    &range,
                    &VARIANT::from(self.atoms[usize::from(converted)] as i32),
                )?;
                if let Some((start, length)) = focus {
                    let focused = range.Clone()?;
                    let mut shifted = 0;
                    focused.ShiftStart(cookie, start as i32, &mut shifted, std::ptr::null())?;
                    focused.ShiftEnd(
                        cookie,
                        (start + length) as i32 - self.text.encode_utf16().count() as i32,
                        &mut shifted,
                        std::ptr::null(),
                    )?;
                    property.SetValue(cookie, &focused, &VARIANT::from(self.atoms[2] as i32))?;
                }
            }
            let caret = range.Clone()?;
            caret.Collapse(cookie, TF_ANCHOR_END)?;
            let mut selection = TF_SELECTION {
                range: ManuallyDrop::new(Some(caret)),
                style: TF_SELECTIONSTYLE {
                    ase: TF_AE_END,
                    fInterimChar: false.into(),
                },
            };
            let selected = self
                .context
                .SetSelection(cookie, std::slice::from_ref(&selection));
            ManuallyDrop::drop(&mut selection.range);
            selected?;
            if self.finish {
                composition.EndComposition(cookie)?;
                self.state.borrow_mut().clear();
            } else {
                let view = self.context.GetActiveView()?;
                let mut rect = RECT::default();
                let mut clipped = BOOL::default();
                if view
                    .GetTextExt(cookie, &range, &mut rect, &mut clipped)
                    .is_ok()
                {
                    let parent = view.GetWnd().unwrap_or_else(|_| GetFocus());
                    let (choices, selected) = {
                        let state = self.state.borrow();
                        (state.choices.clone(), state.selected)
                    };
                    self.state
                        .borrow_mut()
                        .popup
                        .show(parent, rect, &choices, selected)?;
                }
            }
        }
        Ok(())
    }
}
