//! The candidate window: a numbered list below the composition.

use crate::candidates::{PAGE_SIZE, Page};
use windows::{
    Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*},
    core::*,
};

const WIDTH: i32 = 350;
const INITIAL_HEIGHT: i32 = 200;
const ROW_HEIGHT: i32 = 22;
const PADDING: i32 = 10;
const GAP_BELOW_COMPOSITION: i32 = 2;
/// Longer candidates are cut to keep the window narrow.
const MAX_CHARACTERS: usize = 36;

#[derive(Default)]
pub(crate) struct CandidateWindow {
    window: Option<HWND>,
}

impl CandidateWindow {
    pub fn hide(&self) {
        if let Some(window) = self.window {
            unsafe {
                let _ = ShowWindow(window, SW_HIDE);
            }
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
            SetWindowTextW(window, &text)?;
            SetWindowPos(
                window,
                Some(HWND_TOPMOST),
                composition.left,
                composition.bottom + GAP_BELOW_COMPOSITION,
                WIDTH,
                rows * ROW_HEIGHT + PADDING,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
        }
        Ok(())
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

    /// A topmost tool window that never takes focus from the application.
    unsafe fn create(parent: HWND) -> Result<HWND> {
        unsafe {
            let window = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                w!("STATIC"),
                w!(""),
                WS_POPUP | WS_BORDER,
                0,
                0,
                WIDTH,
                INITIAL_HEIGHT,
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
            Ok(window)
        }
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        if let Some(window) = self.window {
            unsafe {
                let _ = DestroyWindow(window);
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
