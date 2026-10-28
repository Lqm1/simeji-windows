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

#[cfg(test)]
const OUTLINE: u32 = 0x8000_0000;

/// Fit the visible ink, rather than the font's line box, with a one-pixel
/// margin. Averaging the large glyph keeps curves smooth at tray sizes.
fn fit_coverage(source: &[u8], side: usize, size: usize) -> Vec<u8> {
    let mut left = side;
    let mut top = side;
    let (mut right, mut bottom) = (0, 0);
    for (i, &alpha) in source.iter().enumerate() {
        if alpha > 0 {
            left = left.min(i % side);
            top = top.min(i / side);
            right = right.max(i % side + 1);
            bottom = bottom.max(i / side + 1);
        }
    }
    let mut result = vec![0; size * size];
    if left == side {
        return result;
    }
    let scale = (size - 2) as f64 / (right - left).max(bottom - top) as f64;
    let width = (right - left) as f64 * scale;
    let height = (bottom - top) as f64 * scale;
    let offset_x = (size as f64 - width) / 2.0;
    let offset_y = (size as f64 - height) / 2.0;
    for y in 0..size {
        for x in 0..size {
            let mut sum = 0u32;
            // Sample an 8 by 8 grid per output pixel, including transparent
            // space at its edges, so fractional centering stays symmetric.
            for sy in 0..8 {
                for sx in 0..8 {
                    let px = (x as f64 + (sx as f64 + 0.5) / 8.0 - offset_x) / scale;
                    let py = (y as f64 + (sy as f64 + 0.5) / 8.0 - offset_y) / scale;
                    if px >= 0.0
                        && py >= 0.0
                        && px < (right - left) as f64
                        && py < (bottom - top) as f64
                    {
                        sum += u32::from(source[(top + py as usize) * side + left + px as usize]);
                    }
                }
            }
            result[y * size + x] = ((sum + 32) / 64) as u8;
        }
    }
    result
}

/// ARGB pixels from the glyph's coverage: white where the glyph is, the
/// outline around it, transparent elsewhere.
fn pixels(coverage: &[u8], size: usize) -> Vec<u32> {
    let covered = |x: isize, y: isize| {
        if (0..size as isize).contains(&x) && (0..size as isize).contains(&y) {
            u32::from(coverage[y as usize * size + x as usize])
        } else {
            0
        }
    };
    (0..size * size)
        .map(|index| {
            let (x, y) = ((index % size) as isize, (index / size) as isize);
            let alpha = u32::from(coverage[index]);
            let edge = (-1..=1)
                .flat_map(|dy| (-1..=1).map(move |dx| covered(x + dx, y + dy)))
                .max()
                .unwrap_or(0);
            let outline = (edge * 128 / 255) * (255 - alpha) / 255;
            let opacity = alpha + outline;
            let shade = (alpha * 255).checked_div(opacity).unwrap_or(0);
            (opacity << 24) | (shade << 16) | (shade << 8) | shade
        })
        .collect()
}

/// Draws `glyph` in a small icon. The caller owns the icon.
pub(crate) fn mode_icon(glyph: &str) -> Result<HICON> {
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        let render_size = size * 4;
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(Some(screen));
        ReleaseDC(None, screen);
        let header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: render_size,
                biHeight: -render_size,
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
            -render_size,
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
            ANTIALIASED_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Meiryo"),
        );
        let previous_font = SelectObject(memory, font.into());
        SetBkMode(memory, TRANSPARENT);
        SetTextColor(memory, COLORREF(0x00ff_ffff));
        let mut area = RECT {
            left: 0,
            top: 0,
            right: render_size,
            bottom: render_size,
        };
        let mut text = glyph.encode_utf16().collect::<Vec<_>>();
        DrawTextW(
            memory,
            &mut text,
            &mut area,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        let _ = GdiFlush();
        let count = (render_size * render_size) as usize;
        let drawn = std::slice::from_raw_parts_mut(bits.cast::<u32>(), count);
        let coverage = drawn
            .iter()
            .map(|pixel| (pixel & 0xff) as u8)
            .collect::<Vec<_>>();
        let coverage = fit_coverage(&coverage, render_size as usize, size as usize);
        SelectObject(memory, previous_font);
        SelectObject(memory, previous_bitmap);
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(memory);
        let _ = DeleteObject(color.into());
        let mut header = header;
        header.bmiHeader.biWidth = size;
        header.bmiHeader.biHeight = -size;
        let color = CreateDIBSection(None, &header, DIB_RGB_COLORS, &mut bits, None, 0)?;
        std::slice::from_raw_parts_mut(bits.cast::<u32>(), (size * size) as usize)
            .copy_from_slice(&pixels(&coverage, size as usize));
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
    fn fits_visible_ink_independently_of_font_padding() {
        let mut source = vec![0; 32 * 32];
        for y in 7..23 {
            for x in 3..11 {
                source[y * 32 + x] = 255;
            }
        }
        let fitted = fit_coverage(&source, 32, 16);
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(fitted[y * 16 + x], fitted[y * 16 + 15 - x]);
                assert_eq!(fitted[y * 16 + x], fitted[(15 - y) * 16 + x]);
            }
        }
        assert!(fitted[16..32].contains(&255));
        assert!(fitted[..16].iter().all(|&alpha| alpha == 0));
        assert_eq!(fit_coverage(&[0; 16], 4, 16), vec![0; 256]);
    }

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
