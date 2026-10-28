//! Loads and links the bundled ARM32 libraries in guest memory.

use crate::{
    EngineError,
    arm::{ArmMachine, read_u32, write_u32},
};
use goblin::elf::{
    Elf,
    header::{EM_ARM, ET_DYN},
    program_header::PT_LOAD,
    reloc::{R_ARM_ABS32, R_ARM_GLOB_DAT, R_ARM_JUMP_SLOT, R_ARM_NONE, R_ARM_RELATIVE},
    section_header::SHN_UNDEF,
    sym::{STT_OBJECT, Sym},
};
use std::collections::BTreeMap;

const PAGE_SIZE: u64 = 4096;
const IMAGE_SLOT_SIZE: u64 = 0x0100_0000;
pub(crate) const IMPORT_CODE_BASE: u32 = 0x7200_0000;
const IMPORT_DATA_BASE: u32 = 0x7300_0000;
pub(crate) const IMPORT_AREA_SIZE: u32 = 0x1_0000;
const IMPORT_DATA_ALIGNMENT: u32 = 16;
const IMPORT_FUNCTION_STUB_SIZE: u32 = 4;
/// `UDF #0`: an import without a host implementation traps.
const UNDEFINED_INSTRUCTION: [u8; 4] = [0xf0, 0x00, 0xf0, 0xe7];
const EXIDX_ENTRY_SIZE: u64 = 8;

struct Library {
    name: &'static str,
    image: &'static [u8],
    base: u32,
}

/// In dependency order: earlier libraries provide symbols to later ones.
const LIBRARIES: [Library; 3] = [
    Library {
        name: "libc++_shared.so",
        image: include_bytes!("../../../assets/native/libc++_shared.so"),
        base: 0x3000_0000,
    },
    Library {
        name: "libbaiducmph.so",
        image: include_bytes!("../../../assets/native/libbaiducmph.so"),
        base: 0x3100_0000,
    },
    Library {
        name: "libbaiduime.so",
        image: include_bytes!("../../../assets/native/libbaiduime.so"),
        base: 0x3200_0000,
    },
];

/// An Android dependency of the libraries, implemented by the host.
#[derive(Debug, Clone)]
pub struct Import {
    pub name: String,
    pub address: u32,
    pub is_data: bool,
}

/// The `.ARM.exidx` table that C++ exception unwinding looks up per library.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UnwindTable {
    pub image_base: u32,
    pub address: u32,
    pub entries: u32,
}

/// The source app's libraries, linked together without loading them into Windows.
pub struct LibrarySet {
    pub machine: ArmMachine,
    pub imports: Vec<Import>,
    pub initializers: Vec<u32>,
    pub(crate) unwind_tables: Vec<UnwindTable>,
    symbols: BTreeMap<String, u32>,
}

struct ImportArea {
    next_function: u32,
    next_data: u32,
}

impl ImportArea {
    fn function(&mut self) -> Result<u32, EngineError> {
        if self.next_function >= IMPORT_CODE_BASE + IMPORT_AREA_SIZE {
            return Err(EngineError::new("Import code area exhausted"));
        }
        let address = self.next_function;
        self.next_function += IMPORT_FUNCTION_STUB_SIZE;
        Ok(address)
    }

    fn data(&mut self, size: u64) -> Result<u32, EngineError> {
        let size = u32::try_from(size.max(u64::from(IMPORT_DATA_ALIGNMENT)))
            .map_err(|_| EngineError::new("Import data size overflow"))?;
        let next = self
            .next_data
            .checked_add(size.div_ceil(IMPORT_DATA_ALIGNMENT) * IMPORT_DATA_ALIGNMENT)
            .filter(|end| *end <= IMPORT_DATA_BASE + IMPORT_AREA_SIZE)
            .ok_or_else(|| EngineError::new("Import data area exhausted"))?;
        let address = self.next_data;
        self.next_data = next;
        Ok(address)
    }
}

fn parse(image: &'static [u8]) -> Result<Elf<'static>, EngineError> {
    let elf = Elf::parse(image).map_err(|error| EngineError::new(error.to_string()))?;
    if elf.is_64
        || !elf.little_endian
        || elf.header.e_machine != EM_ARM
        || elf.header.e_type != ET_DYN
    {
        return Err(EngineError::new(
            "Expected a little-endian ARM32 shared library",
        ));
    }
    Ok(elf)
}

impl LibrarySet {
    pub fn bundled() -> Result<Self, EngineError> {
        let mut machine = ArmMachine::new()?;
        machine.map(IMPORT_CODE_BASE.into(), IMPORT_AREA_SIZE.into())?;
        machine.map(IMPORT_DATA_BASE.into(), IMPORT_AREA_SIZE.into())?;
        let mut set = Self {
            machine,
            imports: Vec::new(),
            initializers: Vec::new(),
            unwind_tables: Vec::new(),
            symbols: BTreeMap::new(),
        };
        let mut images = Vec::new();
        for library in &LIBRARIES {
            let elf = parse(library.image)?;
            let size = set.load(library, &elf)?;
            images.push((library, elf, size));
        }
        let mut area = ImportArea {
            next_function: IMPORT_CODE_BASE,
            next_data: IMPORT_DATA_BASE,
        };
        for (library, elf, size) in &images {
            set.relocate(library, elf, *size, &mut area)?;
        }
        for (library, elf, size) in &images {
            set.read_sections(library, elf, *size)?;
        }
        Ok(set)
    }

    pub fn symbol(&self, name: &str) -> Result<u32, EngineError> {
        self.symbols
            .get(name)
            .copied()
            .ok_or_else(|| EngineError::new(format!("Unknown engine symbol: {name}")))
    }

    pub fn call(&mut self, name: &str, arguments: &[u32]) -> Result<u32, EngineError> {
        let address = self.symbol(name)?;
        self.machine.call(address, arguments)
    }

    /// Copies the loadable segments into the library's slot, records its
    /// exported symbols and returns the image size.
    fn load(&mut self, library: &Library, elf: &Elf<'_>) -> Result<u64, EngineError> {
        let segments = || elf.program_headers.iter().filter(|s| s.p_type == PT_LOAD);
        let size = segments()
            .map(|segment| segment.p_vaddr.checked_add(segment.p_memsz))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| EngineError::new("ELF segment address overflow"))?
            .into_iter()
            .max()
            .filter(|end| *end > 0 && *end <= IMAGE_SLOT_SIZE)
            .ok_or_else(|| EngineError::new(format!("Invalid ELF image size: {}", library.name)))?;
        self.machine
            .map(library.base.into(), size.div_ceil(PAGE_SIZE) * PAGE_SIZE)?;
        for segment in segments() {
            let start = usize::try_from(segment.p_offset)
                .map_err(|_| EngineError::new("ELF file offset overflow"))?;
            let length = usize::try_from(segment.p_filesz)
                .map_err(|_| EngineError::new("ELF segment length overflow"))?;
            let end = start
                .checked_add(length)
                .ok_or_else(|| EngineError::new("ELF file range overflow"))?;
            let content = library
                .image
                .get(start..end)
                .filter(|_| segment.p_filesz <= segment.p_memsz)
                .ok_or_else(|| EngineError::new("ELF segment exceeds file or memory range"))?;
            self.machine
                .write(u64::from(library.base) + segment.p_vaddr, content)?;
        }
        for symbol in &elf.dynsyms {
            if symbol.st_shndx == SHN_UNDEF as usize {
                continue;
            }
            let Some(name) = elf.dynstrtab.get_at(symbol.st_name) else {
                continue;
            };
            let offset = u32::try_from(symbol.st_value)
                .map_err(|_| EngineError::new("ELF symbol address overflow"))?;
            if u64::from(offset & !1) >= size {
                return Err(EngineError::new("ELF symbol exceeds image"));
            }
            self.symbols
                .entry(name.to_owned())
                .or_insert(library.base + offset);
        }
        Ok(size)
    }

    fn relocate(
        &mut self,
        library: &Library,
        elf: &Elf<'_>,
        image_size: u64,
        area: &mut ImportArea,
    ) -> Result<(), EngineError> {
        if !elf.dynrelas.is_empty() {
            return Err(EngineError::new("ARM RELA relocations are unsupported"));
        }
        for relocation in elf.dynrels.iter().chain(elf.pltrelocs.iter()) {
            if relocation.r_type == R_ARM_NONE {
                continue;
            }
            if relocation
                .r_offset
                .checked_add(4)
                .is_none_or(|end| end > image_size)
            {
                return Err(EngineError::new("ELF relocation exceeds image"));
            }
            let address = library.base + relocation.r_offset as u32;
            let addend = read_u32(&self.machine.guest, address)?;
            let value = match relocation.r_type {
                R_ARM_RELATIVE => library.base.wrapping_add(addend),
                R_ARM_ABS32 | R_ARM_GLOB_DAT | R_ARM_JUMP_SLOT => {
                    let symbol = elf
                        .dynsyms
                        .get(relocation.r_sym)
                        .ok_or_else(|| EngineError::new("Invalid ELF relocation symbol"))?;
                    let name = elf
                        .dynstrtab
                        .get_at(symbol.st_name)
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| EngineError::new("Missing ELF import name"))?;
                    let resolved = match self.symbols.get(name) {
                        Some(address) => *address,
                        None => self.import(name, &symbol, area)?,
                    };
                    if relocation.r_type == R_ARM_ABS32 {
                        resolved.wrapping_add(addend)
                    } else {
                        resolved
                    }
                }
                kind => {
                    return Err(EngineError::new(format!(
                        "Unsupported relocation {kind} in {}",
                        library.name
                    )));
                }
            };
            write_u32(&mut self.machine.guest, address, value)?;
        }
        Ok(())
    }

    /// Gives an unresolved import an address: a trapping stub for a
    /// function or zeroed space for data.
    fn import(
        &mut self,
        name: &str,
        symbol: &Sym,
        area: &mut ImportArea,
    ) -> Result<u32, EngineError> {
        let is_data = symbol.st_type() == STT_OBJECT;
        let address = if is_data {
            area.data(symbol.st_size)?
        } else {
            let address = area.function()?;
            self.machine.write(address.into(), &UNDEFINED_INSTRUCTION)?;
            address
        };
        self.symbols.insert(name.to_owned(), address);
        self.imports.push(Import {
            name: name.to_owned(),
            address,
            is_data,
        });
        Ok(address)
    }

    /// Records the unwind table and the relocated `.init_array` entries.
    fn read_sections(
        &mut self,
        library: &Library,
        elf: &Elf<'_>,
        image_size: u64,
    ) -> Result<(), EngineError> {
        for section in &elf.section_headers {
            match elf.shdr_strtab.get_at(section.sh_name) {
                Some(".ARM.exidx") => self.unwind_tables.push(UnwindTable {
                    image_base: library.base,
                    address: library.base + section.sh_addr as u32,
                    entries: (section.sh_size / EXIDX_ENTRY_SIZE) as u32,
                }),
                Some(".init_array") => {
                    if !section.sh_size.is_multiple_of(4)
                        || section
                            .sh_addr
                            .checked_add(section.sh_size)
                            .is_none_or(|end| end > image_size)
                    {
                        return Err(EngineError::new("Invalid ELF initializer array"));
                    }
                    for offset in (0..section.sh_size).step_by(4) {
                        let entry = library.base + (section.sh_addr + offset) as u32;
                        let address = read_u32(&self.machine.guest, entry)?;
                        if address != 0 && address != u32::MAX {
                            self.initializers.push(address);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
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
