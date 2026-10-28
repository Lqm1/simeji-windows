//! The candidate window: a numbered list below the composition, owned by
//! the application's window, scaled for its DPI, announced with the IME
//! WinEvents so that light-dismiss surfaces keep it open, and exposed to
//! UI Automation.

use crate::{
    candidate_accessibility::{CandidateList, SharedList},
    candidates::{PAGE_SIZE, Page},
    dll,
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{
            Accessibility::{
                IRawElementProviderSimple, NotifyWinEvent, UIA_EVENT_ID, UIA_MenuClosedEventId,
                UIA_MenuOpenedEventId, UIA_SelectionItem_ElementSelectedEventId,
                UiaClientsAreListening, UiaRaiseAutomationEvent, UiaReturnRawElementProvider,
                UiaRootObjectId,
            },
            HiDpi::GetDpiForWindow,
            WindowsAndMessaging::*,
        },
    },
    core::*,
};

const DEFAULT_DPI: i32 = 96;
const WIDTH: i32 = 350;
const PADDING: i32 = 5;
const GAP_BELOW_COMPOSITION: i32 = 2;
const FONT_POINTS: i32 = 10;
const POINTS_PER_INCH: i32 = 72;
/// Rows are this much taller than the font, in percent.
const ROW_SPACING_PERCENT: i32 = 160;
/// Longer candidates are cut to keep the window narrow.
const MAX_CHARACTERS: usize = 36;
const CHILDID_SELF: i32 = 0;
/// Shows a shadow and marks the class as IME user interface.
const CLASS_STYLE: WNDCLASS_STYLES = WNDCLASS_STYLES(CS_IME.0 | CS_DROPSHADOW.0);

fn scale(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / DEFAULT_DPI
}

/// The data the window procedure reaches through `GWLP_USERDATA`.
struct WindowData {
    list: SharedList,
    provider: RefCell<Option<ComObject<CandidateList>>>,
    font: RefCell<Option<HFONT>>,
    footer: RefCell<Option<String>>,
}

pub(crate) struct CandidateWindow {
    window: Option<HWND>,
    data: Rc<WindowData>,
    /// The DPI the font was made for.
    font_dpi: u32,
    visible: bool,
}

impl Default for CandidateWindow {
    fn default() -> Self {
        Self {
            window: None,
            data: Rc::new(WindowData {
                list: SharedList::default(),
                provider: RefCell::new(None),
                font: RefCell::new(None),
                footer: RefCell::new(None),
            }),
            font_dpi: 0,
            visible: false,
        }
    }
}

/// The class name includes the module address: a class registered by an
/// unloaded copy of this DLL at another address is never used.
fn class_name() -> HSTRING {
    HSTRING::from(format!(
        "SimejiWindowsCandidates{:x}",
        dll::module().0 as usize
    ))
}

fn register_class() -> Result<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::Acquire) {
        return Ok(());
    }
    let name = class_name();
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CLASS_STYLE,
        lpfnWndProc: Some(window_procedure),
        hInstance: dll::module().into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        lpszClassName: PCWSTR(name.as_ptr()),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        let error = Error::from_thread();
        if error.code() != ERROR_CLASS_ALREADY_EXISTS.to_hresult() {
            return Err(error);
        }
    }
    REGISTERED.store(true, Ordering::Release);
    Ok(())
}

fn data(window: HWND) -> Option<&'static WindowData> {
    let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const WindowData;
    // The pointer stays valid until WM_NCDESTROY releases it.
    unsafe { pointer.as_ref() }
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_NCCREATE => {
                let create = &*(lparam.0 as *const CREATESTRUCTW);
                SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
                DefWindowProcW(window, message, wparam, lparam)
            }
            WM_NCDESTROY => {
                let pointer = SetWindowLongPtrW(window, GWLP_USERDATA, 0) as *const WindowData;
                if !pointer.is_null() {
                    drop(Rc::from_raw(pointer));
                }
                DefWindowProcW(window, message, wparam, lparam)
            }
            WM_PAINT => {
                if let Some(data) = data(window) {
                    paint(window, data);
                }
                LRESULT(0)
            }
            WM_GETOBJECT if lparam.0 as i32 == UiaRootObjectId => match data(window) {
                Some(data) => {
                    let provider: IRawElementProviderSimple = data
                        .provider
                        .borrow_mut()
                        .get_or_insert_with(|| {
                            ComObject::new(CandidateList::new(window, data.list.clone()))
                        })
                        .to_interface();
                    UiaReturnRawElementProvider(window, wparam, lparam, &provider)
                }
                None => DefWindowProcW(window, message, wparam, lparam),
            },
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

unsafe fn paint(window: HWND, data: &WindowData) {
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let context = BeginPaint(window, &mut paint);
        let mut client = RECT::default();
        let _ = GetClientRect(window, &mut client);
        FillRect(context, &client, GetSysColorBrush(COLOR_WINDOW));
        let previous_font = data
            .font
            .borrow()
            .map(|font| SelectObject(context, font.into()));
        SetBkMode(context, TRANSPARENT);
        let list = data.list.borrow();
        let lines = list
            .items
            .iter()
            .enumerate()
            .map(|(index, text)| (format!("{}  {text}", index + 1), index == list.selected))
            .chain(data.footer.borrow().clone().map(|footer| (footer, false)));
        for (row, (line, selected)) in lines.enumerate() {
            let top = list.padding + list.row_height * row as i32;
            let mut area = RECT {
                left: client.left,
                top,
                right: client.right,
                bottom: top + list.row_height,
            };
            let text_color = if selected {
                FillRect(context, &area, GetSysColorBrush(COLOR_HIGHLIGHT));
                COLOR_HIGHLIGHTTEXT
            } else {
                COLOR_WINDOWTEXT
            };
            SetTextColor(context, COLORREF(GetSysColor(text_color)));
            area.left += list.padding * 2;
            let mut text = line.encode_utf16().collect::<Vec<_>>();
            DrawTextW(
                context,
                &mut text,
                &mut area,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
        }
        if let Some(previous) = previous_font {
            SelectObject(context, previous);
        }
        let _ = EndPaint(window, &paint);
    }
}

impl CandidateWindow {
    pub fn hide(&mut self) {
        if let Some(window) = self.window.filter(|_| self.visible) {
            unsafe {
                let _ = ShowWindow(window, SW_HIDE);
                NotifyWinEvent(EVENT_OBJECT_IME_HIDE, window, OBJID_CLIENT.0, CHILDID_SELF);
            }
            self.raise_automation_event(UIA_MenuClosedEventId);
            self.visible = false;
        }
    }

    /// Shows `page` below `composition`, creating the window on first use.
    pub fn show(&mut self, parent: HWND, composition: RECT, page: &Page) -> Result<()> {
        let window = match self.window {
            Some(window) => window,
            None => {
                let window = self.create(parent)?;
                self.window = Some(window);
                window
            }
        };
        let dpi = unsafe { GetDpiForWindow(window) }.max(1);
        self.use_font(dpi);
        let selection_changed = {
            let mut list = self.data.list.borrow_mut();
            let items = Self::visible_items(page);
            let changed = !self.visible || list.items != items || list.selected != page.selected;
            list.items = items;
            list.selected = page.selected;
            list.row_height = Self::row_height(dpi);
            list.padding = scale(PADDING, dpi);
            changed
        };
        *self.data.footer.borrow_mut() = Self::footer(page);
        let rows = (page.items.len() + usize::from(page.total > PAGE_SIZE)) as i32;
        let height = rows * Self::row_height(dpi) + 2 * scale(PADDING, dpi);
        unsafe {
            SetWindowPos(
                window,
                Some(HWND_TOPMOST),
                composition.left,
                composition.bottom + scale(GAP_BELOW_COMPOSITION, dpi),
                scale(WIDTH, dpi),
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
            let _ = InvalidateRect(Some(window), None, true);
            let event = if self.visible {
                EVENT_OBJECT_IME_CHANGE
            } else {
                EVENT_OBJECT_IME_SHOW
            };
            NotifyWinEvent(event, window, OBJID_CLIENT.0, CHILDID_SELF);
        }
        if !self.visible {
            self.raise_automation_event(UIA_MenuOpenedEventId);
        }
        self.visible = true;
        if selection_changed {
            self.raise_selection_event();
        }
        Ok(())
    }

    fn row_height(dpi: u32) -> i32 {
        Self::font_height(dpi) * ROW_SPACING_PERCENT / 100
    }

    fn font_height(dpi: u32) -> i32 {
        FONT_POINTS * dpi as i32 / POINTS_PER_INCH
    }

    /// The candidates as shown: at most [`MAX_CHARACTERS`] long, without
    /// control characters.
    fn visible_items(page: &Page) -> Vec<String> {
        page.items
            .iter()
            .map(|text| {
                text.chars()
                    .take(MAX_CHARACTERS)
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect()
            })
            .collect()
    }

    /// The position among all candidates when they span several pages.
    fn footer(page: &Page) -> Option<String> {
        (page.total > PAGE_SIZE).then(|| format!("{}/{}", page.index + 1, page.total))
    }

    fn use_font(&mut self, dpi: u32) {
        if self.font_dpi == dpi {
            return;
        }
        let font = unsafe {
            CreateFontW(
                -Self::font_height(dpi),
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                DEFAULT_PITCH.0 as u32,
                w!("Yu Gothic UI"),
            )
        };
        if let Some(previous) = self.data.font.borrow_mut().replace(font) {
            unsafe {
                let _ = DeleteObject(previous.into());
            }
        }
        self.font_dpi = dpi;
    }

    fn raise_automation_event(&self, event: UIA_EVENT_ID) {
        let provider = self.data.provider.borrow().clone();
        if let Some(provider) = provider
            && unsafe { UiaClientsAreListening() }.as_bool()
        {
            let provider: IRawElementProviderSimple = provider.to_interface();
            unsafe {
                let _ = UiaRaiseAutomationEvent(&provider, event);
            }
        }
    }

    fn raise_selection_event(&self) {
        let provider = self.data.provider.borrow().clone();
        if let Some(provider) = provider
            && unsafe { UiaClientsAreListening() }.as_bool()
            && let Ok(item) = provider.selected_item()
        {
            unsafe {
                let _ = UiaRaiseAutomationEvent(&item, UIA_SelectionItem_ElementSelectedEventId);
            }
        }
    }

    /// A topmost window owned by `parent` that never takes focus.
    fn create(&self, parent: HWND) -> Result<HWND> {
        register_class()?;
        let data = Rc::into_raw(self.data.clone());
        let created = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                &class_name(),
                w!(""),
                WS_POPUP | WS_BORDER,
                0,
                0,
                WIDTH,
                PADDING,
                Some(parent),
                None,
                Some(dll::module().into()),
                Some(data.cast()),
            )
        };
        if created.is_err() {
            drop(unsafe { Rc::from_raw(data) });
        }
        created
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        unsafe {
            if let Some(window) = self.window {
                let _ = DestroyWindow(window);
            }
            if let Some(font) = self.data.font.borrow_mut().take() {
                let _ = DeleteObject(font.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        },
        UI::Accessibility::{
            CUIAutomation, IUIAutomation, TreeScope_Children, UIA_ListControlTypeId,
        },
    };

    fn page() -> Page {
        Page {
            items: vec!["今日".into(), "京\n".into(), "あ".repeat(40)],
            selected: 1,
            index: 10,
            total: 12,
        }
    }

    #[test]
    fn shortens_candidates_and_shows_the_position_on_long_lists() {
        let page = page();
        assert_eq!(
            CandidateWindow::visible_items(&page),
            ["今日".to_owned(), "京 ".into(), "あ".repeat(MAX_CHARACTERS)]
        );
        assert_eq!(CandidateWindow::footer(&page).as_deref(), Some("11/12"));
        assert_eq!(CandidateWindow::footer(&Page { total: 3, ..page }), None);
    }

    /// UI Automation must not query a window from the thread that owns it,
    /// so the client runs on another thread while this one pumps messages.
    #[test]
    fn exposes_the_candidates_to_ui_automation() -> Result<()> {
        let mut window = CandidateWindow::default();
        window.show(unsafe { GetDesktopWindow() }, RECT::default(), &page())?;
        let handle = window.window.unwrap().0 as isize;
        let (sender, receiver) = std::sync::mpsc::channel();
        let client = std::thread::spawn(move || {
            let read = || -> Result<(String, i32, String, i32)> {
                unsafe {
                    CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
                    let automation: IUIAutomation =
                        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
                    let element = automation.ElementFromHandle(HWND(handle as *mut _))?;
                    let condition = automation.CreateTrueCondition()?;
                    let items = element.FindAll(TreeScope_Children, &condition)?;
                    Ok((
                        element.CurrentAutomationId()?.to_string(),
                        items.Length()?,
                        items.GetElement(0)?.CurrentName()?.to_string(),
                        element.CurrentControlType()?.0,
                    ))
                }
            };
            let _ = sender.send(read().map_err(|error| error.code()));
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let result = loop {
            if let Ok(result) = receiver.try_recv() {
                break result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "UI Automation timed out"
            );
            unsafe {
                let mut message = MSG::default();
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        client.join().unwrap();
        let (automation_id, count, first, control_type) = result.map_err(Error::from_hresult)?;
        assert_eq!(automation_id, "IME_Candidate_Window");
        assert_eq!(count, 3);
        assert_eq!(first, "今日");
        assert_eq!(control_type, UIA_ListControlTypeId.0);
        window.hide();
        Ok(())
    }
}
