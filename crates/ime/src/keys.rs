//! Maps Windows virtual keys to the keys the composer handles.

use windows::Win32::{Foundation::LPARAM, UI::Input::KeyboardAndMouse::*};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    /// 半角/全角 or 漢字: switches Japanese input on and off.
    ToggleInputMode,
    Enter,
    Escape,
    Space {
        shift: bool,
    },
    Backspace,
    Delete,
    Left {
        shift: bool,
    },
    Right {
        shift: bool,
    },
    Up {
        shift: bool,
    },
    Down {
        shift: bool,
    },
    Home,
    End,
    /// A key that types a character. The number keys 1–9 also select the
    /// candidate with that number on the shown page.
    Printable {
        candidate_number: Option<usize>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub windows: bool,
}

impl Modifiers {
    /// Reads the modifier state of the key event being processed.
    pub fn current() -> Self {
        let down = |key: VIRTUAL_KEY| unsafe { GetKeyState(i32::from(key.0)) } < 0;
        Self {
            shift: down(VK_SHIFT),
            control: down(VK_CONTROL),
            alt: down(VK_MENU),
            windows: down(VK_LWIN) || down(VK_RWIN),
        }
    }
}

const fn vk_code(key: VIRTUAL_KEY) -> u32 {
    key.0 as u32
}

/// Returns the key for a virtual-key code. Shortcuts with Ctrl, Alt or the
/// Windows key belong to the application, except the input mode toggles.
pub(crate) fn classify(vk: u32, modifiers: Modifiers) -> Option<Key> {
    const KANJI: u32 = vk_code(VK_KANJI);
    const OEM_AUTO: u32 = vk_code(VK_OEM_AUTO);
    const OEM_ENLW: u32 = vk_code(VK_OEM_ENLW);
    const BACK: u32 = vk_code(VK_BACK);
    const DELETE: u32 = vk_code(VK_DELETE);
    const RETURN: u32 = vk_code(VK_RETURN);
    const ESCAPE: u32 = vk_code(VK_ESCAPE);
    const SPACE: u32 = vk_code(VK_SPACE);
    const LEFT: u32 = vk_code(VK_LEFT);
    const UP: u32 = vk_code(VK_UP);
    const RIGHT: u32 = vk_code(VK_RIGHT);
    const DOWN: u32 = vk_code(VK_DOWN);
    const HOME: u32 = vk_code(VK_HOME);
    const END: u32 = vk_code(VK_END);
    const DIGIT_0: u32 = vk_code(VK_0);
    const DIGIT_1: u32 = vk_code(VK_1);
    const DIGIT_9: u32 = vk_code(VK_9);
    const LETTER_A: u32 = vk_code(VK_A);
    const LETTER_Z: u32 = vk_code(VK_Z);
    // ;: =+ ,< -= .> /? @` on the JIS layout.
    const OEM_1: u32 = vk_code(VK_OEM_1);
    const OEM_3: u32 = vk_code(VK_OEM_3);
    // [{ \| ]} ^~ on the JIS layout.
    const OEM_4: u32 = vk_code(VK_OEM_4);
    const OEM_7: u32 = vk_code(VK_OEM_7);
    // \_ (ろ) on the JIS layout.
    const OEM_102: u32 = vk_code(VK_OEM_102);

    if matches!(vk, KANJI | OEM_AUTO | OEM_ENLW) {
        return Some(Key::ToggleInputMode);
    }
    if modifiers.control || modifiers.alt || modifiers.windows {
        return None;
    }
    let shift = modifiers.shift;
    Some(match vk {
        RETURN => Key::Enter,
        ESCAPE => Key::Escape,
        SPACE => Key::Space { shift },
        BACK => Key::Backspace,
        DELETE => Key::Delete,
        LEFT => Key::Left { shift },
        RIGHT => Key::Right { shift },
        UP => Key::Up { shift },
        DOWN => Key::Down { shift },
        HOME => Key::Home,
        END => Key::End,
        DIGIT_0..=DIGIT_9 | LETTER_A..=LETTER_Z | OEM_1..=OEM_3 | OEM_4..=OEM_7 | OEM_102 => {
            Key::Printable {
                candidate_number: (DIGIT_1..=DIGIT_9)
                    .contains(&vk)
                    .then(|| (vk - DIGIT_0) as usize),
            }
        }
        _ => return None,
    })
}

/// The text the key types under the active keyboard layout, if any.
pub(crate) fn typed_text(vk: u32, lparam: LPARAM) -> Option<String> {
    /// `ToUnicodeEx` flag: leave the keyboard state (dead keys) unchanged.
    const KEEP_KEYBOARD_STATE: u32 = 0x4;
    let scan_code = (lparam.0 as u32 >> 16) & 0xff;
    let mut keyboard = [0; 256];
    let mut output = [0u16; 8];
    let count = unsafe {
        GetKeyboardState(&mut keyboard).ok()?;
        ToUnicodeEx(
            vk,
            scan_code,
            &keyboard,
            &mut output,
            KEEP_KEYBOARD_STATE,
            Some(GetKeyboardLayout(0)),
        )
    };
    let count = usize::try_from(count).ok().filter(|count| *count > 0)?;
    String::from_utf16(&output[..count]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers {
        shift: false,
        control: false,
        alt: false,
        windows: false,
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NONE
    };

    #[test]
    fn toggles_input_even_with_modifiers() {
        let control = Modifiers {
            control: true,
            ..NONE
        };
        for vk in [0x19, 0xf3, 0xf4] {
            assert_eq!(classify(vk, NONE), Some(Key::ToggleInputMode));
            assert_eq!(classify(vk, control), Some(Key::ToggleInputMode));
        }
    }

    #[test]
    fn leaves_shortcuts_to_the_application() {
        for modifiers in [
            Modifiers {
                control: true,
                ..NONE
            },
            Modifiers { alt: true, ..NONE },
            Modifiers {
                windows: true,
                ..NONE
            },
        ] {
            assert_eq!(classify(u32::from(b'A'), modifiers), None);
            assert_eq!(classify(0x0d, modifiers), None);
        }
    }

    #[test]
    fn classifies_editing_keys_with_shift() {
        assert_eq!(classify(0x0d, NONE), Some(Key::Enter));
        assert_eq!(classify(0x1b, NONE), Some(Key::Escape));
        assert_eq!(classify(0x08, NONE), Some(Key::Backspace));
        assert_eq!(classify(0x2e, NONE), Some(Key::Delete));
        assert_eq!(classify(0x24, NONE), Some(Key::Home));
        assert_eq!(classify(0x23, NONE), Some(Key::End));
        assert_eq!(classify(0x20, SHIFT), Some(Key::Space { shift: true }));
        assert_eq!(
            [0x25, 0x26, 0x27, 0x28].map(|vk| classify(vk, NONE)),
            [
                Key::Left { shift: false },
                Key::Up { shift: false },
                Key::Right { shift: false },
                Key::Down { shift: false },
            ]
            .map(Some)
        );
        assert_eq!(classify(0x25, SHIFT), Some(Key::Left { shift: true }));
    }

    #[test]
    fn number_keys_one_to_nine_select_candidates() {
        assert_eq!(
            classify(u32::from(b'0'), NONE),
            Some(Key::Printable {
                candidate_number: None
            })
        );
        for number in 1..=9 {
            assert_eq!(
                classify(u32::from(b'0') + number, NONE),
                Some(Key::Printable {
                    candidate_number: Some(number as usize)
                })
            );
        }
    }

    #[test]
    fn letters_and_punctuation_are_printable() {
        for vk in [
            u32::from(b'A'),
            u32::from(b'Z'),
            0xba,
            0xc0,
            0xdb,
            0xde,
            0xe2,
        ] {
            assert_eq!(
                classify(vk, NONE),
                Some(Key::Printable {
                    candidate_number: None
                })
            );
        }
        for vk in [0x70, 0x21, 0x22, 0xc1] {
            assert_eq!(classify(vk, NONE), None);
        }
    }
}
