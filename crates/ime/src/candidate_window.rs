//! The candidate window: a numbered list under the composition, owned by
//! the application's window, scaled for its DPI, kept clear of the touch
//! keyboard, announced with the IME WinEvents so that light-dismiss surfaces
//! keep it open, and exposed to UI Automation.
//!
//! Typing must not make it flicker. It stays open while the composition has
//! candidates, is moved or resized only when that changes, keeps its width
//! while open, repaints only when what it shows changes, and is drawn
//! completely before it appears. Direct2D and DirectWrite draw it, which
//! also gives color emoji and smooth corners.

use crate::{
    candidate_accessibility::{CandidateList, SharedList},
    candidates::{CandidateView, PAGE_SIZE},
    composer::CandidateAction,
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
        Graphics::{
            Direct2D::{
                Common::{D2D_RECT_F, D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT},
                D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
                D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
                D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1CreateFactory,
                ID2D1DCRenderTarget, ID2D1Factory, ID2D1RenderTarget,
            },
            DirectWrite::*,
            Dwm::*,
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::*,
        },
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
            Registry::*,
        },
        UI::{
            Accessibility::{
                HCF_HIGHCONTRASTON, HIGHCONTRASTW, IRawElementProviderSimple, NotifyWinEvent,
                UIA_EVENT_ID, UIA_MenuClosedEventId, UIA_MenuOpenedEventId,
                UIA_SelectionItem_ElementSelectedEventId, UiaClientsAreListening,
                UiaRaiseAutomationEvent, UiaReturnRawElementProvider, UiaRootObjectId,
            },
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
            Shell::{FrameworkInputPane, IFrameworkInputPane},
            WindowsAndMessaging::*,
        },
    },
    core::*,
};

const DEFAULT_DPI: u32 = 96;
const CHILDID_SELF: i32 = 0;
/// Shows a shadow and marks the class as IME user interface.
const CLASS_STYLE: WNDCLASS_STYLES = WNDCLASS_STYLES(CS_IME.0 | CS_DROPSHADOW.0);
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;
const WM_MOUSELEAVE: u32 = 0x02a3;

// Sizes at 96 DPI, after the expanded Microsoft IME list.
const PADDING: i32 = 4;
const ROW_HEIGHT: i32 = 32;
const CORNER: f32 = 4.0;
/// The selection mark and the number left of the candidate text.
const NUMBER_COLUMN: i32 = 32;
const MARK_WIDTH: f32 = 3.0;
const MARK_HEIGHT: f32 = 16.0;
/// The cloud mark at the end of a row.
const ICON_COLUMN: i32 = 32;
/// Space after the text of a row without a cloud mark.
const TEXT_END: i32 = 12;
const FOOTER_HEIGHT: i32 = 32;
const BUTTON_WIDTH: i32 = 32;
const BUTTON_HEIGHT: i32 = 28;
const FOOTER_TEXT_GAP: i32 = 12;
const MIN_WIDTH: i32 = 200;
const MAX_WIDTH: i32 = 520;
const GAP_TO_COMPOSITION: i32 = 2;
const TEXT_POINTS: f32 = 11.0;
const SMALL_POINTS: f32 = 9.0;
const ICON_PIXELS: f32 = 14.0;
const ARROW_PIXELS: f32 = 12.0;
/// Longer candidates are cut before they are measured.
const MAX_CHARACTERS: usize = 48;
/// The layout size for measuring text, larger than any window.
const MEASURE_LIMIT: f32 = 8192.0;

const TEXT_FONT: PCWSTR = w!("Yu Gothic UI");
const ICON_FONTS: [PCWSTR; 2] = [w!("Segoe Fluent Icons"), w!("Segoe MDL2 Assets")];
const LOCALE: PCWSTR = w!("ja-jp");
const CLOUD: &str = "\u{E753}";
/// CaretSolidUp and CaretSolidDown in Segoe Fluent Icons.
const FLUENT_ARROWS: [&str; 2] = ["\u{F090}", "\u{F08E}"];
/// ChevronUpSmall and ChevronDownSmall, which Segoe MDL2 Assets has too.
const MDL2_ARROWS: [&str; 2] = ["\u{E96D}", "\u{E96E}"];
const TEXT_ARROWS: [&str; 2] = ["▲", "▼"];
const CLOUD_LABEL: &str = "クラウド候補";

fn scale(value: i32, dpi: u32) -> i32 {
    (value * dpi as i32 + DEFAULT_DPI as i32 / 2) / DEFAULT_DPI as i32
}

fn scale_f(value: f32, dpi: u32) -> f32 {
    value * dpi as f32 / DEFAULT_DPI as f32
}

fn points(points: f32, dpi: u32) -> f32 {
    points * dpi as f32 / 72.0
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn rect_f(rect: RECT) -> D2D_RECT_F {
    D2D_RECT_F {
        left: rect.left as f32,
        top: rect.top as f32,
        right: rect.right as f32,
        bottom: rect.bottom as f32,
    }
}

fn contains(rect: RECT, x: i32, y: i32) -> bool {
    rect.left <= x && x < rect.right && rect.top <= y && y < rect.bottom
}

type CandidateHandler = Rc<dyn Fn(CandidateAction)>;

/// Colors as `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Theme {
    dark: bool,
    high_contrast: bool,
    background: u32,
    text: u32,
    secondary: u32,
    hover: u32,
    selection: u32,
    selection_text: u32,
    accent: u32,
    border: u32,
    disabled: u32,
}

/// Converts between `0xRRGGBB` and `COLORREF`'s `0xBBGGRR`.
fn swap_red_blue(color: u32) -> u32 {
    ((color & 0xff) << 16) | (color & 0xff00) | ((color >> 16) & 0xff)
}

fn d2d_color(rgb: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xff) as f32 / 255.0,
        g: ((rgb >> 8) & 0xff) as f32 / 255.0,
        b: (rgb & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

fn read_registry<const N: usize>(
    key: PCWSTR,
    value: PCWSTR,
    flags: REG_ROUTINE_FLAGS,
) -> Option<[u8; N]> {
    let mut data = [0u8; N];
    let mut size = N as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            flags,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .is_ok()
    .then_some(data)
    .filter(|_| size as usize == N)
}

impl Theme {
    fn read() -> Self {
        unsafe {
            let mut contrast = HIGHCONTRASTW {
                cbSize: size_of::<HIGHCONTRASTW>() as u32,
                ..Default::default()
            };
            let _ = SystemParametersInfoW(
                SPI_GETHIGHCONTRAST,
                contrast.cbSize,
                Some((&raw mut contrast).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            );
            if contrast.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0 {
                let system = |index| swap_red_blue(GetSysColor(index));
                return Self {
                    dark: false,
                    high_contrast: true,
                    background: system(COLOR_WINDOW),
                    text: system(COLOR_WINDOWTEXT),
                    secondary: system(COLOR_WINDOWTEXT),
                    hover: system(COLOR_HIGHLIGHT),
                    selection: system(COLOR_HIGHLIGHT),
                    selection_text: system(COLOR_HIGHLIGHTTEXT),
                    accent: system(COLOR_HIGHLIGHTTEXT),
                    border: system(COLOR_WINDOWTEXT),
                    disabled: system(COLOR_GRAYTEXT),
                };
            }
        }
        let light = read_registry::<4>(
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
        )
        .is_none_or(|value| u32::from_le_bytes(value) != 0);
        let dark = !light;
        let accent = Self::accent(dark).unwrap_or(if dark { 0x60cdff } else { 0x005fb8 });
        Self::colors(dark, accent)
    }

    /// The Windows 11 flyout colors.
    fn colors(dark: bool, accent: u32) -> Self {
        if dark {
            Self {
                dark,
                high_contrast: false,
                background: 0x2c2c2c,
                text: 0xffffff,
                secondary: 0xc5c5c5,
                hover: 0x383838,
                selection: 0x3d3d3d,
                selection_text: 0xffffff,
                accent,
                border: 0x3f3f3f,
                disabled: 0x787878,
            }
        } else {
            Self {
                dark,
                high_contrast: false,
                background: 0xf9f9f9,
                text: 0x1b1b1b,
                secondary: 0x616161,
                hover: 0xf0f0f0,
                selection: 0xe9e9e9,
                selection_text: 0x1b1b1b,
                accent,
                border: 0xe0e0e0,
                disabled: 0xa0a0a0,
            }
        }
    }

    /// The accent shade Windows uses for selection marks: a darker one on
    /// light backgrounds and a lighter one on dark backgrounds.
    fn accent(dark: bool) -> Option<u32> {
        let palette = read_registry::<32>(
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
        )?;
        let index = if dark { 1 } else { 4 };
        let [r, g, b] = [0, 1, 2].map(|channel| u32::from(palette[index * 4 + channel]));
        Some((r << 16) | (g << 8) | b)
    }
}

/// What a point in the window acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Row(usize),
    PreviousPage,
    NextPage,
}

/// What the window shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Model {
    items: Vec<String>,
    cloud: Vec<bool>,
    /// The selected row, once the user selects.
    selected: Option<usize>,
    page: usize,
    pages: usize,
}

impl Model {
    fn new(view: &CandidateView) -> Self {
        let start = view.page_start();
        let items = Self::visible_items(view);
        Self {
            cloud: (start..start + items.len())
                .map(|index| view.cloud.get(index).copied().unwrap_or(false))
                .collect(),
            items,
            selected: view.selecting.then(|| view.selected_on_page()),
            page: start / PAGE_SIZE,
            pages: view.page_count(),
        }
    }

    fn visible_items(view: &CandidateView) -> Vec<String> {
        view.page()
            .iter()
            .map(|text| {
                text.chars()
                    .take(MAX_CHARACTERS)
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect()
            })
            .collect()
    }

    fn has_cloud(&self) -> bool {
        self.cloud.iter().any(|cloud| *cloud)
    }

    /// The text right of the page buttons; the cloud mark goes before it.
    fn footer(&self) -> String {
        let page = (self.pages > 1).then(|| format!("{}/{}", self.page + 1, self.pages));
        match (self.has_cloud(), page) {
            (true, Some(page)) => format!("{CLOUD_LABEL}　{page}"),
            (true, None) => CLOUD_LABEL.into(),
            (false, Some(page)) => page,
            (false, None) => String::new(),
        }
    }

    /// The page buttons and the footer text, when there is something to
    /// page through or a cloud mark to explain.
    fn has_footer(&self) -> bool {
        self.pages > 1 || self.has_cloud()
    }

    /// Lists of several pages keep the height of a full page.
    fn rows(&self) -> usize {
        if self.pages > 1 {
            PAGE_SIZE
        } else {
            self.items.len()
        }
    }
}

/// Where everything is, in pixels of the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Layout {
    dpi: u32,
    width: i32,
    rows: usize,
    footer: bool,
}

impl Layout {
    fn px(&self, value: i32) -> i32 {
        scale(value, self.dpi)
    }

    fn height(&self) -> i32 {
        let footer = if self.footer {
            self.px(FOOTER_HEIGHT)
        } else {
            0
        };
        self.px(PADDING) * 2 + self.px(ROW_HEIGHT) * self.rows as i32 + footer
    }

    fn row(&self, row: usize) -> RECT {
        let top = self.px(PADDING) + self.px(ROW_HEIGHT) * row as i32;
        RECT {
            left: self.px(PADDING),
            top,
            right: self.width - self.px(PADDING),
            bottom: top + self.px(ROW_HEIGHT),
        }
    }

    /// Where the candidate text starts, from the left of the window.
    fn text_left(&self) -> i32 {
        self.px(PADDING) + self.px(NUMBER_COLUMN)
    }

    fn footer(&self) -> RECT {
        let top = self.px(PADDING) + self.px(ROW_HEIGHT) * self.rows as i32;
        RECT {
            left: self.px(PADDING),
            top,
            right: self.width - self.px(PADDING),
            bottom: top + self.px(FOOTER_HEIGHT),
        }
    }

    fn button(&self, next: bool) -> RECT {
        let footer = self.footer();
        let top = footer.top + (footer.bottom - footer.top - self.px(BUTTON_HEIGHT)) / 2;
        let left = footer.left + if next { self.px(BUTTON_WIDTH) } else { 0 };
        RECT {
            left,
            top,
            right: left + self.px(BUTTON_WIDTH),
            bottom: top + self.px(BUTTON_HEIGHT),
        }
    }

    fn hit(&self, model: &Model, x: i32, y: i32) -> Option<Target> {
        if let Some(row) = (0..model.items.len()).find(|row| contains(self.row(*row), x, y)) {
            return Some(Target::Row(row));
        }
        if model.pages <= 1 {
            return None;
        }
        [Target::PreviousPage, Target::NextPage]
            .into_iter()
            .find(|target| contains(self.button(*target == Target::NextPage), x, y))
    }
}

/// The text formats for one DPI.
struct Text {
    dpi: u32,
    candidate: IDWriteTextFormat,
    number: IDWriteTextFormat,
    footer: IDWriteTextFormat,
    icons: Option<IDWriteTextFormat>,
    arrows: Option<IDWriteTextFormat>,
    arrow_glyphs: [&'static str; 2],
}

fn text_format(
    factory: &IDWriteFactory,
    family: PCWSTR,
    size: f32,
    alignment: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = factory.CreateTextFormat(
            family,
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            LOCALE,
        )?;
        format.SetTextAlignment(alignment)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let sign = factory.CreateEllipsisTrimmingSign(&format)?;
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        format.SetTrimming(&trimming, &sign)?;
        Ok(format)
    }
}

/// The first installed icon font: Segoe Fluent Icons on Windows 11.
fn icon_font(factory: &IDWriteFactory) -> Option<PCWSTR> {
    let mut fonts = None;
    unsafe { factory.GetSystemFontCollection(&mut fonts, false) }.ok()?;
    let fonts = fonts?;
    ICON_FONTS.into_iter().find(|family| {
        let (mut index, mut exists) = (0, BOOL::default());
        unsafe { fonts.FindFamilyName(*family, &mut index, &mut exists) }.is_ok()
            && exists.as_bool()
    })
}

impl Text {
    fn new(factory: &IDWriteFactory, dpi: u32) -> Result<Self> {
        let icon_family = icon_font(factory);
        let icon_format = |size| {
            icon_family.and_then(|family| {
                text_format(factory, family, size, DWRITE_TEXT_ALIGNMENT_CENTER).ok()
            })
        };
        let arrow_glyphs = match icon_family {
            Some(family) if family == ICON_FONTS[0] => FLUENT_ARROWS,
            Some(_) => MDL2_ARROWS,
            None => TEXT_ARROWS,
        };
        Ok(Self {
            dpi,
            candidate: text_format(
                factory,
                TEXT_FONT,
                points(TEXT_POINTS, dpi),
                DWRITE_TEXT_ALIGNMENT_LEADING,
            )?,
            number: text_format(
                factory,
                TEXT_FONT,
                points(SMALL_POINTS + 1.0, dpi),
                DWRITE_TEXT_ALIGNMENT_CENTER,
            )?,
            footer: text_format(
                factory,
                TEXT_FONT,
                points(SMALL_POINTS, dpi),
                DWRITE_TEXT_ALIGNMENT_TRAILING,
            )?,
            icons: icon_format(scale_f(ICON_PIXELS, dpi)),
            arrows: icon_format(scale_f(ARROW_PIXELS, dpi)),
            arrow_glyphs,
        })
    }

    fn width(factory: &IDWriteFactory, format: &IDWriteTextFormat, text: &str) -> i32 {
        let mut metrics = DWRITE_TEXT_METRICS::default();
        let measured = unsafe {
            factory
                .CreateTextLayout(&wide(text), format, MEASURE_LIMIT, MEASURE_LIMIT)
                .and_then(|layout| layout.GetMetrics(&mut metrics))
        };
        if measured.is_err() {
            return 0;
        }
        metrics.widthIncludingTrailingWhitespace.ceil() as i32
    }
}

/// Everything painting needs, shared with the window procedure.
struct Surface {
    model: Model,
    layout: Layout,
    theme: Theme,
    /// Whether DWM rounds the corners and draws the border.
    rounded: bool,
    hover: Option<Target>,
    pressed: Option<Target>,
    tracking_mouse: bool,
    writer: Option<IDWriteFactory>,
    painter: Option<ID2D1Factory>,
    target: Option<ID2D1DCRenderTarget>,
    text: Option<Text>,
}

impl Default for Surface {
    fn default() -> Self {
        Self {
            model: Model::default(),
            layout: Layout::default(),
            theme: Theme::read(),
            rounded: false,
            hover: None,
            pressed: None,
            tracking_mouse: false,
            writer: None,
            painter: None,
            target: None,
            text: None,
        }
    }
}

impl Surface {
    fn writer(&mut self) -> Result<IDWriteFactory> {
        if let Some(writer) = &self.writer {
            return Ok(writer.clone());
        }
        let writer: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        self.writer = Some(writer.clone());
        Ok(writer)
    }

    /// The text formats for `dpi`.
    fn text(&mut self, dpi: u32) -> Result<&Text> {
        if self.text.as_ref().is_none_or(|text| text.dpi != dpi) {
            let writer = self.writer()?;
            self.text = Some(Text::new(&writer, dpi)?);
        }
        Ok(self.text.as_ref().unwrap())
    }

    /// The width the model needs at `dpi`, within the limits.
    fn measure(&mut self, model: &Model, dpi: u32) -> Result<i32> {
        let writer = self.writer()?;
        let text = self.text(dpi)?;
        let px = |value| scale(value, dpi);
        let longest = model
            .items
            .iter()
            .map(|item| Text::width(&writer, &text.candidate, item))
            .max()
            .unwrap_or(0);
        let end = if model.has_cloud() {
            ICON_COLUMN
        } else {
            TEXT_END
        };
        let rows = px(PADDING) * 2 + px(NUMBER_COLUMN) + longest + px(end);
        if !model.has_footer() {
            return Ok(rows.clamp(px(MIN_WIDTH), px(MAX_WIDTH)));
        }
        let footer_text = Text::width(&writer, &text.footer, &model.footer());
        let icon = if model.has_cloud() {
            px(ICON_COLUMN) / 2
        } else {
            0
        };
        let footer =
            px(PADDING) * 2 + px(BUTTON_WIDTH) * 2 + px(FOOTER_TEXT_GAP) * 2 + icon + footer_text;
        Ok(rows.max(footer).clamp(px(MIN_WIDTH), px(MAX_WIDTH)))
    }

    fn render_target(&mut self) -> Result<ID2D1DCRenderTarget> {
        if let Some(target) = &self.target {
            return Ok(target.clone());
        }
        let painter = match &self.painter {
            Some(painter) => painter.clone(),
            None => {
                let painter: ID2D1Factory =
                    unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
                self.painter = Some(painter.clone());
                painter
            }
        };
        // One unit is one pixel; the layout already scales for the DPI.
        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: DEFAULT_DPI as f32,
            dpiY: DEFAULT_DPI as f32,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let target = unsafe { painter.CreateDCRenderTarget(&properties)? };
        self.target = Some(target.clone());
        Ok(target)
    }

    fn render(&mut self, context: HDC, client: &RECT) -> Result<()> {
        let target = self.render_target()?;
        unsafe {
            target.BindDC(context, client)?;
            target.BeginDraw();
            let drawn = self.draw(&target);
            let ended = target.EndDraw(None, None);
            if let Err(error) = &ended
                && error.code() == D2DERR_RECREATE_TARGET
            {
                self.target = None;
            }
            drawn.and(ended)
        }
    }

    fn draw(&mut self, target: &ID2D1RenderTarget) -> Result<()> {
        let layout = self.layout;
        let theme = self.theme;
        let (model, hover, rounded) = (self.model.clone(), self.hover, self.rounded);
        self.text(layout.dpi)?;
        let (Some(text), Some(writer)) = (self.text.as_ref(), self.writer.as_ref()) else {
            return Ok(());
        };
        let px = |value| scale(value, layout.dpi);
        let brush = |rgb| unsafe { target.CreateSolidColorBrush(&d2d_color(rgb), None) };
        let fill = |area: RECT, rgb, radius: f32| -> Result<()> {
            let rounded = D2D1_ROUNDED_RECT {
                rect: rect_f(area),
                radiusX: radius,
                radiusY: radius,
            };
            unsafe { target.FillRoundedRectangle(&rounded, &brush(rgb)?) };
            Ok(())
        };
        let label = |content: &str, format: &IDWriteTextFormat, area: RECT, rgb| -> Result<()> {
            unsafe {
                target.DrawText(
                    &wide(content),
                    format,
                    &rect_f(area),
                    &brush(rgb)?,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP | D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            Ok(())
        };
        let corner = scale_f(CORNER, layout.dpi);
        unsafe { target.Clear(Some(&d2d_color(theme.background))) };
        if !rounded {
            let border = RECT {
                left: 0,
                top: 0,
                right: layout.width,
                bottom: layout.height(),
            };
            unsafe {
                target.DrawRectangle(
                    &D2D_RECT_F {
                        left: 0.5,
                        top: 0.5,
                        right: border.right as f32 - 0.5,
                        bottom: border.bottom as f32 - 0.5,
                    },
                    &brush(theme.border)?,
                    1.0,
                    None,
                );
            }
        }
        let end_column = if model.has_cloud() {
            ICON_COLUMN
        } else {
            TEXT_END
        };
        for (row, item) in model.items.iter().enumerate() {
            let area = layout.row(row);
            let selected = model.selected == Some(row);
            let hovered = hover == Some(Target::Row(row));
            let highlighted = selected || (hovered && theme.high_contrast);
            if highlighted {
                fill(area, theme.selection, corner)?;
            } else if hovered {
                fill(area, theme.hover, corner)?;
            }
            if selected && !theme.high_contrast {
                let mark_height = scale_f(MARK_HEIGHT, layout.dpi);
                let top = (area.top + area.bottom) as f32 / 2.0 - mark_height / 2.0;
                let mark = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: area.left as f32,
                        top,
                        right: area.left as f32 + scale_f(MARK_WIDTH, layout.dpi),
                        bottom: top + mark_height,
                    },
                    radiusX: scale_f(MARK_WIDTH, layout.dpi) / 2.0,
                    radiusY: scale_f(MARK_WIDTH, layout.dpi) / 2.0,
                };
                unsafe { target.FillRoundedRectangle(&mark, &brush(theme.accent)?) };
            }
            let (foreground, secondary) = if highlighted {
                (theme.selection_text, theme.selection_text)
            } else {
                (theme.text, theme.secondary)
            };
            label(
                &(row + 1).to_string(),
                &text.number,
                RECT {
                    right: area.left + px(NUMBER_COLUMN) - px(4),
                    ..area
                },
                secondary,
            )?;
            label(
                item,
                &text.candidate,
                RECT {
                    left: layout.text_left(),
                    right: area.right - px(end_column),
                    ..area
                },
                foreground,
            )?;
            if model.cloud.get(row) == Some(&true)
                && let Some(icons) = &text.icons
            {
                label(
                    CLOUD,
                    icons,
                    RECT {
                        left: area.right - px(ICON_COLUMN),
                        ..area
                    },
                    secondary,
                )?;
            }
        }
        if !layout.footer {
            return Ok(());
        }
        let footer = layout.footer();
        let paged = model.pages > 1;
        for (glyph, target_button) in text
            .arrow_glyphs
            .into_iter()
            .zip([Target::PreviousPage, Target::NextPage])
        {
            let area = layout.button(target_button == Target::NextPage);
            if paged && hover == Some(target_button) {
                fill(area, theme.hover, corner)?;
            }
            let color = if paged { theme.text } else { theme.disabled };
            label(
                glyph,
                text.arrows.as_ref().unwrap_or(&text.number),
                area,
                color,
            )?;
        }
        let content = model.footer();
        if !content.is_empty() {
            let area = RECT {
                left: layout.button(true).right + px(FOOTER_TEXT_GAP),
                right: footer.right - px(FOOTER_TEXT_GAP),
                ..footer
            };
            label(&content, &text.footer, area, theme.secondary)?;
            if model.has_cloud()
                && let Some(icons) = &text.icons
            {
                let width = Text::width(writer, &text.footer, &content);
                let right = area.right - width;
                label(
                    CLOUD,
                    icons,
                    RECT {
                        left: right - px(ICON_COLUMN) / 2 - px(4),
                        right: right - px(4),
                        ..area
                    },
                    theme.secondary,
                )?;
            }
        }
        Ok(())
    }

    /// Shows DWM's rounded corners and border in the theme's colors.
    fn apply_frame(&mut self, window: HWND) {
        unsafe {
            let disabled = BOOL::from(true);
            let _ = DwmSetWindowAttribute(
                window,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                (&raw const disabled).cast(),
                size_of_val(&disabled) as u32,
            );
            let corner = DWMWCP_ROUND;
            self.rounded = DwmSetWindowAttribute(
                window,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&raw const corner).cast(),
                size_of_val(&corner) as u32,
            )
            .is_ok();
            let dark = BOOL::from(self.theme.dark);
            let _ = DwmSetWindowAttribute(
                window,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&raw const dark).cast(),
                size_of_val(&dark) as u32,
            );
            let border = swap_red_blue(self.theme.border);
            let _ = DwmSetWindowAttribute(
                window,
                DWMWA_BORDER_COLOR,
                (&raw const border).cast(),
                size_of_val(&border) as u32,
            );
        }
    }
}

/// The data the window procedure reaches through `GWLP_USERDATA`.
#[derive(Default)]
struct WindowData {
    list: SharedList,
    provider: RefCell<Option<ComObject<CandidateList>>>,
    surface: RefCell<Surface>,
    action: RefCell<Option<CandidateHandler>>,
}

impl WindowData {
    fn raise_automation_event(&self, event: UIA_EVENT_ID) {
        let provider = self.provider.borrow().clone();
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
        let provider = self.provider.borrow().clone();
        if let Some(provider) = provider
            && unsafe { UiaClientsAreListening() }.as_bool()
            && let Ok(item) = provider.selected_item()
        {
            unsafe {
                let _ = UiaRaiseAutomationEvent(&item, UIA_SelectionItem_ElementSelectedEventId);
            }
        }
    }
}

/// The composition the window belongs to.
#[derive(Clone, Copy, Debug)]
struct Anchor {
    parent: HWND,
    composition: RECT,
}

#[derive(Default)]
pub(crate) struct CandidateWindow {
    window: Option<HWND>,
    data: Rc<WindowData>,
    anchor: Option<Anchor>,
    visible: bool,
    /// The window's place on screen while it is open.
    placed: Option<RECT>,
    /// The width while open; it only grows so that the window keeps still.
    width: i32,
    /// Where the touch keyboard was when the window opened.
    touch_keyboard: Option<RECT>,
    input_pane: Option<Option<IFrameworkInputPane>>,
}

/// The window's top left corner: the candidate text lines up with the
/// composition, below it unless that leaves the screen or covers the touch
/// keyboard.
fn place(
    composition: RECT,
    size: SIZE,
    text_left: i32,
    gap: i32,
    work_area: RECT,
    touch_keyboard: Option<RECT>,
) -> POINT {
    let x = (composition.left - text_left)
        .min(work_area.right - size.cx)
        .max(work_area.left);
    let mut lowest = work_area.bottom;
    if let Some(keyboard) = touch_keyboard
        && keyboard.left < x + size.cx
        && x < keyboard.right
    {
        lowest = lowest.min(keyboard.top);
    }
    let below = composition.bottom + gap;
    let y = if below + size.cy <= lowest {
        below
    } else {
        (composition.top - gap - size.cy).max(work_area.top)
    };
    POINT { x, y }
}

fn work_area(composition: RECT) -> Option<RECT> {
    unsafe {
        let monitor = MonitorFromRect(&composition, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(monitor, &mut info)
            .as_bool()
            .then_some(info.rcWork)
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

fn mouse_position(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 as u16) as i16 as i32;
    let y = ((lparam.0 >> 16) as u16) as i16 as i32;
    (x, y)
}

fn redraw(window: HWND) {
    unsafe {
        let _ = RedrawWindow(Some(window), None, None, RDW_INVALIDATE | RDW_UPDATENOW);
    }
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
            // Painting covers every pixel, so erasing first would only flicker.
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let context = BeginPaint(window, &mut paint);
                if let Some(data) = data(window)
                    && let Ok(mut surface) = data.surface.try_borrow_mut()
                {
                    let mut client = RECT::default();
                    let _ = GetClientRect(window, &mut client);
                    if surface.render(context, &client).is_err() {
                        let brush =
                            CreateSolidBrush(COLORREF(swap_red_blue(surface.theme.background)));
                        FillRect(context, &client, brush);
                        let _ = DeleteObject(brush.into());
                    }
                }
                let _ = EndPaint(window, &paint);
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
            WM_MOUSEMOVE => {
                if let Some(data) = data(window) {
                    let (x, y) = mouse_position(lparam);
                    let changed = {
                        let mut surface = data.surface.borrow_mut();
                        if !surface.tracking_mouse {
                            let mut track = TRACKMOUSEEVENT {
                                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                                dwFlags: TME_LEAVE,
                                hwndTrack: window,
                                dwHoverTime: 0,
                            };
                            surface.tracking_mouse = TrackMouseEvent(&mut track).is_ok();
                        }
                        let hover = surface.layout.hit(&surface.model, x, y);
                        std::mem::replace(&mut surface.hover, hover) != hover
                    };
                    if changed {
                        redraw(window);
                    }
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                if let Some(data) = data(window) {
                    let changed = {
                        let mut surface = data.surface.borrow_mut();
                        surface.tracking_mouse = false;
                        surface.pressed = None;
                        surface.hover.take().is_some()
                    };
                    if changed {
                        redraw(window);
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                if let Some(data) = data(window) {
                    let (x, y) = mouse_position(lparam);
                    let mut surface = data.surface.borrow_mut();
                    surface.pressed = surface.layout.hit(&surface.model, x, y);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(data) = data(window) {
                    let (x, y) = mouse_position(lparam);
                    let (target, page) = {
                        let mut surface = data.surface.borrow_mut();
                        let pressed = surface.pressed.take();
                        let target = surface.layout.hit(&surface.model, x, y);
                        (target.filter(|_| target == pressed), surface.model.page)
                    };
                    let action = data.action.borrow().clone();
                    // The action updates this window, so nothing is borrowed.
                    match (action, target) {
                        (Some(action), Some(Target::Row(row))) => {
                            action(CandidateAction::Select(page * PAGE_SIZE + row));
                            action(CandidateAction::Commit);
                        }
                        (Some(action), Some(Target::PreviousPage)) => {
                            action(CandidateAction::Page(-1));
                        }
                        (Some(action), Some(Target::NextPage)) => action(CandidateAction::Page(1)),
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            WM_SETTINGCHANGE
            | WM_THEMECHANGED
            | WM_SYSCOLORCHANGE
            | WM_DWMCOLORIZATIONCOLORCHANGED => {
                if let Some(data) = data(window) {
                    {
                        let mut surface = data.surface.borrow_mut();
                        surface.theme = Theme::read();
                        surface.apply_frame(window);
                    }
                    redraw(window);
                }
                DefWindowProcW(window, message, wparam, lparam)
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

impl CandidateWindow {
    pub fn set_action(&mut self, action: CandidateHandler) {
        *self.data.action.borrow_mut() = Some(action);
    }

    /// Remembers where the composition is, for this and later updates.
    pub fn set_anchor(&mut self, parent: HWND, composition: RECT) {
        self.anchor = Some(Anchor {
            parent,
            composition,
        });
    }

    pub fn clear_anchor(&mut self) {
        self.anchor = None;
    }

    pub fn hide(&mut self) {
        if let Some(window) = self.window.filter(|_| self.visible) {
            unsafe {
                let _ = ShowWindow(window, SW_HIDE);
                NotifyWinEvent(EVENT_OBJECT_IME_HIDE, window, OBJID_CLIENT.0, CHILDID_SELF);
            }
            self.data.raise_automation_event(UIA_MenuClosedEventId);
            let mut surface = self.data.surface.borrow_mut();
            surface.hover = None;
            surface.pressed = None;
        }
        self.visible = false;
        self.placed = None;
        self.width = 0;
    }

    /// Shows `view` at the composition, updating the open window in place.
    pub fn show(&mut self, view: &CandidateView) -> Result<()> {
        let Some(anchor) = self.anchor else {
            return Ok(());
        };
        let window = match self.window {
            Some(window) => window,
            None => {
                let window = self.create(anchor.parent)?;
                self.window = Some(window);
                window
            }
        };
        // The window takes the DPI of the monitor it moves to only after the
        // move, so it is sized for the application window it belongs to.
        let dpi = [anchor.parent, window]
            .into_iter()
            .map(|owner| unsafe { GetDpiForWindow(owner) })
            .find(|dpi| *dpi > 0)
            .unwrap_or(DEFAULT_DPI);
        let opening = !self.visible;
        if opening {
            self.touch_keyboard = self.find_touch_keyboard();
            let mut surface = self.data.surface.borrow_mut();
            surface.theme = Theme::read();
            surface.apply_frame(window);
        }
        let model = Model::new(view);
        let (layout, changed) = {
            let mut surface = self.data.surface.borrow_mut();
            if surface.layout.dpi != dpi {
                self.width = 0;
            }
            // Typing often leaves the candidates as they are.
            if opening || surface.model != model || surface.layout.dpi != dpi {
                self.width = self.width.max(surface.measure(&model, dpi)?);
            }
            let layout = Layout {
                dpi,
                width: self.width,
                rows: model.rows(),
                footer: model.has_footer(),
            };
            let changed = surface.model != model || surface.layout != layout;
            surface.layout = layout;
            surface.model = model.clone();
            (layout, changed)
        };
        let selection_changed = {
            let mut list = self.data.list.borrow_mut();
            let selection = (list.items != model.items)
                || list.selected != view.selected_on_page()
                || list.selecting != view.selecting;
            list.items.clone_from(&model.items);
            list.cloud.clone_from(&model.cloud);
            list.selected = view.selected_on_page();
            list.selecting = view.selecting;
            list.row_height = layout.px(ROW_HEIGHT);
            list.padding = layout.px(PADDING);
            opening || selection
        };
        let size = SIZE {
            cx: layout.width,
            cy: layout.height(),
        };
        let gap = layout.px(GAP_TO_COMPOSITION);
        let corner = match work_area(anchor.composition) {
            Some(area) => place(
                anchor.composition,
                size,
                layout.text_left(),
                gap,
                area,
                self.touch_keyboard,
            ),
            None => POINT {
                x: anchor.composition.left - layout.text_left(),
                y: anchor.composition.bottom + gap,
            },
        };
        let placed = RECT {
            left: corner.x,
            top: corner.y,
            right: corner.x + size.cx,
            bottom: corner.y + size.cy,
        };
        let moved = self.placed != Some(placed);
        unsafe {
            if opening {
                SetWindowPos(
                    window,
                    Some(HWND_TOPMOST),
                    placed.left,
                    placed.top,
                    size.cx,
                    size.cy,
                    SWP_NOACTIVATE,
                )?;
                // Paint before DWM shows the window, so that its first frame
                // is complete.
                let cloak = BOOL::from(true);
                let cloaked = DwmSetWindowAttribute(
                    window,
                    DWMWA_CLOAK,
                    (&raw const cloak).cast(),
                    size_of_val(&cloak) as u32,
                )
                .is_ok();
                let _ = ShowWindow(window, SW_SHOWNA);
                redraw(window);
                if cloaked {
                    let uncloak = BOOL::from(false);
                    let _ = DwmSetWindowAttribute(
                        window,
                        DWMWA_CLOAK,
                        (&raw const uncloak).cast(),
                        size_of_val(&uncloak) as u32,
                    );
                }
                NotifyWinEvent(EVENT_OBJECT_IME_SHOW, window, OBJID_CLIENT.0, CHILDID_SELF);
            } else if moved || changed {
                if moved {
                    SetWindowPos(
                        window,
                        None,
                        placed.left,
                        placed.top,
                        size.cx,
                        size.cy,
                        SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOCOPYBITS,
                    )?;
                }
                redraw(window);
                NotifyWinEvent(
                    EVENT_OBJECT_IME_CHANGE,
                    window,
                    OBJID_CLIENT.0,
                    CHILDID_SELF,
                );
            }
        }
        self.placed = Some(placed);
        self.visible = true;
        if opening {
            self.data.raise_automation_event(UIA_MenuOpenedEventId);
        }
        if selection_changed && view.selecting {
            self.data.raise_selection_event();
        }
        Ok(())
    }

    fn find_touch_keyboard(&mut self) -> Option<RECT> {
        let pane = self.input_pane.get_or_insert_with(|| unsafe {
            CoCreateInstance(&FrameworkInputPane, None, CLSCTX_INPROC_SERVER).ok()
        });
        let area = unsafe { pane.as_ref()?.Location() }.ok()?;
        (area.right > area.left && area.bottom > area.top).then_some(area)
    }

    fn create(&self, parent: HWND) -> Result<HWND> {
        register_class()?;
        let data = Rc::into_raw(self.data.clone());
        let created = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                &class_name(),
                w!(""),
                WS_POPUP,
                0,
                0,
                MIN_WIDTH,
                ROW_HEIGHT,
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
    use windows::Win32::{
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        },
        UI::Accessibility::{
            CUIAutomation, IUIAutomation, TreeScope_Children, UIA_ListControlTypeId,
        },
    };

    fn page() -> CandidateView {
        let mut texts = (0..12).map(|n| n.to_string()).collect::<Vec<_>>();
        texts.splice(9..12, ["今日".into(), "京\n".into(), "あ".repeat(60)]);
        CandidateView {
            texts,
            selected: 10,
            cloud: Vec::new(),
            selecting: true,
        }
    }

    #[test]
    fn lines_up_with_the_composition_below_it_unless_that_leaves_the_screen_or_covers_the_touch_keyboard()
     {
        let rect = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        let size = SIZE { cx: 300, cy: 200 };
        let screen = rect(0, 0, 1920, 1040);
        let composition = rect(100, 400, 180, 420);
        assert_eq!(
            place(composition, size, 36, 2, screen, None),
            POINT { x: 64, y: 422 }
        );
        let low = rect(100, 900, 180, 920);
        assert_eq!(
            place(low, size, 36, 2, screen, None),
            POINT { x: 64, y: 698 }
        );
        let keyboard = rect(0, 560, 1920, 1040);
        assert_eq!(
            place(composition, size, 36, 2, screen, Some(keyboard)),
            POINT { x: 64, y: 198 }
        );
        let narrow_keyboard = rect(1000, 560, 1920, 1040);
        assert_eq!(
            place(composition, size, 36, 2, screen, Some(narrow_keyboard)),
            POINT { x: 64, y: 422 }
        );
        let right_edge = rect(1800, 400, 1880, 420);
        assert_eq!(
            place(right_edge, size, 36, 2, screen, None),
            POINT { x: 1620, y: 422 }
        );
        let left_edge = rect(10, 400, 80, 420);
        assert_eq!(
            place(left_edge, size, 36, 2, screen, None),
            POINT { x: 0, y: 422 }
        );
        let second_monitor = rect(1920, 0, 3840, 1040);
        assert_eq!(
            place(
                rect(1800, 400, 1880, 420),
                size,
                36,
                2,
                second_monitor,
                None
            ),
            POINT { x: 1920, y: 422 }
        );
    }

    #[test]
    fn shortens_candidates_and_describes_the_page_below_them() {
        let page = page();
        let model = Model::new(&page);
        assert_eq!(
            model.items,
            ["今日".to_owned(), "京 ".into(), "あ".repeat(MAX_CHARACTERS)]
        );
        assert_eq!((model.selected, model.page, model.pages), (Some(1), 1, 2));
        assert_eq!(model.footer(), "2/2");
        assert_eq!(model.rows(), PAGE_SIZE);
        let short = CandidateView {
            texts: vec!["今日".into(), "今日は晴れ".into()],
            selected: 0,
            cloud: vec![false, true],
            selecting: false,
        };
        let model = Model::new(&short);
        assert_eq!((model.selected, model.rows()), (None, 2));
        assert_eq!(model.footer(), CLOUD_LABEL);
        let single = Model::new(&CandidateView {
            cloud: vec![false],
            texts: vec!["今日".into()],
            ..short
        });
        assert!(!single.has_footer());
    }

    #[test]
    fn finds_rows_and_page_buttons_under_the_pointer() {
        let model = Model::new(&page());
        let layout = Layout {
            dpi: 144,
            width: 300,
            rows: model.rows(),
            footer: model.has_footer(),
        };
        let row = layout.row(2);
        assert_eq!(
            layout.hit(&model, row.left + 1, row.top + 1),
            Some(Target::Row(2))
        );
        // Rows without a candidate on the last page do nothing.
        let empty = layout.row(5);
        assert_eq!(layout.hit(&model, empty.left + 1, empty.top + 1), None);
        let next = layout.button(true);
        assert_eq!(
            layout.hit(&model, next.left + 1, next.top + 1),
            Some(Target::NextPage)
        );
        assert_eq!(layout.height(), (4 * 2 + 32 * 9 + 32) * 3 / 2);
    }

    /// Writes what the window draws in several states as bitmaps into
    /// `SIMEJI_PREVIEW_DIR`, for checking the look by eye.
    #[test]
    #[ignore = "writes images for a visual check"]
    fn renders_a_preview() -> Result<()> {
        let Some(directory) = std::env::var_os("SIMEJI_PREVIEW_DIR") else {
            return Ok(());
        };
        let typing = CandidateView {
            texts: [
                "こんにちは",
                "こんにちは(*^▽^)/",
                "こんにちは😊✨",
                "こんにちはー🐰",
                "今日は",
                "こんにちは(o´ω`o)ﾉ",
                "😊",
                "こんにちは〜",
                "コンニチハ",
                "こんにちわ",
                "今日わ",
            ]
            .map(String::from)
            .to_vec(),
            selected: 0,
            cloud: [
                false, true, true, true, false, true, true, true, false, true, true,
            ]
            .to_vec(),
            selecting: false,
        };
        let converting = CandidateView {
            texts: ["今日", "京", "強", "教", "協", "共", "経", "興", "凶", "叫"]
                .map(String::from)
                .to_vec(),
            selected: 2,
            cloud: Vec::new(),
            selecting: true,
        };
        let single = CandidateView {
            texts: vec!["日本語".into()],
            selected: 0,
            cloud: Vec::new(),
            selecting: false,
        };
        let states = [
            ("typing-light", &typing, None, false, 96),
            ("typing-dark", &typing, Some(Target::Row(4)), true, 96),
            (
                "converting-light",
                &converting,
                Some(Target::NextPage),
                false,
                96,
            ),
            ("converting-dark-150", &converting, None, true, 144),
            ("single-light", &single, None, false, 96),
        ];
        for (name, view, hover, dark, dpi) in states {
            let mut surface = Surface {
                theme: Theme::colors(dark, if dark { 0x60cdff } else { 0x005fb8 }),
                hover,
                ..Surface::default()
            };
            let model = Model::new(view);
            let width = surface.measure(&model, dpi)?;
            surface.layout = Layout {
                dpi,
                width,
                rows: model.rows(),
                footer: model.has_footer(),
            };
            surface.model = model;
            let (width, height) = (surface.layout.width, surface.layout.height());
            let mut pixels = std::ptr::null_mut();
            let header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let bytes = unsafe {
                let context = CreateCompatibleDC(None);
                let bitmap =
                    CreateDIBSection(Some(context), &header, DIB_RGB_COLORS, &mut pixels, None, 0)?;
                let previous = SelectObject(context, bitmap.into());
                let client = RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                };
                surface.render(context, &client)?;
                let bytes =
                    std::slice::from_raw_parts(pixels.cast::<u8>(), (width * height * 4) as usize)
                        .to_vec();
                SelectObject(context, previous);
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(context);
                bytes
            };
            let mut file = Vec::new();
            let size = 54 + bytes.len() as u32;
            file.extend_from_slice(b"BM");
            file.extend_from_slice(&size.to_le_bytes());
            file.extend_from_slice(&[0; 4]);
            file.extend_from_slice(&54u32.to_le_bytes());
            file.extend_from_slice(&40u32.to_le_bytes());
            file.extend_from_slice(&width.to_le_bytes());
            file.extend_from_slice(&(-height).to_le_bytes());
            file.extend_from_slice(&1u16.to_le_bytes());
            file.extend_from_slice(&32u16.to_le_bytes());
            file.extend_from_slice(&[0; 24]);
            file.extend_from_slice(&bytes);
            std::fs::write(
                std::path::Path::new(&directory).join(format!("{name}.bmp")),
                file,
            )
            .unwrap();
        }
        Ok(())
    }

    /// UI Automation must not query a window from the thread that owns it,
    /// so the client runs on another thread while this one pumps messages.
    #[test]
    fn exposes_the_candidates_to_ui_automation() -> Result<()> {
        let mut window = CandidateWindow::default();
        window.set_anchor(unsafe { GetDesktopWindow() }, RECT::default());
        window.show(&page())?;
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
