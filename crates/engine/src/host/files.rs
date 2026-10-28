//! Guest file access, confined to the dictionary and learning directories.

use super::{read_bytes, transfer_size};
use crate::{
    EngineError,
    arm::{Guest, read_c_string, write_u32},
};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

/// Guest path of the read-only dictionary directory.
pub(crate) const DICTIONARY_ROOT: &str = "/dict/";
/// Guest path of the writable learning directory.
pub(crate) const LEARNING_ROOT: &str = "/learn/";
const FIRST_HANDLE: u32 = 3;
const FAILURE: u32 = u32::MAX;
const STAT_SIZE: usize = 104;
const STAT_MODE: u32 = 16;
const STAT_FILE_SIZE: u64 = 48;
const REGULAR_FILE_MODE: u32 = 0o100644;

pub(super) struct Files {
    dictionary: PathBuf,
    learning: PathBuf,
    open: BTreeMap<u32, File>,
    next_handle: u32,
}

impl Files {
    pub fn new(dictionary: &Path, learning: &Path) -> Result<Self, EngineError> {
        std::fs::create_dir_all(learning)?;
        Ok(Self {
            dictionary: dictionary.canonicalize()?,
            learning: learning.canonicalize()?,
            open: BTreeMap::new(),
            next_handle: FIRST_HANDLE,
        })
    }

    /// Maps a guest path to a file directly inside one of the two roots.
    fn resolve(&self, guest: &Guest<'_>, address: u32) -> Result<PathBuf, EngineError> {
        let bytes = read_c_string(guest, address)?;
        let path =
            std::str::from_utf8(&bytes).map_err(|_| EngineError::new("Invalid guest path"))?;
        let (root, name) = if let Some(name) = path.strip_prefix(DICTIONARY_ROOT) {
            (&self.dictionary, name)
        } else if let Some(name) = path.strip_prefix(LEARNING_ROOT) {
            (&self.learning, name)
        } else {
            return Err(EngineError::new(
                "Guest path outside dictionary and learning roots",
            ));
        };
        let name = name.trim_start_matches('/');
        if name.is_empty() || name.contains(['/', '\\', ':']) || name == "." || name == ".." {
            return Err(EngineError::new("Invalid guest filename"));
        }
        let resolved = root.join(name);
        if resolved.exists() && !resolved.canonicalize()?.starts_with(root) {
            return Err(EngineError::new("Guest path escaped its root"));
        }
        Ok(resolved)
    }

    fn resolve_learning(&self, guest: &Guest<'_>, address: u32) -> Result<PathBuf, EngineError> {
        let path = self.resolve(guest, address)?;
        if !path.starts_with(&self.learning) {
            return Err(EngineError::new("Dictionary assets are read-only"));
        }
        Ok(path)
    }

    fn file(&mut self, handle: u32) -> Result<&mut File, EngineError> {
        self.open
            .get_mut(&handle)
            .ok_or_else(|| EngineError::new("Invalid guest file handle"))
    }

    fn insert(&mut self, file: File) -> u32 {
        let handle = self.next_handle;
        self.next_handle += 1;
        self.open.insert(handle, file);
        handle
    }

    pub fn rename(&self, guest: &Guest<'_>, from: u32, to: u32) -> Result<u32, EngineError> {
        let from = self.resolve_learning(guest, from)?;
        let to = self.resolve_learning(guest, to)?;
        Ok(std::fs::rename(from, to).map_or(FAILURE, |()| 0))
    }

    pub fn remove(&self, guest: &Guest<'_>, path: u32) -> Result<u32, EngineError> {
        let path = self.resolve_learning(guest, path)?;
        Ok(std::fs::remove_file(path).map_or(FAILURE, |()| 0))
    }

    pub fn stat(&self, guest: &mut Guest<'_>, path: u32, buffer: u32) -> Result<u32, EngineError> {
        let metadata = std::fs::metadata(self.resolve(guest, path)?);
        write_stat(guest, buffer, metadata.ok())
    }

    pub fn fstat(
        &mut self,
        guest: &mut Guest<'_>,
        handle: u32,
        buffer: u32,
    ) -> Result<u32, EngineError> {
        let metadata = self.file(handle)?.metadata();
        write_stat(guest, buffer, metadata.ok())
    }

    /// `fopen`: the dictionary directory only opens for reading.
    pub fn fopen(&mut self, guest: &Guest<'_>, path: u32, mode: u32) -> Result<u32, EngineError> {
        let path = self.resolve(guest, path)?;
        let mode = read_c_string(guest, mode)?;
        let mode =
            std::str::from_utf8(&mode).map_err(|_| EngineError::new("Invalid fopen mode"))?;
        if path.starts_with(&self.dictionary) && mode != "r" && mode != "rb" {
            return Err(EngineError::new("Dictionary assets are read-only"));
        }
        let mut options = OpenOptions::new();
        match mode {
            "r" | "rb" => options.read(true),
            "w" | "wb" => options.write(true).create(true).truncate(true),
            "a" | "ab" => options.append(true).create(true),
            "r+" | "rb+" | "r+b" => options.read(true).write(true),
            _ => return Err(EngineError::new(format!("Unsupported fopen mode: {mode}"))),
        };
        Ok(options.open(path).map_or(0, |file| self.insert(file)))
    }

    /// `open` for reading only.
    pub fn open(&mut self, guest: &Guest<'_>, path: u32, flags: u32) -> Result<u32, EngineError> {
        let path = self.resolve(guest, path)?;
        if flags != 0 {
            return Err(EngineError::new("Unsupported open flags"));
        }
        Ok(File::open(path).map_or(FAILURE, |file| self.insert(file)))
    }

    pub fn close(&mut self, handle: u32) -> Result<u32, EngineError> {
        self.open
            .remove(&handle)
            .ok_or_else(|| EngineError::new("Invalid close"))?;
        Ok(0)
    }

    /// `fread`: returns the number of whole items read.
    pub fn read(
        &mut self,
        guest: &mut Guest<'_>,
        buffer: u32,
        item_size: u32,
        count: u32,
        handle: u32,
    ) -> Result<u32, EngineError> {
        let length = transfer_size(item_size, count)?;
        let mut bytes = vec![0; length];
        let file = self.file(handle)?;
        let mut total = 0;
        while total < length {
            let read = file.read(&mut bytes[total..])?;
            if read == 0 {
                break;
            }
            total += read;
        }
        guest.mem_write(buffer.into(), &bytes[..total])?;
        Ok((total as u32).checked_div(item_size).unwrap_or(0))
    }

    pub fn write(
        &mut self,
        guest: &Guest<'_>,
        buffer: u32,
        item_size: u32,
        count: u32,
        handle: u32,
    ) -> Result<u32, EngineError> {
        let length = transfer_size(item_size, count)?;
        let bytes = read_bytes(guest, buffer, length as u32)?;
        self.file(handle)?.write_all(&bytes)?;
        Ok(if item_size == 0 { 0 } else { count })
    }

    /// The bytes at `offset` for `mmap`, leaving the file position unchanged.
    pub fn read_at(
        &mut self,
        handle: u32,
        offset: u32,
        length: usize,
    ) -> Result<Vec<u8>, EngineError> {
        let file = self.file(handle)?;
        let position = file.stream_position()?;
        file.seek(SeekFrom::Start(offset.into()))?;
        let mut bytes = vec![0; length];
        file.read_exact(&mut bytes)?;
        file.seek(SeekFrom::Start(position))?;
        Ok(bytes)
    }

    pub fn tell(&mut self, handle: u32) -> Result<u32, EngineError> {
        u32::try_from(self.file(handle)?.stream_position()?)
            .map_err(|_| EngineError::new("Guest file offset overflow"))
    }

    pub fn seek(&mut self, handle: u32, offset: u32, whence: u32) -> Result<u32, EngineError> {
        let offset = i64::from(offset as i32);
        let from = match whence {
            0 => SeekFrom::Start(
                u64::try_from(offset).map_err(|_| EngineError::new("Negative file offset"))?,
            ),
            1 => SeekFrom::Current(offset),
            2 => SeekFrom::End(offset),
            _ => return Err(EngineError::new("Invalid seek mode")),
        };
        self.file(handle)?.seek(from)?;
        Ok(0)
    }

    pub fn flush(&mut self, handle: u32) -> Result<u32, EngineError> {
        self.file(handle)?.flush()?;
        Ok(0)
    }

    /// `fgetc`: the next byte, or `EOF` (-1).
    pub fn get_byte(&mut self, handle: u32) -> Result<u32, EngineError> {
        let mut byte = [0];
        Ok(if self.file(handle)?.read(&mut byte)? == 0 {
            FAILURE
        } else {
            u32::from(byte[0])
        })
    }

    /// `fgets`: one line of at most `capacity - 1` bytes, or null at end of file.
    pub fn get_line(
        &mut self,
        guest: &mut Guest<'_>,
        buffer: u32,
        capacity: u32,
        handle: u32,
    ) -> Result<u32, EngineError> {
        let capacity = transfer_size(capacity, 1)?;
        if capacity == 0 {
            return Err(EngineError::new("Invalid fgets length"));
        }
        let file = self.file(handle)?;
        let mut line = Vec::new();
        for _ in 1..capacity {
            let mut byte = [0];
            if file.read(&mut byte)? == 0 {
                break;
            }
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        if line.is_empty() {
            return Ok(0);
        }
        line.push(0);
        guest.mem_write(buffer.into(), &line)?;
        Ok(buffer)
    }
}

/// Fills a bionic ARM32 `struct stat` with a regular file of the given size.
fn write_stat(
    guest: &mut Guest<'_>,
    buffer: u32,
    metadata: Option<std::fs::Metadata>,
) -> Result<u32, EngineError> {
    let Some(metadata) = metadata else {
        return Ok(FAILURE);
    };
    guest.mem_write(buffer.into(), &[0; STAT_SIZE])?;
    write_u32(guest, buffer + STAT_MODE, REGULAR_FILE_MODE)?;
    guest.mem_write(
        u64::from(buffer) + STAT_FILE_SIZE,
        &metadata.len().to_le_bytes(),
    )?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{arm::ArmMachine, host::heap::HEAP_START};

    #[test]
    fn rejects_paths_outside_asset_and_learning_roots() {
        let files = Files::new(&std::env::temp_dir(), &std::env::temp_dir()).unwrap();
        let mut machine = ArmMachine::new().unwrap();
        machine.map(HEAP_START.into(), 4096 * 4).unwrap();
        for path in [
            b"/dict/../outside\0".as_slice(),
            b"/learn/C:\\outside\0",
            b"C:\\outside\0",
        ] {
            machine.write(HEAP_START.into(), path).unwrap();
            assert!(files.resolve(&machine.guest, HEAP_START).is_err());
        }
    }
}
