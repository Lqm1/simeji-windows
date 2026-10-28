//! Links the branding icon into the DLL and marks its COM exports PRIVATE.

use std::{env, fs, path::PathBuf};

/// COM looks these up with `GetProcAddress`; marking them PRIVATE keeps
/// them out of the import library, as the linker expects for COM exports.
const COM_EXPORTS: [&str; 4] = [
    "DllCanUnloadNow",
    "DllGetClassObject",
    "DllRegisterServer",
    "DllUnregisterServer",
];
/// The app logo, including the six sizes required by the input indicator.
const BRAND_ICON: &[u8] = include_bytes!("../../assets/branding/simeji.ico");

const ICON_GROUP_ID: u16 = 1;
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const MOVEABLE_DISCARDABLE: u16 = 0x1010;
const MOVEABLE_PURE_DISCARDABLE: u16 = 0x1030;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/branding/simeji.ico");
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
    let count = u16::from_le_bytes(BRAND_ICON[4..6].try_into().unwrap());
    let mut group = Vec::new();
    for value in [0u16, 1, count] {
        group.extend(value.to_le_bytes());
    }
    for index in 0..usize::from(count) {
        let entry = &BRAND_ICON[6 + index * 16..6 + (index + 1) * 16];
        let length = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        let id = index as u16 + 1;
        group.extend(&entry[..12]);
        group.extend(id.to_le_bytes());
        file.extend(resource(
            RT_ICON,
            id,
            MOVEABLE_DISCARDABLE,
            &BRAND_ICON[offset..offset + length],
        ));
    }
    file.extend(resource(
        RT_GROUP_ICON,
        ICON_GROUP_ID,
        MOVEABLE_PURE_DISCARDABLE,
        &group,
    ));
    file
}
