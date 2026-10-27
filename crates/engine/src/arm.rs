//! Bounded ARM32 execution. Guest addresses never become host pointers.

use crate::EngineError;
use unicorn_engine::{
    RegisterARM, Unicorn,
    unicorn_const::{Arch, Mode, Prot},
};

pub(crate) type Guest<'a> = Unicorn<'a, ()>;

const STACK_BASE: u64 = 0x7000_0000;
const STACK_SIZE: u64 = 0x20_0000;
const STACK_HEADROOM: u64 = 0x1000;
/// Calls return to this address, where emulation stops.
const RETURN_ADDRESS: u64 = 0x710f_fff0;
const PAGE_SIZE: u64 = 0x1000;
const DEFAULT_TIMEOUT_US: u64 = 5_000_000;
const DEFAULT_INSTRUCTION_LIMIT: usize = 20_000_000;
const MAX_ARGUMENTS: usize = 32;
const MAX_C_STRING_BYTES: usize = 1024 * 1024;
/// CPACR value granting full access to the VFP coprocessors.
const COPROCESSOR_ACCESS: u64 = 0x00f0_0000;
/// FPEXC with the VFP enable bit set.
const VFP_ENABLED: u64 = 0x4000_0000;

/// A single-threaded guest machine with a private stack and bounded calls.
pub struct ArmMachine {
    pub(crate) guest: Guest<'static>,
}

impl ArmMachine {
    pub fn new() -> Result<Self, EngineError> {
        let mut guest = Unicorn::new(Arch::ARM, Mode::LITTLE_ENDIAN)?;
        guest.reg_write(RegisterARM::C1_C0_2, COPROCESSOR_ACCESS)?;
        guest.reg_write(RegisterARM::FPEXC, VFP_ENABLED)?;
        guest.mem_map(STACK_BASE, STACK_SIZE, Prot::READ | Prot::WRITE)?;
        guest.mem_map(
            RETURN_ADDRESS & !(PAGE_SIZE - 1),
            PAGE_SIZE,
            Prot::EXEC | Prot::READ,
        )?;
        Ok(Self { guest })
    }

    pub fn map(&mut self, address: u64, size: u64) -> Result<(), EngineError> {
        if size == 0 || !address.is_multiple_of(PAGE_SIZE) || !size.is_multiple_of(PAGE_SIZE) {
            return Err(EngineError::new(
                "Guest mappings must be nonempty whole pages",
            ));
        }
        let end = address
            .checked_add(size)
            .ok_or_else(|| EngineError::new("Guest address overflow"))?;
        if end > u64::from(u32::MAX) + 1 {
            return Err(EngineError::new(
                "Guest mapping exceeds ARM32 address space",
            ));
        }
        Ok(self.guest.mem_map(address, size, Prot::ALL)?)
    }

    pub fn write(&mut self, address: u64, bytes: &[u8]) -> Result<(), EngineError> {
        Ok(self.guest.mem_write(address, bytes)?)
    }

    pub fn read(&self, address: u64, size: usize) -> Result<Vec<u8>, EngineError> {
        Ok(self.guest.mem_read_as_vec(address, size)?)
    }

    /// Calls an AAPCS32 function. A set low bit in `address` selects Thumb.
    pub fn call(&mut self, address: u32, arguments: &[u32]) -> Result<u32, EngineError> {
        self.call_with_limits(
            address,
            arguments,
            DEFAULT_TIMEOUT_US,
            DEFAULT_INSTRUCTION_LIMIT,
        )
    }

    /// An `instruction_limit` of zero leaves only the timeout.
    pub(crate) fn call_with_limits(
        &mut self,
        address: u32,
        arguments: &[u32],
        timeout_us: u64,
        instruction_limit: usize,
    ) -> Result<u32, EngineError> {
        if arguments.len() > MAX_ARGUMENTS {
            return Err(EngineError::new("Too many ARM call arguments"));
        }
        let stack_top = STACK_BASE + STACK_SIZE - STACK_HEADROOM;
        self.guest.reg_write(RegisterARM::SP, stack_top)?;
        self.guest.reg_write(RegisterARM::LR, RETURN_ADDRESS)?;
        let registers = [
            RegisterARM::R0,
            RegisterARM::R1,
            RegisterARM::R2,
            RegisterARM::R3,
        ];
        for (index, register) in registers.into_iter().enumerate() {
            let value = arguments.get(index).copied().unwrap_or(0);
            self.guest.reg_write(register, u64::from(value))?;
        }
        for (index, argument) in arguments.iter().skip(registers.len()).enumerate() {
            self.write(stack_top + (index * 4) as u64, &argument.to_le_bytes())?;
        }
        self.guest.emu_start(
            u64::from(address),
            RETURN_ADDRESS,
            timeout_us,
            instruction_limit,
        )?;
        let pc = self.guest.reg_read(RegisterARM::PC)?;
        if pc != RETURN_ADDRESS {
            return Err(EngineError::new(format!(
                "ARM function did not return within execution limits (PC {pc:#x})"
            )));
        }
        Ok(self.guest.reg_read(RegisterARM::R0)? as u32)
    }
}

pub(crate) fn read_u32(guest: &Guest<'_>, address: u32) -> Result<u32, EngineError> {
    let mut bytes = [0; 4];
    guest.mem_read(u64::from(address), &mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

pub(crate) fn write_u32(
    guest: &mut Guest<'_>,
    address: u32,
    value: u32,
) -> Result<(), EngineError> {
    Ok(guest.mem_write(u64::from(address), &value.to_le_bytes())?)
}

/// Reads a NUL-terminated string without the terminator.
pub(crate) fn read_c_string(guest: &Guest<'_>, address: u32) -> Result<Vec<u8>, EngineError> {
    if address == 0 {
        return Err(EngineError::new("Null guest string"));
    }
    let mut text = Vec::new();
    while text.len() < MAX_C_STRING_BYTES {
        let position = u64::from(address) + text.len() as u64;
        let to_page_end = PAGE_SIZE - (position & (PAGE_SIZE - 1));
        let block = guest.mem_read_as_vec(position, to_page_end.min(256) as usize)?;
        if let Some(end) = block.iter().position(|byte| *byte == 0) {
            text.extend_from_slice(&block[..end]);
            return Ok(text);
        }
        text.extend_from_slice(&block);
    }
    Err(EngineError::new("Unterminated guest string"))
}
