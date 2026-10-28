//! The `printf` and `sscanf` subset the engine uses.

use crate::{
    EngineError,
    arm::{Guest, read_c_string, read_u32, write_u32},
};

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_WIDTH: usize = 1024 * 1024;
const MAX_PRECISION: usize = 1024;
const DEFAULT_FLOAT_PRECISION: usize = 6;
const ARGUMENT_REGISTERS: usize = 4;
/// The only `sscanf` format: a dictionary line of three words and a number.
const DICTIONARY_LINE_FORMAT: &[u8] = b"%s%s%s%d";

/// Where the variable arguments of a call are.
pub(super) enum VarArgs {
    /// A `va_list` pointer.
    List(u32),
    /// The caller's registers R0–R3 and stack, starting at argument `next`.
    Call {
        registers: [u32; ARGUMENT_REGISTERS],
        stack: u32,
        next: usize,
    },
}

impl VarArgs {
    /// The next 32-bit argument, or a 64-bit one (`wide`) aligned to 8 bytes.
    fn next(&mut self, guest: &Guest<'_>, wide: bool) -> Result<u64, EngineError> {
        match self {
            Self::List(pointer) => {
                if wide {
                    *pointer = (*pointer + 7) & !7;
                }
                let low = read_u32(guest, *pointer)?;
                *pointer += 4;
                if !wide {
                    return Ok(u64::from(low));
                }
                let high = read_u32(guest, *pointer)?;
                *pointer += 4;
                Ok(u64::from(low) | (u64::from(high) << 32))
            }
            Self::Call {
                registers,
                stack,
                next,
            } => {
                if wide {
                    *next = (*next + 1) & !1;
                }
                let mut value = 0;
                for half in 0..if wide { 2 } else { 1 } {
                    let word = if *next < ARGUMENT_REGISTERS {
                        registers[*next]
                    } else {
                        read_u32(guest, *stack + ((*next - ARGUMENT_REGISTERS) * 4) as u32)?
                    };
                    value |= u64::from(word) << (half * 32);
                    *next += 1;
                }
                Ok(value)
            }
        }
    }
}

/// Formats like `printf` and returns the text without a terminator.
pub(super) fn print(
    guest: &Guest<'_>,
    format: u32,
    mut args: VarArgs,
) -> Result<Vec<u8>, EngineError> {
    let format = read_c_string(guest, format)?;
    let mut output = Vec::new();
    let mut index = 0;
    while index < format.len() {
        if format[index] != b'%' {
            output.push(format[index]);
            index += 1;
            continue;
        }
        index += 1;
        if format.get(index) == Some(&b'%') {
            output.push(b'%');
            index += 1;
            continue;
        }
        let mut left_aligned = false;
        let mut zero_padded = false;
        while let Some(flag) = format.get(index) {
            match flag {
                b'-' => left_aligned = true,
                b'0' => zero_padded = true,
                b' ' | b'+' | b'#' => {}
                _ => break,
            }
            index += 1;
        }
        let mut width = 0usize;
        if format.get(index) == Some(&b'*') {
            let value = args.next(guest, false)? as i32;
            left_aligned |= value < 0;
            width = (value.unsigned_abs() as usize).min(MAX_WIDTH);
            index += 1;
        }
        while let Some(digit @ b'0'..=b'9') = format.get(index) {
            width = (width * 10 + usize::from(digit - b'0')).min(MAX_WIDTH);
            index += 1;
        }
        let mut precision = None;
        if format.get(index) == Some(&b'.') {
            index += 1;
            if format.get(index) == Some(&b'*') {
                let value = args.next(guest, false)? as i32;
                index += 1;
                if value >= 0 {
                    precision = Some((value as usize).min(MAX_WIDTH));
                }
            } else {
                let mut value = 0;
                while let Some(digit @ b'0'..=b'9') = format.get(index) {
                    value = (value * 10 + usize::from(digit - b'0')).min(MAX_PRECISION);
                    index += 1;
                }
                precision = Some(value);
            }
        }
        let mut long_long = false;
        if format.get(index) == Some(&b'l') {
            index += 1;
            if format.get(index) == Some(&b'l') {
                long_long = true;
                index += 1;
            }
        } else if matches!(format.get(index), Some(b'z' | b't' | b'h')) {
            index += 1;
            if format.get(index) == Some(&b'h') {
                index += 1;
            }
        }
        let conversion = *format
            .get(index)
            .ok_or_else(|| EngineError::new("Truncated guest format"))?;
        index += 1;
        let value = args.next(guest, long_long || matches!(conversion, b'f' | b'g' | b'e'))?;
        let mut text = match conversion {
            b's' if value == 0 => b"(null)".to_vec(),
            b's' => {
                let mut text = read_c_string(guest, value as u32)?;
                if let Some(precision) = precision {
                    text.truncate(precision);
                }
                text
            }
            b'd' | b'i' if long_long => (value as i64).to_string().into_bytes(),
            b'd' | b'i' => (value as i32).to_string().into_bytes(),
            b'u' => value.to_string().into_bytes(),
            b'x' => format!("{value:x}").into_bytes(),
            b'X' => format!("{value:X}").into_bytes(),
            b'p' => format!("0x{value:x}").into_bytes(),
            b'c' => vec![value as u8],
            b'f' => format!(
                "{:.*}",
                precision.unwrap_or(DEFAULT_FLOAT_PRECISION),
                f64::from_bits(value)
            )
            .into_bytes(),
            _ => {
                return Err(EngineError::new(format!(
                    "Unsupported guest format specifier: {}",
                    char::from(conversion)
                )));
            }
        };
        if width > text.len() {
            let padding = vec![if zero_padded { b'0' } else { b' ' }; width - text.len()];
            if left_aligned {
                text.extend(padding);
            } else {
                text.splice(0..0, padding);
            }
        }
        output.extend(text);
        if output.len() > MAX_OUTPUT_BYTES {
            return Err(EngineError::new("Guest formatted output too large"));
        }
    }
    Ok(output)
}

/// `sscanf` with the dictionary line format, writing up to four fields to
/// `outputs` and returning how many it stored.
pub(super) fn scan(
    guest: &mut Guest<'_>,
    input: u32,
    format: u32,
    outputs: [u32; 4],
) -> Result<u32, EngineError> {
    let text = read_c_string(guest, input)?;
    if read_c_string(guest, format)? != DICTIONARY_LINE_FORMAT {
        return Err(EngineError::new("Unsupported scanf format"));
    }
    let text = String::from_utf8_lossy(&text);
    let mut stored = 0;
    for (index, token) in text.split_whitespace().take(outputs.len()).enumerate() {
        let output = outputs[index];
        if index == outputs.len() - 1 {
            let number = token
                .parse::<i32>()
                .map_err(|_| EngineError::new("Invalid dictionary integer"))?;
            write_u32(guest, output, number as u32)?;
        } else {
            guest.mem_write(output.into(), token.as_bytes())?;
            guest.mem_write(u64::from(output) + token.len() as u64, &[0])?;
        }
        stored += 1;
    }
    Ok(stored)
}
