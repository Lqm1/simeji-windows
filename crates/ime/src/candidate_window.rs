//! The candidate window: a numbered list below the composition, owned by
//! the application's window, scaled for its DPI, and announced with the
//! IME WinEvents so that light-dismiss surfaces keep it open.

use crate::candidates::{PAGE_SIZE, Page};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{Accessibility::NotifyWinEvent, HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    },
    core::*,
};

const DEFAULT_DPI: i32 = 96;
const WIDTH: i32 = 350;
const ROW_HEIGHT: i32 = 22;
const PADDING: i32 = 10;
const GAP_BELOW_COMPOSITION: i32 = 2;
const FONT_POINTS: i32 = 10;
const POINTS_PER_INCH: i32 = 72;
/// Longer candidates are cut to keep the window narrow.
const MAX_CHARACTERS: usize = 36;
const CHILDID_SELF: i32 = 0;

#[derive(Default)]
pub(crate) struct CandidateWindow {
    window: Option<HWND>,
    /// The font and the DPI it was made for.
    font: Option<(HFONT, u32)>,
    visible: bool,
}

fn scale(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / DEFAULT_DPI
}

impl CandidateWindow {
    pub fn hide(&mut self) {
        if let Some(window) = self.window.filter(|_| self.visible) {
            unsafe {
                let _ = ShowWindow(window, SW_HIDE);
                NotifyWinEvent(EVENT_OBJECT_IME_HIDE, window, OBJID_CLIENT.0, CHILDID_SELF);
            }
            self.visible = false;
        }
    }

    /// Shows `page` below `composition`, creating the window on first use.
    pub fn show(&mut self, parent: HWND, composition: RECT, page: &Page) -> Result<()> {
        let lines = Self::format_page(page);
        let rows = lines.len() as i32;
        let text = HSTRING::from(lines.join("\r\n"));
        unsafe {
            let window = match self.window {
                Some(window) => window,
                None => {
                    let window = Self::create(parent)?;
                    self.window = Some(window);
                    window
                }
            };
            let dpi = GetDpiForWindow(window).max(1);
            self.use_font(window, dpi);
            SetWindowTextW(window, &text)?;
            SetWindowPos(
                window,
                Some(HWND_TOPMOST),
                composition.left,
                composition.bottom + scale(GAP_BELOW_COMPOSITION, dpi),
                scale(WIDTH, dpi),
                scale(rows * ROW_HEIGHT + PADDING, dpi),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
            let event = if self.visible {
                EVENT_OBJECT_IME_CHANGE
            } else {
                EVENT_OBJECT_IME_SHOW
            };
            NotifyWinEvent(event, window, OBJID_CLIENT.0, CHILDID_SELF);
            self.visible = true;
        }
        Ok(())
    }

    /// Gives the window a Japanese UI font sized for `dpi`.
    unsafe fn use_font(&mut self, window: HWND, dpi: u32) {
        if self.font.is_some_and(|(_, made_for)| made_for == dpi) {
            return;
        }
        unsafe {
            let font = CreateFontW(
                -(FONT_POINTS * dpi as i32 / POINTS_PER_INCH),
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
            );
            SendMessageW(
                window,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
            if let Some((previous, _)) = self.font.replace((font, dpi)) {
                let _ = DeleteObject(previous.into());
            }
        }
    }

    /// One numbered line per candidate, and the position among all of them
    /// when they span several pages.
    fn format_page(page: &Page) -> Vec<String> {
        let mut lines = page
            .items
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let marker = if index == page.selected { "▶" } else { "  " };
                let text = text
                    .chars()
                    .take(MAX_CHARACTERS)
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect::<String>();
                format!("{marker} {}  {text}", index + 1)
            })
            .collect::<Vec<_>>();
        if page.total > PAGE_SIZE {
            lines.push(format!("      {}/{}", page.index + 1, page.total));
        }
        lines
    }

    /// A topmost tool window, owned by `parent`, that never takes focus.
    unsafe fn create(parent: HWND) -> Result<HWND> {
        unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                w!("STATIC"),
                w!(""),
                WS_POPUP | WS_BORDER,
                0,
                0,
                WIDTH,
                PADDING,
                Some(parent),
                None,
                None,
                None,
            )
        }
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        unsafe {
            if let Some(window) = self.window {
                let _ = DestroyWindow(window);
            }
            if let Some((font, _)) = self.font {
                let _ = DeleteObject(font.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_the_page_and_marks_the_selection() {
        let page = Page {
            items: vec!["今日".into(), "京\n".into(), "あ".repeat(40)],
            selected: 1,
            index: 10,
            total: 12,
        };
        let lines = CandidateWindow::format_page(&page);
        assert_eq!(lines[0], "   1  今日");
        assert_eq!(lines[1], "▶ 2  京 ");
        assert_eq!(lines[2], format!("   3  {}", "あ".repeat(MAX_CHARACTERS)));
        assert_eq!(lines[3], "      11/12");
        let single = Page { total: 3, ..page };
        assert_eq!(CandidateWindow::format_page(&single).len(), 3);
    }
}
