//! Links the branding icon into the DLL and marks its COM exports PRIVATE.

use std::{env, fs, path::PathBuf};
use windows::{
    Win32::{Foundation::*, Graphics::Gdi::*},
    core::w,
};

/// COM looks these up with `GetProcAddress`; marking them PRIVATE keeps
/// them out of the import library, as the linker expects for COM exports.
const COM_EXPORTS: [&str; 4] = [
    "DllCanUnloadNow",
    "DllGetClassObject",
    "DllRegisterServer",
    "DllUnregisterServer",
];
/// The sizes Microsoft's IME icon guidelines ask for.
const ICON_SIZES: [i32; 6] = [16, 20, 24, 32, 40, 48];
const BRAND_GLYPH: &str = "S";
const ICON_GROUP_ID: u16 = 1;
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const MOVEABLE_DISCARDABLE: u16 = 0x1010;
const MOVEABLE_PURE_DISCARDABLE: u16 = 0x1030;
/// 1 px border of black at 50 % opacity around the white box.
const BORDER: u32 = 0x8000_0000;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    for export in COM_EXPORTS {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:{export},PRIVATE");
    }
    let resources =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set")).join("branding.res");
    fs::write(&resources, resource_file()).expect("the branding resources can be written");
    println!("cargo:rustc-cdylib-link-arg={}", resources.display());
}

/// The glyph's coverage, drawn with GDI at `size` pixels.
fn coverage(size: i32) -> Vec<u8> {
    unsafe {
        let memory = CreateCompatibleDC(None);
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
        let bitmap = CreateDIBSection(Some(memory), &header, DIB_RGB_COLORS, &mut bits, None, 0)
            .expect("a bitmap can be created");
        let previous_bitmap = SelectObject(memory, bitmap.into());
        let font = CreateFontW(
            -(size * 3 / 4),
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
            w!("Segoe UI"),
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
        let mut text = BRAND_GLYPH.encode_utf16().collect::<Vec<_>>();
        DrawTextW(
            memory,
            &mut text,
            &mut area,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        let _ = GdiFlush();
        let pixels = std::slice::from_raw_parts(bits.cast::<u32>(), (size * size) as usize);
        let coverage = pixels.iter().map(|pixel| (pixel & 0xff) as u8).collect();
        SelectObject(memory, previous_font);
        SelectObject(memory, previous_bitmap);
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        coverage
    }
}

/// A white box with a 50 % black border and the black glyph, top-down ARGB.
fn brand_pixels(size: usize, coverage: &[u8]) -> Vec<u32> {
    (0..size * size)
        .map(|index| {
            let (x, y) = (index % size, index / size);
            if x == 0 || y == 0 || x == size - 1 || y == size - 1 {
                return BORDER;
            }
            let shade = 255 - u32::from(coverage[index]);
            0xff00_0000 | (shade << 16) | (shade << 8) | shade
        })
        .collect()
}

/// An `RT_ICON` image: a 32-bit DIB of double height, bottom-up, followed
/// by an empty AND mask since the alpha channel defines transparency.
fn icon_image(size: i32) -> Vec<u8> {
    let side = size as usize;
    let pixels = brand_pixels(side, &coverage(size));
    let mask_row = side.div_ceil(32) * 4;
    let mut image = Vec::new();
    for value in [40u32, size as u32, 2 * size as u32] {
        image.extend(value.to_le_bytes());
    }
    image.extend(1u16.to_le_bytes());
    image.extend(32u16.to_le_bytes());
    image.extend(0u32.to_le_bytes());
    image.extend(((side * side * 4 + mask_row * side) as u32).to_le_bytes());
    image.extend([0u8; 16]);
    for row in pixels.chunks(side).rev() {
        for pixel in row {
            image.extend(pixel.to_le_bytes());
        }
    }
    image.extend(vec![0u8; mask_row * side]);
    image
}

fn resource(kind: u16, id: u16, flags: u16, data: &[u8]) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend((data.len() as u32).to_le_bytes());
    entry.extend(32u32.to_le_bytes());
    for value in [0xffff, kind, 0xffff, id] {
        entry.extend(value.to_le_bytes());
    }
    entry.extend(0u32.to_le_bytes());
    entry.extend(flags.to_le_bytes());
    entry.extend(0u16.to_le_bytes());
    entry.extend(0u32.to_le_bytes());
    entry.extend(0u32.to_le_bytes());
    entry.extend(data);
    entry.resize(entry.len().next_multiple_of(4), 0);
    entry
}

/// A compiled resource file: the leading empty entry, one `RT_ICON` per
/// size and the `RT_GROUP_ICON` that lists them.
fn resource_file() -> Vec<u8> {
    let mut file = vec![0u8; 32];
    file[4] = 32;
    file[8..12].copy_from_slice(&[0xff, 0xff, 0, 0]);
    file[12..16].copy_from_slice(&[0xff, 0xff, 0, 0]);
    let mut group = Vec::new();
    for value in [0u16, 1, ICON_SIZES.len() as u16] {
        group.extend(value.to_le_bytes());
    }
    for (index, size) in ICON_SIZES.into_iter().enumerate() {
        let id = index as u16 + 1;
        let image = icon_image(size);
        group.extend([size as u8, size as u8, 0, 0]);
        group.extend(1u16.to_le_bytes());
        group.extend(32u16.to_le_bytes());
        group.extend((image.len() as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
        file.extend(resource(RT_ICON, id, MOVEABLE_DISCARDABLE, &image));
    }
    file.extend(resource(
        RT_GROUP_ICON,
        ICON_GROUP_ID,
        MOVEABLE_PURE_DISCARDABLE,
        &group,
    ));
    file
}
