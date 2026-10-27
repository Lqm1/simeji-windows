//! `<string.h>`, `<wchar.h>` and `<stdlib.h>` functions on guest memory.

use super::{read_bytes, transfer_size};
use crate::{
    EngineError,
    arm::{Guest, read_c_string, read_u32, write_u32},
};
use std::cmp::Ordering;

const WCHAR_SIZE: u32 = 4;
const MAX_WIDE_STRING_CHARS: u32 = 1024 * 1024;
const MAX_UTF8_SEQUENCE: u32 = 4;

pub(super) fn copy(
    guest: &mut Guest<'_>,
    destination: u32,
    source: u32,
    length: u32,
) -> Result<u32, EngineError> {
    let bytes = read_bytes(guest, source, length)?;
    guest.mem_write(destination.into(), &bytes)?;
    Ok(destination)
}

pub(super) fn fill(
    guest: &mut Guest<'_>,
    destination: u32,
    byte: u8,
    length: u32,
) -> Result<u32, EngineError> {
    let length = transfer_size(length, 1)?;
    guest.mem_write(destination.into(), &vec![byte; length])?;
    Ok(destination)
}

pub(super) fn length(guest: &Guest<'_>, address: u32) -> Result<u32, EngineError> {
    Ok(read_c_string(guest, address)?.len() as u32)
}

pub(super) fn wide_length(guest: &Guest<'_>, address: u32) -> Result<u32, EngineError> {
    let mut length = 0;
    while read_u32(guest, address + length * WCHAR_SIZE)? != 0 {
        length += 1;
        if length > MAX_WIDE_STRING_CHARS {
            return Err(EngineError::new("Unterminated wide string"));
        }
    }
    Ok(length)
}

pub(super) fn copy_wide(
    guest: &mut Guest<'_>,
    destination: u32,
    source: u32,
    count: u32,
) -> Result<u32, EngineError> {
    let length = transfer_size(count, WCHAR_SIZE)?;
    copy(guest, destination, source, length as u32)
}

/// `memcmp` and `wmemcmp`, comparing bytes.
pub(super) fn compare_memory(
    guest: &Guest<'_>,
    left: u32,
    right: u32,
    count: u32,
    unit: u32,
) -> Result<u32, EngineError> {
    let length = transfer_size(count, unit)? as u32;
    Ok(ordering(
        &read_bytes(guest, left, length)?,
        &read_bytes(guest, right, length)?,
    ))
}

/// `strcmp`, `strncmp` with a `limit` and `strcasecmp` with `ignore_case`.
pub(super) fn compare_strings(
    guest: &Guest<'_>,
    left: u32,
    right: u32,
    limit: Option<u32>,
    ignore_case: bool,
) -> Result<u32, EngineError> {
    let mut left = read_c_string(guest, left)?;
    let mut right = read_c_string(guest, right)?;
    if ignore_case {
        left.make_ascii_lowercase();
        right.make_ascii_lowercase();
    }
    if let Some(limit) = limit {
        left.truncate(limit as usize);
        right.truncate(limit as usize);
    }
    Ok(ordering(&left, &right))
}

fn ordering(left: &[u8], right: &[u8]) -> u32 {
    match left.cmp(right) {
        Ordering::Less => u32::MAX,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// `strcpy`, or `strncpy` padding or cutting the copy to `limit` bytes.
pub(super) fn copy_string(
    guest: &mut Guest<'_>,
    destination: u32,
    source: u32,
    limit: Option<u32>,
) -> Result<u32, EngineError> {
    let mut bytes = read_c_string(guest, source)?;
    bytes.push(0);
    if let Some(limit) = limit {
        bytes.resize(transfer_size(limit, 1)?, 0);
    }
    guest.mem_write(destination.into(), &bytes)?;
    Ok(destination)
}

/// `strchr`, `strrchr` (`last`) or, with a `length`, `memchr`.
pub(super) fn find_byte(
    guest: &Guest<'_>,
    address: u32,
    byte: u32,
    length: Option<u32>,
    last: bool,
) -> Result<u32, EngineError> {
    let bytes = match length {
        Some(length) => read_bytes(guest, address, length)?,
        None => {
            let mut bytes = read_c_string(guest, address)?;
            bytes.push(0);
            bytes
        }
    };
    let byte = byte as u8;
    let index = if last {
        bytes.iter().rposition(|candidate| *candidate == byte)
    } else {
        bytes.iter().position(|candidate| *candidate == byte)
    };
    Ok(index.map_or(0, |index| address + index as u32))
}

pub(super) fn find_wide(
    guest: &Guest<'_>,
    address: u32,
    character: u32,
    count: u32,
) -> Result<u32, EngineError> {
    transfer_size(count, WCHAR_SIZE)?;
    for index in 0..count {
        let position = address
            .checked_add(index * WCHAR_SIZE)
            .ok_or_else(|| EngineError::new("Wide character range overflow"))?;
        if read_u32(guest, position)? == character {
            return Ok(position);
        }
    }
    Ok(0)
}

pub(super) fn parse_integer(guest: &Guest<'_>, address: u32) -> Result<u32, EngineError> {
    let bytes = read_c_string(guest, address)?;
    Ok(String::from_utf8_lossy(&bytes)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .parse::<i32>()
        .unwrap_or(0) as u32)
}

/// `mbtowc` for UTF-8: the sequence length, 0 for NUL or -1 when incomplete.
pub(super) fn decode_utf8(
    guest: &mut Guest<'_>,
    output: u32,
    input: u32,
    available: u32,
) -> Result<u32, EngineError> {
    if input == 0 {
        return Ok(0);
    }
    let bytes = read_bytes(guest, input, available.min(MAX_UTF8_SEQUENCE))?;
    let width = match bytes.first() {
        Some(0) => 0,
        Some(0..=0x7f) => 1,
        Some(0xc2..=0xdf) => 2,
        Some(0xe0..=0xef) => 3,
        Some(0xf0..=0xf4) => 4,
        _ => return Err(EngineError::new("Invalid UTF-8 sequence")),
    };
    if width > bytes.len() {
        return Ok(u32::MAX);
    }
    if width == 0 {
        return Ok(0);
    }
    let character = std::str::from_utf8(&bytes[..width])
        .map_err(|_| EngineError::new("Invalid UTF-8 sequence"))?
        .chars()
        .next()
        .ok_or_else(|| EngineError::new("Empty UTF-8 character"))?;
    if output != 0 {
        write_u32(guest, output, character as u32)?;
    }
    Ok(width as u32)
}
