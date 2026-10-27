//! Bounded ARM execution. Guest addresses never become host pointers.

use std::{error::Error, fmt};

use unicorn_engine::{
    RegisterARM, Unicorn,
    unicorn_const::{Arch, Mode, Prot},
};

const STACK: u64 = 0x7000_0000;
const STACK_SIZE: u64 = 0x20_0000;
const RETURN: u64 = 0x710f_fff0;
const TIMEOUT_US: u64 = 5_000_000;
const INSTRUCTION_LIMIT: usize = 20_000_000;

#[derive(Debug)]
pub struct ArmError(String);

impl ArmError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ArmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for ArmError {}

fn checked<T>(result: Result<T, unicorn_engine::uc_error>) -> Result<T, ArmError> {
    result.map_err(|error| ArmError::new(format!("ARM execution: {error:?}")))
}

/// A single-threaded guest machine with a private stack and bounded calls.
pub struct ArmMachine {
    pub(crate) emulator: Unicorn<'static, ()>,
}

impl ArmMachine {
    pub fn new() -> Result<Self, ArmError> {
        let mut emulator = checked(Unicorn::new(Arch::ARM, Mode::LITTLE_ENDIAN))?;
        checked(emulator.reg_write(RegisterARM::C1_C0_2, 0x00f0_0000))?;
        checked(emulator.reg_write(RegisterARM::FPEXC, 0x4000_0000))?;
        checked(emulator.mem_map(STACK, STACK_SIZE, Prot::READ | Prot::WRITE))?;
        checked(emulator.mem_map(RETURN & !0xfff, 0x1000, Prot::EXEC | Prot::READ))?;
        Ok(Self { emulator })
    }

    pub fn map(&mut self, address: u64, size: u64) -> Result<(), ArmError> {
        if size == 0 || !address.is_multiple_of(4096) || !size.is_multiple_of(4096) {
            return Err(ArmError::new("Guest mappings must be nonempty whole pages"));
        }
        let end = address
            .checked_add(size)
            .ok_or_else(|| ArmError::new("Guest address overflow"))?;
        if end > u64::from(u32::MAX) + 1 {
            return Err(ArmError::new("Guest mapping exceeds ARM32 address space"));
        }
        checked(self.emulator.mem_map(address, size, Prot::ALL))
    }

    pub fn write(&mut self, address: u64, bytes: &[u8]) -> Result<(), ArmError> {
        checked(self.emulator.mem_write(address, bytes))
    }

    pub fn read(&self, address: u64, size: usize) -> Result<Vec<u8>, ArmError> {
        checked(self.emulator.mem_read_as_vec(address, size))
    }

    /// Calls an AAPCS32 function. A set low bit selects Thumb instructions.
    pub fn call(&mut self, address: u32, arguments: &[u32]) -> Result<u32, ArmError> {
        if arguments.len() > 32 {
            return Err(ArmError::new("Too many ARM call arguments"));
        }
        let stack_top = STACK + STACK_SIZE - 0x1000;
        checked(self.emulator.reg_write(RegisterARM::SP, stack_top))?;
        checked(self.emulator.reg_write(RegisterARM::LR, RETURN))?;
        for (index, register) in [
            RegisterARM::R0,
            RegisterARM::R1,
            RegisterARM::R2,
            RegisterARM::R3,
        ]
        .into_iter()
        .enumerate()
        {
            checked(self.emulator.reg_write(
                register,
                u64::from(arguments.get(index).copied().unwrap_or(0)),
            ))?;
        }
        for (index, argument) in arguments.iter().skip(4).enumerate() {
            self.write(stack_top + (index * 4) as u64, &argument.to_le_bytes())?;
        }
        checked(self.emulator.emu_start(
            u64::from(address),
            RETURN,
            TIMEOUT_US,
            INSTRUCTION_LIMIT,
        ))?;
        let pc = checked(self.emulator.reg_read(RegisterARM::PC))?;
        if pc != RETURN {
            return Err(ArmError::new(format!(
                "ARM function did not return within execution limits (PC {pc:#x})"
            )));
        }
        Ok(checked(self.emulator.reg_read(RegisterARM::R0))? as u32)
    }
}
