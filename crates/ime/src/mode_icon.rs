//! Input mode icons for the Windows input indicator, drawn at run time: a
//! white glyph with a 1 px black outline at 50 % opacity, as Microsoft's
//! IME icon guidelines describe.

use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::WindowsAndMessaging::{
            CreateIconIndirect, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
        },
    },
    core::*,
};

const GLYPH: u32 = 0x00ff_ffff;
const OUTLINE: u32 = 0x8000_0000;

/// ARGB pixels from the glyph's coverage: white where the glyph is, the
/// outline around it, transparent elsewhere.
fn pixels(coverage: &[u8], size: usize) -> Vec<u32> {
    let covered = |x: isize, y: isize| {
        (0..size as isize).contains(&x)
            && (0..size as isize).contains(&y)
            && coverage[y as usize * size + x as usize] > 0
    };
    (0..size * size)
        .map(|index| {
            let (x, y) = ((index % size) as isize, (index / size) as isize);
            let alpha = u32::from(coverage[index]);
            if alpha > 0 {
                GLYPH | (alpha << 24)
            } else if (-1..=1).any(|dy| (-1..=1).any(|dx| covered(x + dx, y + dy))) {
                OUTLINE
            } else {
                0
            }
        })
        .collect()
}

/// Draws `glyph` in a small icon. The caller owns the icon.
pub(crate) fn mode_icon(glyph: &str) -> Result<HICON> {
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(Some(screen));
        ReleaseDC(None, screen);
        let header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(Some(memory), &header, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let previous_bitmap = SelectObject(memory, color.into());
        let font = CreateFontW(
            -(size * 7 / 8),
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Yu Gothic UI"),
        );
        let previous_font = SelectObject(memory, font.into());
        SetBkMode(memory, TRANSPARENT);
        SetTextColor(memory, COLORREF(0x00ff_ffff));
        let mut area = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        let mut text = glyph.encode_utf16().collect::<Vec<_>>();
        DrawTextW(
            memory,
            &mut text,
            &mut area,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        let _ = GdiFlush();
        let count = (size * size) as usize;
        let drawn = std::slice::from_raw_parts_mut(bits.cast::<u32>(), count);
        let coverage = drawn
            .iter()
            .map(|pixel| (pixel & 0xff) as u8)
            .collect::<Vec<_>>();
        drawn.copy_from_slice(&pixels(&coverage, size as usize));
        SelectObject(memory, previous_font);
        SelectObject(memory, previous_bitmap);
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(memory);
        let mask_row_bytes = (size as usize).div_ceil(16) * 2;
        let mask_bits = vec![0u8; mask_row_bytes * size as usize];
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr().cast()));
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outlines_the_glyph_and_keeps_the_rest_transparent() {
        let mut coverage = vec![0u8; 25];
        coverage[12] = 255;
        let pixels = pixels(&coverage, 5);
        assert_eq!(pixels[12], 0xffff_ffff);
        for neighbour in [6, 7, 8, 11, 13, 16, 17, 18] {
            assert_eq!(pixels[neighbour], OUTLINE);
        }
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[24], 0);
    }

    #[test]
    fn draws_an_icon() {
        let icon = mode_icon("あ").unwrap();
        assert!(!icon.is_invalid());
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(icon);
        }
    }
}
