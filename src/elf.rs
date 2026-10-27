//! ARM32 ELF loading and relocation in guest memory.

use std::collections::BTreeMap;

use goblin::elf::{
    Elf,
    header::{EM_ARM, ET_DYN},
    program_header::PT_LOAD,
    reloc::{R_ARM_ABS32, R_ARM_GLOB_DAT, R_ARM_JUMP_SLOT, R_ARM_NONE, R_ARM_RELATIVE},
    section_header::SHN_UNDEF,
    sym::STT_OBJECT,
};

use crate::arm::{ArmError, ArmMachine};

const PAGE: u64 = 4096;
const SLOT_SIZE: u64 = 0x0100_0000;
const IMPORT_CODE: u32 = 0x7200_0000;
const IMPORT_DATA: u32 = 0x7300_0000;
const IMPORT_SIZE: u32 = 0x1_0000;

const LIBRARIES: [(&str, &[u8], u32); 3] = [
    (
        "libc++_shared.so",
        include_bytes!("../assets/native/libc++_shared.so"),
        0x3000_0000,
    ),
    (
        "libbaiducmph.so",
        include_bytes!("../assets/native/libbaiducmph.so"),
        0x3100_0000,
    ),
    (
        "libbaiduime.so",
        include_bytes!("../assets/native/libbaiduime.so"),
        0x3200_0000,
    ),
];

/// An unresolved Android dependency, represented only by a guest address.
#[derive(Debug, Clone)]
pub struct Import {
    pub name: String,
    pub address: u32,
    pub is_data: bool,
}

/// Source-app libraries linked together without loading them into Windows.
/// Android imports initially trap; the host must implement them explicitly.
pub struct LibrarySet {
    pub machine: ArmMachine,
    pub imports: Vec<Import>,
    pub initializers: Vec<u32>,
    pub(crate) unwind: Vec<(u32, u32, u32)>,
    symbols: BTreeMap<String, u32>,
}

fn parse(bytes: &[u8]) -> Result<Elf<'_>, ArmError> {
    let elf = Elf::parse(bytes).map_err(|error| ArmError::new(error.to_string()))?;
    if elf.is_64
        || !elf.little_endian
        || elf.header.e_machine != EM_ARM
        || elf.header.e_type != ET_DYN
    {
        return Err(ArmError::new(
            "Expected a little-endian ARM32 shared library",
        ));
    }
    Ok(elf)
}

fn word(machine: &ArmMachine, address: u64) -> Result<u32, ArmError> {
    let bytes = machine.read(address, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn write_word(machine: &mut ArmMachine, address: u64, value: u32) -> Result<(), ArmError> {
    machine.write(address, &value.to_le_bytes())
}

impl LibrarySet {
    pub fn bundled() -> Result<Self, ArmError> {
        let mut machine = ArmMachine::new()?;
        machine.map(u64::from(IMPORT_CODE), u64::from(IMPORT_SIZE))?;
        machine.map(u64::from(IMPORT_DATA), u64::from(IMPORT_SIZE))?;
        let mut symbols = BTreeMap::new();
        let mut sizes = BTreeMap::new();
        for (name, bytes, base) in LIBRARIES {
            let elf = parse(bytes)?;
            let end = elf
                .program_headers
                .iter()
                .filter(|segment| segment.p_type == PT_LOAD)
                .map(|segment| segment.p_vaddr.checked_add(segment.p_memsz))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| ArmError::new("ELF segment address overflow"))?
                .into_iter()
                .max()
                .filter(|end| *end > 0 && *end <= SLOT_SIZE)
                .ok_or_else(|| ArmError::new(format!("Invalid ELF image size: {name}")))?;
            machine.map(u64::from(base), end.div_ceil(PAGE) * PAGE)?;
            sizes.insert(base, end);
            for segment in &elf.program_headers {
                if segment.p_type != PT_LOAD {
                    continue;
                }
                let start = usize::try_from(segment.p_offset)
                    .map_err(|_| ArmError::new("ELF file offset overflow"))?;
                let length = usize::try_from(segment.p_filesz)
                    .map_err(|_| ArmError::new("ELF segment length overflow"))?;
                let file_end = start
                    .checked_add(length)
                    .ok_or_else(|| ArmError::new("ELF file range overflow"))?;
                let content = bytes
                    .get(start..file_end)
                    .filter(|_| segment.p_filesz <= segment.p_memsz)
                    .ok_or_else(|| ArmError::new("ELF segment exceeds file or memory range"))?;
                machine.write(u64::from(base) + segment.p_vaddr, content)?;
            }
            for symbol in &elf.dynsyms {
                if symbol.st_shndx == SHN_UNDEF as usize {
                    continue;
                }
                if let Some(name) = elf.dynstrtab.get_at(symbol.st_name) {
                    let offset = u32::try_from(symbol.st_value)
                        .map_err(|_| ArmError::new("ELF symbol address overflow"))?;
                    if u64::from(offset & !1) >= end {
                        return Err(ArmError::new("ELF symbol exceeds image"));
                    }
                    symbols.entry(name.to_owned()).or_insert(base + offset);
                }
            }
        }
        let mut imports = Vec::new();
        let mut code_cursor = IMPORT_CODE;
        let mut data_cursor = IMPORT_DATA;
        for (library, bytes, base) in LIBRARIES {
            let elf = parse(bytes)?;
            if !elf.dynrelas.is_empty() {
                return Err(ArmError::new("ARM RELA relocations are unsupported"));
            }
            let image_size = sizes[&base];
            for relocation in elf.dynrels.iter().chain(elf.pltrelocs.iter()) {
                if relocation.r_type == R_ARM_NONE {
                    continue;
                }
                if relocation
                    .r_offset
                    .checked_add(4)
                    .is_none_or(|end| end > image_size)
                {
                    return Err(ArmError::new("ELF relocation exceeds image"));
                }
                let address = u64::from(base) + relocation.r_offset;
                let addend = word(&machine, address)?;
                let value = match relocation.r_type {
                    R_ARM_RELATIVE => base.wrapping_add(addend),
                    R_ARM_ABS32 | R_ARM_GLOB_DAT | R_ARM_JUMP_SLOT => {
                        let symbol = elf
                            .dynsyms
                            .get(relocation.r_sym)
                            .ok_or_else(|| ArmError::new("Invalid ELF relocation symbol"))?;
                        let name = elf
                            .dynstrtab
                            .get_at(symbol.st_name)
                            .filter(|name| !name.is_empty())
                            .ok_or_else(|| ArmError::new("Missing ELF import name"))?;
                        let resolved = if let Some(address) = symbols.get(name) {
                            *address
                        } else {
                            let is_data = symbol.st_type() == STT_OBJECT;
                            let address = if is_data {
                                let size = u32::try_from(symbol.st_size.max(16))
                                    .map_err(|_| ArmError::new("Import data size overflow"))?;
                                let next = data_cursor
                                    .checked_add(size.div_ceil(16) * 16)
                                    .filter(|end| *end <= IMPORT_DATA + IMPORT_SIZE)
                                    .ok_or_else(|| ArmError::new("Import data area exhausted"))?;
                                let address = data_cursor;
                                data_cursor = next;
                                address
                            } else {
                                if code_cursor >= IMPORT_CODE + IMPORT_SIZE {
                                    return Err(ArmError::new("Import code area exhausted"));
                                }
                                let address = code_cursor;
                                code_cursor += 4;
                                // UDF #0: unresolved functions fail instead of returning fake success.
                                machine.write(u64::from(address), &[0xf0, 0x00, 0xf0, 0xe7])?;
                                address
                            };
                            symbols.insert(name.to_owned(), address);
                            imports.push(Import {
                                name: name.to_owned(),
                                address,
                                is_data,
                            });
                            address
                        };
                        if relocation.r_type == R_ARM_ABS32 {
                            resolved.wrapping_add(addend)
                        } else {
                            resolved
                        }
                    }
                    kind => {
                        return Err(ArmError::new(format!(
                            "Unsupported relocation {kind} in {library}"
                        )));
                    }
                };
                write_word(&mut machine, address, value)?;
            }
        }
        let mut initializers = Vec::new();
        let mut unwind = Vec::new();
        for (_, bytes, base) in LIBRARIES {
            let elf = parse(bytes)?;
            for section in &elf.section_headers {
                if elf.shdr_strtab.get_at(section.sh_name) == Some(".ARM.exidx") {
                    unwind.push((
                        base,
                        base + section.sh_addr as u32,
                        section.sh_size as u32 / 8,
                    ));
                }
                if elf.shdr_strtab.get_at(section.sh_name) != Some(".init_array") {
                    continue;
                }
                if !section.sh_size.is_multiple_of(4)
                    || section
                        .sh_addr
                        .checked_add(section.sh_size)
                        .is_none_or(|end| end > sizes[&base])
                {
                    return Err(ArmError::new("Invalid ELF initializer array"));
                }
                for offset in (0..section.sh_size).step_by(4) {
                    let address = word(&machine, u64::from(base) + section.sh_addr + offset)?;
                    if address != 0 && address != u32::MAX {
                        initializers.push(address);
                    }
                }
            }
        }
        Ok(Self {
            machine,
            imports,
            initializers,
            unwind,
            symbols,
        })
    }

    pub fn symbol(&self, name: &str) -> Result<u32, ArmError> {
        self.symbols
            .get(name)
            .copied()
            .ok_or_else(|| ArmError::new(format!("Unknown engine symbol: {name}")))
    }

    pub fn call(&mut self, name: &str, arguments: &[u32]) -> Result<u32, ArmError> {
        let address = self.symbol(name)?;
        self.machine.call(address, arguments)
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn rejects_non_elf_and_truncated_elf() {
        assert!(parse(b"not an ELF").is_err());
        assert!(parse(b"\x7fELF\x01\x01\x01").is_err());
    }
}
