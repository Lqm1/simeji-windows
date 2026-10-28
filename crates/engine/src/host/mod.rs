//! Android libc for the engine. Each import of the bundled libraries traps
//! into [`Host::invoke`], which performs the call on the host and returns to
//! the guest.

mod files;
mod format;
mod heap;
mod strings;

pub(crate) use files::{DICTIONARY_ROOT, LEARNING_ROOT};
pub(crate) use heap::{HEAP_END, HEAP_START};

use crate::{
    EngineError,
    arm::{Guest, read_c_string, read_u32, write_u32},
    elf::UnwindTable,
};
use chrono::TimeZone;
use files::Files;
use format::VarArgs;
use heap::{ERRNO_ADDRESS, Heap};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use unicorn_engine::RegisterARM;

const MAX_TRANSFER: u32 = 64 * 1024 * 1024;
/// Largest alignment `posix_memalign` gets; every block is 16-byte aligned.
const MAX_ALIGNMENT: u32 = 16;
const MIN_ALIGNMENT: u32 = 4;
const LOCALE_SIZE: u32 = 256;
const MAX_MULTIBYTE_CHARACTER: u32 = 4;
/// The guest address range covered by one library's unwind table.
const UNWIND_TABLE_RANGE: u32 = 0x100_0000;
const ARGUMENT_REGISTERS: [RegisterARM; 4] = [
    RegisterARM::R0,
    RegisterARM::R1,
    RegisterARM::R2,
    RegisterARM::R3,
];

pub(crate) struct Host {
    heap: Heap,
    files: Files,
    thread_values: BTreeMap<u32, u32>,
    next_thread_key: u32,
    unwind_tables: Vec<UnwindTable>,
    /// A failed import; the engine reports it after emulation stops.
    pub error: Option<EngineError>,
}

/// How an import hands control back to the guest.
enum Completion {
    Return(u32),
    /// A `double` or 64-bit integer result in R0 and R1.
    ReturnWide(u64),
    /// Continues at a guest function that returns to the import's caller.
    Jump(u32),
}

use Completion::{Jump, Return, ReturnWide};

impl Host {
    pub fn new(
        dictionary: &Path,
        learning: &Path,
        unwind_tables: Vec<UnwindTable>,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            heap: Heap::new(),
            files: Files::new(dictionary, learning)?,
            thread_values: BTreeMap::new(),
            next_thread_key: 1,
            unwind_tables,
            error: None,
        })
    }

    pub fn allocate(&mut self, size: u32) -> Result<u32, EngineError> {
        self.heap.allocate(size)
    }

    pub fn free(&mut self, address: u32) -> Result<(), EngineError> {
        self.heap.free(address)
    }

    pub fn store_c_string(
        &mut self,
        guest: &mut Guest<'_>,
        bytes: &[u8],
    ) -> Result<u32, EngineError> {
        let size = u32::try_from(bytes.len() + 1)
            .map_err(|_| EngineError::new("Guest string too long"))?;
        let address = self.heap.allocate(size)?;
        guest.mem_write(address.into(), bytes)?;
        guest.mem_write(u64::from(address) + bytes.len() as u64, &[0])?;
        Ok(address)
    }

    pub fn invoke(&mut self, guest: &mut Guest<'_>, name: &str) -> Result<(), EngineError> {
        let args = arguments(guest)?;
        match self.call(guest, name, args)? {
            Return(value) => return_to_caller(guest, value.into()),
            ReturnWide(value) => {
                guest.reg_write(RegisterARM::R1, value >> 32)?;
                return_to_caller(guest, value & u64::from(u32::MAX))
            }
            Jump(address) => Ok(guest.reg_write(RegisterARM::PC, address.into())?),
        }
    }

    fn call(
        &mut self,
        guest: &mut Guest<'_>,
        name: &str,
        args: [u32; 4],
    ) -> Result<Completion, EngineError> {
        let [a0, a1, a2, a3] = args;
        Ok(match name {
            "pow" | "log" | "log10" | "exp" | "sqrt" | "floor" | "ceil" | "round" => {
                ReturnWide(math(name, a0, a1, a2, a3))
            }
            "vsnprintf" => self.print(guest, a0, Some(a1), a2, VarArgs::List(a3))?,
            "snprintf" => {
                let caller = call_args(guest, args, 3)?;
                self.print(guest, a0, Some(a1), a2, caller)?
            }
            "sprintf" => {
                let caller = call_args(guest, args, 2)?;
                self.print(guest, a0, None, a1, caller)?
            }
            "fprintf" => Return(0),
            "sscanf" => {
                let [a4, a5] = stack_arguments(guest)?;
                Return(format::scan(guest, a0, a1, [a2, a3, a4, a5])?)
            }

            "malloc" => Return(self.heap.allocate(a0)?),
            "calloc" => {
                let length = transfer_size(a0, a1)?;
                let address = self.heap.allocate(length as u32)?;
                guest.mem_write(address.into(), &vec![0; length])?;
                Return(address)
            }
            "realloc" => Return(self.reallocate(guest, a0, a1)?),
            "free" | "munmap" => {
                self.heap.free(a0)?;
                Return(0)
            }
            "posix_memalign" => {
                if !(MIN_ALIGNMENT..=MAX_ALIGNMENT).contains(&a1) || !a1.is_power_of_two() {
                    return Err(EngineError::new("Unsupported guest allocation alignment"));
                }
                let address = self.heap.allocate(a2)?;
                write_u32(guest, a0, address)?;
                Return(0)
            }
            "mmap" => {
                let [a4, a5] = stack_arguments(guest)?;
                let length = transfer_size(a1, 1)?;
                let address = self.heap.allocate(length as u32)?;
                let bytes = self.files.read_at(a4, a5, length)?;
                guest.mem_write(address.into(), &bytes)?;
                Return(address)
            }
            "strdup" => {
                let bytes = read_c_string(guest, a0)?;
                Return(self.store_c_string(guest, &bytes)?)
            }

            "memcpy" | "memmove" | "__aeabi_memcpy" | "__aeabi_memcpy4" | "__aeabi_memcpy8"
            | "__aeabi_memmove" | "__aeabi_memmove4" | "__aeabi_memmove8" | "__memcpy_chk" => {
                Return(strings::copy(guest, a0, a1, a2)?)
            }
            "memset" => Return(strings::fill(guest, a0, a1 as u8, a2)?),
            "__aeabi_memset" | "__aeabi_memset4" | "__aeabi_memset8" => {
                Return(strings::fill(guest, a0, a2 as u8, a1)?)
            }
            "__aeabi_memclr" | "__aeabi_memclr4" | "__aeabi_memclr8" => {
                Return(strings::fill(guest, a0, 0, a1)?)
            }
            "memcmp" => Return(strings::compare_memory(guest, a0, a1, a2, 1)?),
            "wmemcmp" => Return(strings::compare_memory(guest, a0, a1, a2, 4)?),
            "memchr" => Return(strings::find_byte(guest, a0, a1, Some(a2), false)?),
            "wmemchr" => Return(strings::find_wide(guest, a0, a1, a2)?),
            "wmemcpy" => Return(strings::copy_wide(guest, a0, a1, a2)?),
            "strlen" | "__strlen_chk" => Return(strings::length(guest, a0)?),
            "wcslen" => Return(strings::wide_length(guest, a0)?),
            "strcmp" => Return(strings::compare_strings(guest, a0, a1, None, false)?),
            "strncmp" => Return(strings::compare_strings(guest, a0, a1, Some(a2), false)?),
            "strcasecmp" => Return(strings::compare_strings(guest, a0, a1, None, true)?),
            "strcpy" => Return(strings::copy_string(guest, a0, a1, None)?),
            "strncpy" | "__strncpy_chk2" => Return(strings::copy_string(guest, a0, a1, Some(a2))?),
            "strchr" => Return(strings::find_byte(guest, a0, a1, None, false)?),
            "strrchr" => Return(strings::find_byte(guest, a0, a1, None, true)?),
            "atoi" => Return(strings::parse_integer(guest, a0)?),
            "mbtowc" | "mbrtowc" => Return(strings::decode_utf8(guest, a0, a1, a2)?),
            "__ctype_get_mb_cur_max" => Return(MAX_MULTIBYTE_CHARACTER),

            "open" | "__open_2" => Return(self.files.open(guest, a0, a1)?),
            "fopen" => Return(self.files.fopen(guest, a0, a1)?),
            "close" | "fclose" => Return(self.files.close(a0)?),
            "fread" => Return(self.files.read(guest, a0, a1, a2, a3)?),
            "fwrite" => Return(self.files.write(guest, a0, a1, a2, a3)?),
            "ftell" => Return(self.files.tell(a0)?),
            "fseek" => Return(self.files.seek(a0, a1, a2)?),
            "fflush" => Return(self.files.flush(a0)?),
            "fgetc" => Return(self.files.get_byte(a0)?),
            "fgets" => Return(self.files.get_line(guest, a0, a1, a2)?),
            "stat" => Return(self.files.stat(guest, a0, a1)?),
            "fstat" => Return(self.files.fstat(guest, a0, a1)?),
            "rename" => Return(self.files.rename(guest, a0, a1)?),
            "remove" | "unlink" => Return(self.files.remove(guest, a0)?),

            "__cxa_atexit"
            | "__cxa_finalize"
            | "__register_atfork"
            | "pthread_mutex_lock"
            | "pthread_mutex_unlock"
            | "pthread_mutex_init"
            | "pthread_mutex_destroy"
            | "pthread_cond_broadcast"
            | "pthread_cond_signal"
            | "__android_log_print"
            | "uselocale"
            | "getenv" => Return(0),
            "pthread_once" => {
                if read_u32(guest, a0)? == 0 {
                    write_u32(guest, a0, 1)?;
                    Jump(a1)
                } else {
                    Return(0)
                }
            }
            "pthread_key_create" => {
                write_u32(guest, a0, self.next_thread_key)?;
                self.next_thread_key += 1;
                Return(0)
            }
            "pthread_setspecific" => {
                self.thread_values.insert(a0, a1);
                Return(0)
            }
            "pthread_getspecific" => Return(self.thread_values.get(&a0).copied().unwrap_or(0)),
            "__cxa_guard_acquire" => Return(u32::from(read_u32(guest, a0)? == 0)),
            "__cxa_guard_release" => {
                write_u32(guest, a0, 1)?;
                Return(0)
            }
            "dl_unwind_find_exidx" => Return(self.find_unwind_table(guest, a0, a1)?),
            "newlocale" | "duplocale" => Return(self.heap.allocate(LOCALE_SIZE)?),
            "freelocale" => {
                self.heap.free(a0)?;
                Return(0)
            }
            "__errno" => Return(ERRNO_ADDRESS),
            "time" => {
                let seconds = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| EngineError::new("Invalid system time"))?
                    .as_secs() as u32;
                if a0 != 0 {
                    write_u32(guest, a0, seconds)?;
                }
                Return(seconds)
            }
            "ctime" => {
                let timestamp = i64::from(read_u32(guest, a0)? as i32);
                let local = chrono::Local
                    .timestamp_opt(timestamp, 0)
                    .single()
                    .ok_or_else(|| EngineError::new("Invalid guest timestamp"))?;
                let text = local.format("%a %b %e %T %Y\n").to_string();
                Return(self.store_c_string(guest, text.as_bytes())?)
            }
            _ => {
                return Err(EngineError::new(format!(
                    "Unimplemented Android import: {name}"
                )));
            }
        })
    }

    /// `vsnprintf`, `snprintf` (with a `capacity`) and `sprintf` (without).
    /// A zero capacity only measures the output.
    fn print(
        &mut self,
        guest: &mut Guest<'_>,
        buffer: u32,
        capacity: Option<u32>,
        format: u32,
        args: VarArgs,
    ) -> Result<Completion, EngineError> {
        let mut text = format::print(guest, format, args)?;
        let length = text.len() as u32;
        if let Some(capacity) = capacity {
            if capacity == 0 {
                return Ok(Return(length));
            }
            text.truncate(capacity as usize - 1);
        }
        text.push(0);
        guest.mem_write(buffer.into(), &text)?;
        Ok(Return(length))
    }

    fn reallocate(
        &mut self,
        guest: &mut Guest<'_>,
        address: u32,
        size: u32,
    ) -> Result<u32, EngineError> {
        if size == 0 {
            self.heap.free(address)?;
            return Ok(0);
        }
        let moved = self.heap.allocate(size)?;
        if address != 0 {
            let old_size = self
                .heap
                .size_of(address)
                .ok_or_else(|| EngineError::new("Invalid realloc"))?;
            let bytes = read_bytes(guest, address, old_size.min(size))?;
            guest.mem_write(moved.into(), &bytes)?;
            self.heap.free(address)?;
        }
        Ok(moved)
    }

    /// Returns the unwind table covering `pc` and writes its entry count.
    fn find_unwind_table(
        &self,
        guest: &mut Guest<'_>,
        pc: u32,
        count: u32,
    ) -> Result<u32, EngineError> {
        let Some(table) = self
            .unwind_tables
            .iter()
            .find(|table| pc >= table.image_base && pc < table.image_base + UNWIND_TABLE_RANGE)
        else {
            return Ok(0);
        };
        write_u32(guest, count, table.entries)?;
        Ok(table.address)
    }
}

fn arguments(guest: &Guest<'_>) -> Result<[u32; 4], EngineError> {
    let mut args = [0; 4];
    for (value, register) in args.iter_mut().zip(ARGUMENT_REGISTERS) {
        *value = guest.reg_read(register)? as u32;
    }
    Ok(args)
}

/// The fifth and sixth arguments, which AAPCS passes on the stack.
fn stack_arguments(guest: &Guest<'_>) -> Result<[u32; 2], EngineError> {
    let stack = guest.reg_read(RegisterARM::SP)? as u32;
    Ok([read_u32(guest, stack)?, read_u32(guest, stack + 4)?])
}

/// The variable arguments of a call whose fixed arguments end before `next`.
fn call_args(guest: &Guest<'_>, args: [u32; 4], next: usize) -> Result<VarArgs, EngineError> {
    Ok(VarArgs::Call {
        registers: args,
        stack: guest.reg_read(RegisterARM::SP)? as u32,
        next,
    })
}

fn return_to_caller(guest: &mut Guest<'_>, value: u64) -> Result<(), EngineError> {
    guest.reg_write(RegisterARM::R0, value)?;
    let caller = guest.reg_read(RegisterARM::LR)?;
    Ok(guest.reg_write(RegisterARM::PC, caller)?)
}

/// The `double` functions, with arguments in R0:R1 and R2:R3.
fn math(name: &str, a0: u32, a1: u32, a2: u32, a3: u32) -> u64 {
    let x = f64::from_bits(u64::from(a0) | (u64::from(a1) << 32));
    let y = f64::from_bits(u64::from(a2) | (u64::from(a3) << 32));
    match name {
        "pow" => x.powf(y),
        "log" => x.ln(),
        "log10" => x.log10(),
        "exp" => x.exp(),
        "sqrt" => x.sqrt(),
        "floor" => x.floor(),
        "ceil" => x.ceil(),
        _ => x.round(),
    }
    .to_bits()
}

/// The byte count of `count` items of `size`, within the transfer limit.
fn transfer_size(count: u32, size: u32) -> Result<usize, EngineError> {
    count
        .checked_mul(size)
        .filter(|length| *length <= MAX_TRANSFER)
        .map(|length| length as usize)
        .ok_or_else(|| EngineError::new("Guest IO size limit exceeded"))
}

fn read_bytes(guest: &Guest<'_>, address: u32, length: u32) -> Result<Vec<u8>, EngineError> {
    if length > MAX_TRANSFER {
        return Err(EngineError::new("Guest transfer limit exceeded"));
    }
    Ok(guest.mem_read_as_vec(address.into(), length as usize)?)
}
