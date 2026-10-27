//! Single-threaded Android libc services for the conversion worker.

use crate::arm::ArmError;
use chrono::TimeZone;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use unicorn_engine::{RegisterARM, Unicorn};

type Guest<'a> = Unicorn<'a, ()>;
const HEAP: u32 = 0x1000_0000;
const HEAP_END: u32 = 0x2000_0000;
const MAX_TRANSFER: u32 = 64 * 1024 * 1024;

pub(crate) fn word(u: &Guest<'_>, p: u32) -> Result<u32, ArmError> {
    let mut b = [0; 4];
    u.mem_read(u64::from(p), &mut b)?;
    Ok(u32::from_le_bytes(b))
}

pub(crate) fn write_word(u: &mut Guest<'_>, p: u32, value: u32) -> Result<(), ArmError> {
    u.mem_write(u64::from(p), &value.to_le_bytes())?;
    Ok(())
}
fn bytes(u: &Guest<'_>, p: u32, n: u32) -> Result<Vec<u8>, ArmError> {
    if n > MAX_TRANSFER {
        return Err(ArmError::new("Guest transfer limit exceeded"));
    }
    Ok(u.mem_read_as_vec(u64::from(p), n as usize)?)
}
pub(crate) fn string(u: &Guest<'_>, p: u32) -> Result<Vec<u8>, ArmError> {
    if p == 0 {
        return Err(ArmError::new("Null guest string"));
    }
    let mut out = Vec::new();
    while out.len() < 1024 * 1024 {
        let address = u64::from(p) + out.len() as u64;
        let count = (4096 - (address as usize & 4095)).min(256);
        let block = u.mem_read_as_vec(address, count)?;
        if let Some(end) = block.iter().position(|b| *b == 0) {
            out.extend_from_slice(&block[..end]);
            return Ok(out);
        }
        out.extend_from_slice(&block);
    }
    Err(ArmError::new("Unterminated guest string"))
}

pub(crate) struct Host {
    cursor: u32,
    allocations: BTreeMap<u32, u32>,
    free: BTreeMap<u32, u32>,
    files: BTreeMap<u32, File>,
    next_file: u32,
    tls: BTreeMap<u32, u32>,
    next_key: u32,
    dict: PathBuf,
    learn: PathBuf,
    pub error: Option<ArmError>,
    pub unwind: Vec<(u32, u32, u32)>,
}
impl Host {
    pub fn new(dict: &Path, learn: &Path, unwind: Vec<(u32, u32, u32)>) -> Result<Self, ArmError> {
        std::fs::create_dir_all(learn)?;
        Ok(Self {
            cursor: HEAP + 4096,
            allocations: BTreeMap::new(),
            free: BTreeMap::new(),
            files: BTreeMap::new(),
            next_file: 3,
            tls: BTreeMap::new(),
            next_key: 1,
            dict: dict.canonicalize()?,
            learn: learn.canonicalize()?,
            error: None,
            unwind,
        })
    }
    pub fn alloc(&mut self, size: u32) -> Result<u32, ArmError> {
        let size = size
            .max(16)
            .checked_add(15)
            .map(|v| v & !15)
            .ok_or_else(|| ArmError::new("Guest allocation overflow"))?;
        let reusable = self
            .free
            .iter()
            .find(|(_, n)| **n >= size)
            .map(|(p, n)| (*p, *n));
        let p = if let Some((p, n)) = reusable {
            self.free.remove(&p);
            if n > size {
                self.free.insert(p + size, n - size);
            }
            p
        } else {
            let p = self.cursor;
            self.cursor = self
                .cursor
                .checked_add(size)
                .filter(|end| *end <= HEAP_END)
                .ok_or_else(|| ArmError::new("Guest heap exhausted"))?;
            p
        };
        self.allocations.insert(p, size);
        Ok(p)
    }
    pub fn release(&mut self, p: u32) -> Result<(), ArmError> {
        if p == 0 {
            return Ok(());
        }
        let mut size = self
            .allocations
            .remove(&p)
            .ok_or_else(|| ArmError::new(format!("Invalid guest free: {p:#x}")))?;
        let mut start = p;
        if let Some((&before, &n)) = self.free.range(..p).next_back()
            && before + n == p
        {
            start = before;
            size += n;
            self.free.remove(&before);
        }
        if let Some(n) = self.free.remove(&(start + size)) {
            size += n;
        }
        self.free.insert(start, size);
        Ok(())
    }
    pub fn put(&mut self, u: &mut Guest<'_>, b: &[u8]) -> Result<u32, ArmError> {
        let size =
            u32::try_from(b.len() + 1).map_err(|_| ArmError::new("Guest string too long"))?;
        let p = self.alloc(size)?;
        u.mem_write(u64::from(p), b)?;
        u.mem_write(u64::from(p) + b.len() as u64, &[0])?;
        Ok(p)
    }
    fn path(&self, u: &Guest<'_>, p: u32) -> Result<PathBuf, ArmError> {
        let b = string(u, p)?;
        let s = std::str::from_utf8(&b).map_err(|_| ArmError::new("Invalid guest path"))?;
        let (root, name) = if let Some(name) = s.strip_prefix("/dict/") {
            (&self.dict, name)
        } else if let Some(name) = s.strip_prefix("/learn/") {
            (&self.learn, name)
        } else {
            return Err(ArmError::new(
                "Guest path outside dictionary and learning roots",
            ));
        };
        let name = name.trim_start_matches('/');
        if name.is_empty() || name.contains(['/', '\\', ':']) || name == "." || name == ".." {
            return Err(ArmError::new("Invalid guest filename"));
        }
        let path = root.join(name);
        if path.exists() && !path.canonicalize()?.starts_with(root) {
            return Err(ArmError::new("Guest path escaped its root"));
        }
        Ok(path)
    }
    fn file(&mut self, id: u32) -> Result<&mut File, ArmError> {
        self.files
            .get_mut(&id)
            .ok_or_else(|| ArmError::new("Invalid guest file handle"))
    }
    fn transfer(n: u32, m: u32) -> Result<usize, ArmError> {
        n.checked_mul(m)
            .filter(|n| *n <= MAX_TRANSFER)
            .map(|n| n as usize)
            .ok_or_else(|| ArmError::new("Guest IO size limit exceeded"))
    }

    pub fn invoke(&mut self, u: &mut Guest<'_>, name: &str) -> Result<(), ArmError> {
        let mut a = [0u32; 12];
        for (i, r) in [
            RegisterARM::R0,
            RegisterARM::R1,
            RegisterARM::R2,
            RegisterARM::R3,
        ]
        .iter()
        .enumerate()
        {
            a[i] = u.reg_read(*r)? as u32;
        }
        if matches!(name, "mmap" | "sscanf") {
            let sp = u.reg_read(RegisterARM::SP)? as u32;
            for (i, value) in a.iter_mut().enumerate().take(6).skip(4) {
                *value = word(u, sp + 4 * (i as u32 - 4))?;
            }
        }
        let mut result = 0u32;
        match name {
            "malloc" => result = self.alloc(a[0])?,
            "calloc" => {
                let n = Self::transfer(a[0], a[1])?;
                result = self.alloc(n as u32)?;
                u.mem_write(u64::from(result), &vec![0; n])?;
            }
            "free" | "munmap" => self.release(a[0])?,
            "realloc" => {
                if a[1] == 0 {
                    self.release(a[0])?;
                } else {
                    result = self.alloc(a[1])?;
                    if a[0] != 0 {
                        let old = *self
                            .allocations
                            .get(&a[0])
                            .ok_or_else(|| ArmError::new("Invalid realloc"))?;
                        let b = bytes(u, a[0], old.min(a[1]))?;
                        u.mem_write(u64::from(result), &b)?;
                        self.release(a[0])?;
                    }
                }
            }
            "posix_memalign" => {
                if a[1] > 16 || !a[1].is_power_of_two() || a[1] < 4 {
                    return Err(ArmError::new("Unsupported guest allocation alignment"));
                }
                let p = self.alloc(a[2])?;
                write_word(u, a[0], p)?;
            }
            "__cxa_atexit"
            | "__cxa_finalize"
            | "__register_atfork"
            | "pthread_mutex_lock"
            | "pthread_mutex_unlock"
            | "pthread_mutex_init"
            | "pthread_mutex_destroy"
            | "pthread_cond_broadcast"
            | "pthread_cond_signal"
            | "__android_log_print" => {}
            "pthread_once" => {
                if word(u, a[0])? == 0 {
                    write_word(u, a[0], 1)?;
                    u.reg_write(RegisterARM::PC, u64::from(a[1]))?;
                    return Ok(());
                }
            }
            "pthread_key_create" => {
                write_word(u, a[0], self.next_key)?;
                self.next_key += 1;
            }
            "pthread_setspecific" => {
                self.tls.insert(a[0], a[1]);
            }
            "pthread_getspecific" => result = self.tls.get(&a[0]).copied().unwrap_or(0),
            "__cxa_guard_acquire" => result = u32::from(word(u, a[0])? == 0),
            "__cxa_guard_release" => write_word(u, a[0], 1)?,
            "dl_unwind_find_exidx" => {
                if let Some((_, p, n)) = self
                    .unwind
                    .iter()
                    .find(|(base, _, _)| a[0] >= *base && a[0] < *base + 0x100_0000)
                {
                    result = *p;
                    write_word(u, a[1], *n)?;
                }
            }
            "memcpy" | "memmove" | "__aeabi_memcpy" | "__aeabi_memcpy4" | "__aeabi_memcpy8"
            | "__aeabi_memmove" | "__aeabi_memmove4" | "__aeabi_memmove8" | "__memcpy_chk" => {
                let b = bytes(u, a[1], a[2])?;
                u.mem_write(u64::from(a[0]), &b)?;
                result = a[0];
            }
            "memset" | "__aeabi_memset" | "__aeabi_memset4" | "__aeabi_memset8"
            | "__aeabi_memclr" | "__aeabi_memclr4" | "__aeabi_memclr8" => {
                let (n, b) = if name == "memset" {
                    (a[2], a[1] as u8)
                } else if name.contains("memclr") {
                    (a[1], 0)
                } else {
                    (a[1], a[2] as u8)
                };
                let n = Self::transfer(n, 1)?;
                u.mem_write(u64::from(a[0]), &vec![b; n])?;
                result = a[0];
            }
            "strlen" | "__strlen_chk" => result = string(u, a[0])?.len() as u32,
            "wcslen" => {
                while word(u, a[0] + result * 4)? != 0 {
                    result += 1;
                    if result > 1024 * 1024 {
                        return Err(ArmError::new("Unterminated wide string"));
                    }
                }
            }
            "wmemcpy" => {
                let n = Self::transfer(a[2], 4)?;
                let b = bytes(u, a[1], n as u32)?;
                u.mem_write(u64::from(a[0]), &b)?;
                result = a[0];
            }
            "strcmp" | "strcasecmp" | "strncmp" | "memcmp" | "wmemcmp" => {
                let (mut b, mut c) = if name == "memcmp" || name == "wmemcmp" {
                    let n = Self::transfer(a[2], if name == "wmemcmp" { 4 } else { 1 })?;
                    (bytes(u, a[0], n as u32)?, bytes(u, a[1], n as u32)?)
                } else {
                    (string(u, a[0])?, string(u, a[1])?)
                };
                if name == "strcasecmp" {
                    b.make_ascii_lowercase();
                    c.make_ascii_lowercase();
                }
                if name == "strncmp" {
                    b.truncate(a[2] as usize);
                    c.truncate(a[2] as usize);
                }
                result = match b.cmp(&c) {
                    std::cmp::Ordering::Less => u32::MAX,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
            }
            "strcpy" | "strncpy" | "__strncpy_chk2" => {
                let mut b = string(u, a[1])?;
                b.push(0);
                if name != "strcpy" {
                    let n = Self::transfer(a[2], 1)?;
                    b.resize(n, 0);
                    b.truncate(n);
                }
                u.mem_write(u64::from(a[0]), &b)?;
                result = a[0];
            }
            "strdup" => {
                let b = string(u, a[0])?;
                result = self.put(u, &b)?;
            }
            "atoi" => {
                let b = string(u, a[0])?;
                let s = String::from_utf8_lossy(&b);
                result = s
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .parse::<i32>()
                    .unwrap_or(0) as u32;
            }
            "wmemchr" => {
                Self::transfer(a[2], 4)?;
                for i in 0..a[2] {
                    let p = a[0]
                        .checked_add(i * 4)
                        .ok_or_else(|| ArmError::new("Wide character range overflow"))?;
                    if word(u, p)? == a[1] {
                        result = p;
                        break;
                    }
                }
            }
            "strchr" | "strrchr" | "memchr" => {
                let b = if name == "memchr" {
                    bytes(u, a[0], a[2])?
                } else {
                    let mut b = string(u, a[0])?;
                    b.push(0);
                    b
                };
                let pos = if name == "strrchr" {
                    b.iter().rposition(|b| *b == a[1] as u8)
                } else {
                    b.iter().position(|b| *b == a[1] as u8)
                };
                result = pos.map_or(0, |i| a[0] + i as u32);
            }
            "stat" | "fstat" => {
                let meta = if name == "stat" {
                    std::fs::metadata(self.path(u, a[0])?)
                } else {
                    self.file(a[0])?.metadata()
                };
                match meta {
                    Ok(meta) => {
                        u.mem_write(u64::from(a[1]), &[0; 104])?;
                        write_word(u, a[1] + 16, 0o100644)?;
                        u.mem_write(u64::from(a[1]) + 48, &meta.len().to_le_bytes())?;
                    }
                    Err(_) => result = u32::MAX,
                }
            }
            "open" | "__open_2" | "fopen" => {
                let path = self.path(u, a[0])?;
                let mut options = OpenOptions::new();
                if name == "fopen" {
                    let mode = string(u, a[1])?;
                    let mode = std::str::from_utf8(&mode)
                        .map_err(|_| ArmError::new("Invalid fopen mode"))?;
                    if path.starts_with(&self.dict) && mode != "r" && mode != "rb" {
                        return Err(ArmError::new("Dictionary assets are read-only"));
                    }
                    match mode {
                        "r" | "rb" => {
                            options.read(true);
                        }
                        "w" | "wb" => {
                            options.write(true).create(true).truncate(true);
                        }
                        "a" | "ab" => {
                            options.append(true).create(true);
                        }
                        "r+" | "rb+" | "r+b" => {
                            options.read(true).write(true);
                        }
                        _ => return Err(ArmError::new(format!("Unsupported fopen mode: {mode}"))),
                    }
                } else {
                    if a[1] != 0 {
                        return Err(ArmError::new("Unsupported open flags"));
                    }
                    options.read(true);
                }
                match options.open(path) {
                    Ok(file) => {
                        result = self.next_file;
                        self.next_file += 1;
                        self.files.insert(result, file);
                    }
                    Err(_) => result = if name == "fopen" { 0 } else { u32::MAX },
                }
            }
            "close" | "fclose" => {
                self.files
                    .remove(&a[0])
                    .ok_or_else(|| ArmError::new("Invalid close"))?;
            }
            "mmap" => {
                let n = Self::transfer(a[1], 1)?;
                result = self.alloc(n as u32)?;
                let f = self.file(a[4])?;
                let pos = f.stream_position()?;
                f.seek(SeekFrom::Start(u64::from(a[5])))?;
                let mut b = vec![0; n];
                f.read_exact(&mut b)?;
                f.seek(SeekFrom::Start(pos))?;
                u.mem_write(u64::from(result), &b)?;
            }
            "fread" => {
                let n = Self::transfer(a[1], a[2])?;
                let mut b = vec![0; n];
                let f = self.file(a[3])?;
                let mut total = 0;
                while total < n {
                    let got = f.read(&mut b[total..])?;
                    if got == 0 {
                        break;
                    }
                    total += got;
                }
                u.mem_write(u64::from(a[0]), &b[..total])?;
                result = (total as u32).checked_div(a[1]).unwrap_or(0);
            }
            "fwrite" => {
                let n = Self::transfer(a[1], a[2])?;
                let b = bytes(u, a[0], n as u32)?;
                self.file(a[3])?.write_all(&b)?;
                result = if a[1] == 0 { 0 } else { a[2] };
            }
            "ftell" => {
                result = u32::try_from(self.file(a[0])?.stream_position()?)
                    .map_err(|_| ArmError::new("Guest file offset overflow"))?
            }
            "fseek" => {
                let offset = i64::from(a[1] as i32);
                let from = match a[2] {
                    0 => SeekFrom::Start(
                        u64::try_from(offset).map_err(|_| ArmError::new("Negative file offset"))?,
                    ),
                    1 => SeekFrom::Current(offset),
                    2 => SeekFrom::End(offset),
                    _ => return Err(ArmError::new("Invalid seek mode")),
                };
                self.file(a[0])?.seek(from)?;
            }
            "fflush" => self.file(a[0])?.flush()?,
            "fgetc" => {
                let mut b = [0];
                result = if self.file(a[0])?.read(&mut b)? == 0 {
                    u32::MAX
                } else {
                    u32::from(b[0])
                };
            }
            "fgets" => {
                let n = Self::transfer(a[1], 1)?;
                if n == 0 {
                    return Err(ArmError::new("Invalid fgets length"));
                }
                let f = self.file(a[2])?;
                let mut b = Vec::new();
                for _ in 1..n {
                    let mut c = [0];
                    if f.read(&mut c)? == 0 {
                        break;
                    }
                    b.push(c[0]);
                    if c[0] == b'\n' {
                        break;
                    }
                }
                if !b.is_empty() {
                    b.push(0);
                    u.mem_write(u64::from(a[0]), &b)?;
                    result = a[0];
                }
            }
            "sscanf" => {
                let text = string(u, a[0])?;
                let format = string(u, a[1])?;
                if format != b"%s%s%s%d" {
                    return Err(ArmError::new("Unsupported scanf format"));
                }
                let text = String::from_utf8_lossy(&text);
                for (i, token) in text.split_whitespace().take(4).enumerate() {
                    if i == 3 {
                        let n = token
                            .parse::<i32>()
                            .map_err(|_| ArmError::new("Invalid dictionary integer"))?;
                        write_word(u, a[i + 2], n as u32)?;
                    } else {
                        u.mem_write(u64::from(a[i + 2]), token.as_bytes())?;
                        u.mem_write(u64::from(a[i + 2]) + token.len() as u64, &[0])?;
                    }
                    result += 1;
                }
            }
            "fprintf" => {}
            "__ctype_get_mb_cur_max" => result = 4,
            "mbtowc" | "mbrtowc" => {
                if a[1] != 0 {
                    let b = bytes(u, a[1], a[2].min(4))?;
                    let width = match b.first() {
                        Some(0) => 0,
                        Some(0..=127) => 1,
                        Some(0xc2..=0xdf) => 2,
                        Some(0xe0..=0xef) => 3,
                        Some(0xf0..=0xf4) => 4,
                        _ => return Err(ArmError::new("Invalid UTF-8 sequence")),
                    };
                    if width > b.len() {
                        result = u32::MAX;
                    } else if width != 0 {
                        let s = std::str::from_utf8(&b[..width])
                            .map_err(|_| ArmError::new("Invalid UTF-8 sequence"))?;
                        if a[0] != 0 {
                            write_word(
                                u,
                                a[0],
                                s.chars()
                                    .next()
                                    .ok_or_else(|| ArmError::new("Empty UTF-8 character"))?
                                    as u32,
                            )?;
                        }
                        result = width as u32;
                    }
                }
            }
            "newlocale" | "duplocale" => result = self.alloc(256)?,
            "freelocale" => self.release(a[0])?,
            "uselocale" => {}
            "__errno" => result = HEAP,
            "getenv" => {}
            "time" => {
                result = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| ArmError::new("Invalid system time"))?
                    .as_secs() as u32;
                if a[0] != 0 {
                    write_word(u, a[0], result)?;
                }
            }
            "ctime" => {
                let timestamp = i64::from(word(u, a[0])? as i32);
                let local = chrono::Local
                    .timestamp_opt(timestamp, 0)
                    .single()
                    .ok_or_else(|| ArmError::new("Invalid guest timestamp"))?;
                let text = local.format("%a %b %e %T %Y\n").to_string();
                result = self.put(u, text.as_bytes())?;
            }
            _ => {
                return Err(ArmError::new(format!(
                    "Unimplemented Android import: {name}"
                )));
            }
        }
        u.reg_write(RegisterARM::R0, u64::from(result))?;
        let lr = u.reg_read(RegisterARM::LR)?;
        u.reg_write(RegisterARM::PC, lr)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host::new(&std::env::temp_dir(), &std::env::temp_dir(), Vec::new()).unwrap()
    }

    #[test]
    fn reclaims_and_coalesces_guest_allocations() {
        let mut host = host();
        let a = host.alloc(32).unwrap();
        let b = host.alloc(64).unwrap();
        let c = host.alloc(16).unwrap();
        host.release(b).unwrap();
        host.release(a).unwrap();
        host.release(c).unwrap();
        assert_eq!(host.alloc(112).unwrap(), a);
        assert!(host.release(b).is_err());
        assert!(host.alloc(u32::MAX).is_err());
    }

    #[test]
    fn rejects_paths_outside_asset_and_learning_roots() {
        let mut host = host();
        let mut machine = crate::arm::ArmMachine::new().unwrap();
        machine.map(u64::from(HEAP), 4096 * 4).unwrap();
        for path in [
            b"/dict/../outside".as_slice(),
            b"/learn/C:\\outside",
            b"C:\\outside",
        ] {
            let p = host.put(&mut machine.emulator, path).unwrap();
            assert!(host.path(&machine.emulator, p).is_err());
        }
    }
}
