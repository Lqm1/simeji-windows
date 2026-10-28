//! Input scopes whose text must not leave the field: no cloud lookups and no learning.

use windows::{
    Win32::{System::Com::CoTaskMemFree, UI::TextServices::*},
    core::*,
};

/// Scopes of password, PIN and private fields.
const PRIVATE_SCOPES: [InputScope; 6] = [
    IS_PASSWORD,
    IS_PRIVATE,
    IS_NUMERIC_PASSWORD,
    IS_NUMERIC_PIN,
    IS_ALPHANUMERIC_PIN,
    IS_ALPHANUMERIC_PIN_SET,
];
/// More scopes than any real field declares; treated as unreadable.
const MAX_SCOPES: u32 = 64;

/// Whether text typed into `range` may be sent to the cloud and learned.
/// Private fields and fields whose scope cannot be read are not.
pub(crate) fn is_public_field(context: &ITfContext, cookie: u32, range: &ITfRange) -> bool {
    scopes_allow(context, cookie, range).unwrap_or(false)
}

fn scopes_allow(context: &ITfContext, cookie: u32, range: &ITfRange) -> Result<bool> {
    unsafe {
        let value = context
            .GetProperty(&GUID_PROP_INPUTSCOPE)?
            .GetValue(cookie, range)?;
        if value.is_empty() {
            return Ok(true);
        }
        let scope: ITfInputScope = IUnknown::try_from(&value)?.cast()?;
        let mut scopes = std::ptr::null_mut();
        let mut count = 0;
        scope.GetInputScopes(&mut scopes, &mut count)?;
        let allowed = if count == 0 {
            true
        } else if scopes.is_null() || count > MAX_SCOPES {
            false
        } else {
            !std::slice::from_raw_parts(scopes, count as usize)
                .iter()
                .any(|scope| PRIVATE_SCOPES.contains(scope))
        };
        CoTaskMemFree(Some(scopes.cast()));
        Ok(allowed)
    }
}
