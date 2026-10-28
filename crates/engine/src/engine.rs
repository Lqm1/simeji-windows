//! Conversion through the bundled Simeji 23.6.1 engine.

use crate::{
    EngineError,
    arm::{read_c_string, read_u32, write_u32},
    elf::{IMPORT_AREA_SIZE, IMPORT_CODE_BASE, LibrarySet},
    host::{DICTIONARY_ROOT, HEAP_END, HEAP_START, Host, LEARNING_ROOT},
    user_dictionary::UserDictionary,
};
use simeji_protocol::{Candidate, Clause, Conversion};
use std::{cell::RefCell, collections::BTreeMap, path::Path, rc::Rc};

mod symbol {
    pub const SET_SHELL_PROPERTIES: &str = "SetJpImeShellProperties";
    pub const SET_SWITCH: &str = "SetSwitch";
    pub const INITIALIZE_DICTIONARY: &str = "_Z14InitializeDictPKcS0_S0_PFvjEPFvNSt6__ndk112basic_stringIcNS3_11char_traitsIcEENS3_9allocatorIcEEEEE";
    pub const INITIALIZE_KERNEL: &str = "_Z16InitializeKernelv";
    pub const INITIALIZE_ASSIST: &str =
        "_Z16InitializeAssistRKNSt6__ndk112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEEE";
    pub const STRING_INIT: &str =
        "_ZNSt6__ndk112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE6__initEPKcj";
    pub const STRING_DESTRUCTOR: &str =
        "_ZNSt6__ndk112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEED1Ev";
    pub const ROMAJI_TO_HIRAGANA: &str = "_ZN11jpimeassist20ConvRomajiToHiraganaERKNSt6__ndk112basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEEERS6_";
    pub const CANDIDATE_INFO_CONSTRUCTOR: &str = "_ZN14candidate_infoC2ENSt6__ndk112basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEEES6_jjjjjyjttS6_PNS0_6vectorIiNS4_IiEEEESA_PNS7_IS6_NS4_IS6_EEEESD_";
    pub const CANDIDATE_INFO_DESTRUCTOR: &str = "_ZN14candidate_infoD2Ev";
    pub const LEARN: &str = "Learn";
    pub const FLUSH_LEARNING: &str = "FlushLearn";
    pub const CUSTOM_TERM_CONSTRUCTOR: &str = "_ZN10CustomTermC2ERKNSt6__ndk112basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEEES8_yjt";
    pub const CUSTOM_TERM_DESTRUCTOR: &str = "_ZN10CustomTermD2Ev";
    pub const ADD_CUSTOM_TERM: &str = "CustomDictAdd";
    pub const SENTENCE_CANDIDATES: &str = "GetConvertedCandidateList";
    pub const CLAUSE_CANDIDATES: &str = "GetBunsetsuCandidateList";
    pub const FREE_CANDIDATES: &str = "FreeCandidateList";
}

/// `SetJpImeShellProperties` and `SetSwitch` arguments as Simeji 23.6.1 passes them.
const SHELL_PROPERTIES: [u32; 13] = [1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 1, 0];
const SWITCHES: u32 = 64 | 256 | 512 | 1024;

/// State that the Android shell's JNI layer normally initialises, located
/// through PC-relative literals of the exact 23.6.1 `libbaiduime.so`.
mod shell {
    pub const STATE_BASE: u32 = 0x320b_315e;
    pub const STATE_OFFSET: u32 = 0x320b_3290;
    pub const READY_FLAG_FIELDS: [u32; 2] = [0x0, 0x4];
    pub const HISTORY_FIELD: u32 = 0x10;
    pub const HISTORY_SIZE: u32 = 36;
    pub const READY_GOT_BASE: u32 = 0x320b_32fa;
    pub const READY_GOT_OFFSET: u32 = 0x320b_33fc;
}

/// libc++ `std::string` on ARM32. A clear low bit in the first byte marks a
/// short string with its length in the remaining bits and inline text.
mod std_string {
    pub const SIZE: u32 = 12;
    pub const LONG_FLAG: u8 = 1;
    pub const SHORT_TEXT: u32 = 1;
    pub const LONG_LENGTH: u32 = 4;
    pub const LONG_TEXT: u32 = 8;
}

mod candidate_list {
    pub const FAILED: u32 = u32::MAX;
    pub const STATUS: u32 = 0;
    pub const CLAUSE_COUNT: u32 = 4;
    pub const READING_CLAUSE_ENDS: u32 = 12;
    pub const TEXT_CLAUSE_ENDS: u32 = 16;
    pub const CANDIDATE_COUNT: u32 = 20;
    pub const CANDIDATES: u32 = 24;
    pub const ENTRY_SIZE: u32 = 48;
    pub const ENTRY_TEXT: u32 = 0;
    pub const ENTRY_READING: u32 = 20;
    pub const ENTRY_CONNECTION_IDS: u32 = 28;
    pub const ENTRY_PROPERTY: u32 = 36;
}

const CANDIDATE_INFO_SIZE: u32 = 160;
const CUSTOM_TERM_SIZE: u32 = 64;
const CALL_TIMEOUT_US: u64 = 30_000_000;
const NO_INSTRUCTION_LIMIT: usize = 0;
const MAX_INPUT_BYTES: usize = 4096;
const MAX_USER_WORD_BYTES: usize = 1024;
const MAX_TRANSLITERATION_BYTES: u32 = 16384;
const MAX_LIST_ENTRIES: u32 = 4096;

/// Where an engine reads its dictionaries and keeps the user's data.
#[derive(Clone, Copy, Debug)]
pub struct EnginePaths<'a> {
    /// The bundled dictionaries, opened read-only.
    pub dictionary: &'a Path,
    pub learning: &'a Path,
    /// The user dictionary entries, as JSON.
    pub user_dictionary: &'a Path,
}

/// One engine session. Not thread-safe: use it from a single worker thread.
pub struct Engine {
    native: LibrarySet,
    host: Rc<RefCell<Host>>,
    user_dictionary: UserDictionary,
    faulted: bool,
}

impl Engine {
    pub fn open(paths: EnginePaths<'_>) -> Result<Self, EngineError> {
        let mut native = LibrarySet::bundled()?;
        native
            .machine
            .map(HEAP_START.into(), (HEAP_END - HEAP_START).into())?;
        let host = Rc::new(RefCell::new(Host::new(
            paths.dictionary,
            paths.learning,
            native.unwind_tables.clone(),
        )?));
        trap_imports(&mut native, &host)?;
        let mut engine = Self {
            native,
            host,
            user_dictionary: UserDictionary::load(paths.user_dictionary)?,
            faulted: false,
        };
        engine.initialize()?;
        for entry in engine.user_dictionary.entries().to_vec() {
            engine.register_word(&entry.reading, &entry.text)?;
        }
        Ok(engine)
    }

    /// Whether a native call failed. The engine state is then undefined, so
    /// every further call fails until the engine is opened again.
    pub fn is_faulted(&self) -> bool {
        self.faulted
    }

    /// Romaji to hiragana with the engine's own table.
    pub fn transliterate(&mut self, romaji: &str) -> Result<String, EngineError> {
        if romaji.len() > MAX_INPUT_BYTES || romaji.contains('\0') {
            return Err(EngineError::new("Invalid romaji input"));
        }
        let input = self.new_std_string(romaji.as_bytes())?;
        let output = self.new_std_string(b"")?;
        self.call(symbol::ROMAJI_TO_HIRAGANA, &[input, output])?;
        let bytes = self.read_std_string(output)?;
        self.delete_std_string(input)?;
        self.delete_std_string(output)?;
        String::from_utf8(bytes).map_err(|_| EngineError::new("Invalid transliteration text"))
    }

    /// The best sentences for a reading; the first one is split into clauses.
    pub fn convert_sentence(&mut self, reading: &str) -> Result<Conversion, EngineError> {
        self.candidate_list(symbol::SENTENCE_CANDIDATES, reading, None)
    }

    /// Alternatives for one clause, given the text of the clauses before it.
    pub fn clause_candidates(
        &mut self,
        reading: &str,
        context: &str,
    ) -> Result<Vec<Candidate>, EngineError> {
        if context.len() > MAX_INPUT_BYTES || context.contains('\0') {
            return Err(EngineError::new("Invalid conversion context"));
        }
        Ok(self
            .candidate_list(symbol::CLAUSE_CANDIDATES, reading, Some(context))?
            .candidates)
    }

    /// Learns a committed candidate, optionally as following `previous`,
    /// and saves the learning files.
    pub fn learn(
        &mut self,
        candidate: &Candidate,
        previous: Option<&Candidate>,
    ) -> Result<(), EngineError> {
        let current = self.new_candidate_info(Some(candidate))?;
        let previous = self.new_candidate_info(previous)?;
        let context = self.new_std_string(b"")?;
        let result = self.call(symbol::LEARN, &[previous, current, 1, context]);
        self.call(symbol::CANDIDATE_INFO_DESTRUCTOR, &[current])?;
        self.call(symbol::CANDIDATE_INFO_DESTRUCTOR, &[previous])?;
        self.free(current)?;
        self.free(previous)?;
        self.delete_std_string(context)?;
        let code = result?;
        if (code as i32) < 0 {
            return Err(EngineError::new(format!("Learning failed: {code}")));
        }
        self.call(symbol::FLUSH_LEARNING, &[1])?;
        Ok(())
    }

    /// Adds a word to the native user dictionary and saves it.
    pub fn add_word(&mut self, reading: &str, text: &str) -> Result<(), EngineError> {
        self.register_word(reading, text)?;
        self.user_dictionary.insert(reading, text)
    }

    fn initialize(&mut self) -> Result<(), EngineError> {
        for address in self.native.initializers.clone() {
            self.call_address(address, &[])?;
        }
        self.call(symbol::SET_SHELL_PROPERTIES, &SHELL_PROPERTIES)?;
        self.call(symbol::SET_SWITCH, &[SWITCHES, 0])?;
        let dictionary = self.new_c_string(DICTIONARY_ROOT.as_bytes())?;
        let learning = self.new_c_string(LEARNING_ROOT.as_bytes())?;
        self.call(
            symbol::INITIALIZE_DICTIONARY,
            &[dictionary, learning, dictionary, 0, 0],
        )?;
        self.call(symbol::INITIALIZE_KERNEL, &[])?;
        let assist = self.new_std_string(DICTIONARY_ROOT.as_bytes())?;
        self.call(symbol::INITIALIZE_ASSIST, &[assist])?;
        self.delete_std_string(assist)?;
        self.free(dictionary)?;
        self.free(learning)?;
        self.initialize_shell_state()
    }

    fn initialize_shell_state(&mut self) -> Result<(), EngineError> {
        let history = self.host.borrow_mut().allocate(shell::HISTORY_SIZE)?;
        let guest = &mut self.native.machine.guest;
        let state = shell::STATE_BASE.wrapping_add(read_u32(guest, shell::STATE_OFFSET)?);
        guest.mem_write(history.into(), &[0; shell::HISTORY_SIZE as usize])?;
        write_u32(guest, state + shell::HISTORY_FIELD, history)?;
        for field in shell::READY_FLAG_FIELDS {
            write_u32(guest, state + field, 1)?;
        }
        let ready_slot =
            shell::READY_GOT_BASE.wrapping_add(read_u32(guest, shell::READY_GOT_OFFSET)?);
        let ready = read_u32(guest, ready_slot)?;
        guest.mem_write(ready.into(), &[1])?;
        Ok(())
    }

    fn register_word(&mut self, reading: &str, text: &str) -> Result<(), EngineError> {
        for value in [reading, text] {
            if value.is_empty() || value.len() > MAX_USER_WORD_BYTES || value.contains('\0') {
                return Err(EngineError::new("Invalid user dictionary entry"));
            }
        }
        let reading = self.new_std_string(reading.as_bytes())?;
        let text = self.new_std_string(text.as_bytes())?;
        let term = self.host.borrow_mut().allocate(CUSTOM_TERM_SIZE)?;
        self.call(
            symbol::CUSTOM_TERM_CONSTRUCTOR,
            &[term, reading, text, 0, 0, 0, 0, 0],
        )?;
        let result = self.call(symbol::ADD_CUSTOM_TERM, &[term]);
        self.call(symbol::CUSTOM_TERM_DESTRUCTOR, &[term])?;
        self.free(term)?;
        self.delete_std_string(reading)?;
        self.delete_std_string(text)?;
        let code = result?;
        if (code as i32) < 0 {
            return Err(EngineError::new(format!(
                "User dictionary registration failed: {code}"
            )));
        }
        Ok(())
    }

    fn call_address(&mut self, address: u32, arguments: &[u32]) -> Result<u32, EngineError> {
        if self.faulted {
            return Err(EngineError::new(
                "Engine must be reopened after a native execution failure",
            ));
        }
        let result = self.native.machine.call_with_limits(
            address,
            arguments,
            CALL_TIMEOUT_US,
            NO_INSTRUCTION_LIMIT,
        );
        if let Some(error) = self.host.borrow_mut().error.take() {
            self.faulted = true;
            return Err(error);
        }
        if result.is_err() {
            self.faulted = true;
        }
        result
    }

    fn call(&mut self, name: &str, arguments: &[u32]) -> Result<u32, EngineError> {
        let address = self.native.symbol(name)?;
        self.call_address(address, arguments)
    }

    fn new_c_string(&mut self, bytes: &[u8]) -> Result<u32, EngineError> {
        self.host
            .borrow_mut()
            .store_c_string(&mut self.native.machine.guest, bytes)
    }

    fn free(&mut self, address: u32) -> Result<(), EngineError> {
        self.host.borrow_mut().free(address)
    }

    fn new_std_string(&mut self, bytes: &[u8]) -> Result<u32, EngineError> {
        let string = self.host.borrow_mut().allocate(std_string::SIZE)?;
        self.native
            .machine
            .write(string.into(), &[0; std_string::SIZE as usize])?;
        let text = self.new_c_string(bytes)?;
        self.call(symbol::STRING_INIT, &[string, text, bytes.len() as u32])?;
        self.free(text)?;
        Ok(string)
    }

    fn delete_std_string(&mut self, string: u32) -> Result<(), EngineError> {
        self.call(symbol::STRING_DESTRUCTOR, &[string])?;
        self.free(string)
    }

    fn read_std_string(&self, string: u32) -> Result<Vec<u8>, EngineError> {
        let guest = &self.native.machine.guest;
        let header = self.native.machine.read(string.into(), 1)?[0];
        let (text, length) = if header & std_string::LONG_FLAG == 0 {
            (string + std_string::SHORT_TEXT, u32::from(header >> 1))
        } else {
            (
                read_u32(guest, string + std_string::LONG_TEXT)?,
                read_u32(guest, string + std_string::LONG_LENGTH)?,
            )
        };
        if length > MAX_TRANSLITERATION_BYTES {
            return Err(EngineError::new("Invalid transliteration length"));
        }
        self.native.machine.read(text.into(), length as usize)
    }

    /// A native `candidate_info` for learning; `None` gives an empty one.
    fn new_candidate_info(&mut self, candidate: Option<&Candidate>) -> Result<u32, EngineError> {
        let reading =
            self.new_std_string(candidate.map_or(b"".as_slice(), |c| c.reading.as_bytes()))?;
        let text = self.new_std_string(candidate.map_or(b"".as_slice(), |c| c.text.as_bytes()))?;
        let description = self.new_std_string(b"")?;
        let info = self.host.borrow_mut().allocate(CANDIDATE_INFO_SIZE)?;
        let property = candidate.map_or(0, |c| c.property);
        let right_id = u32::from(candidate.map_or(0, |c| c.right_id));
        let left_id = u32::from(candidate.map_or(0, |c| c.left_id));
        self.call(
            symbol::CANDIDATE_INFO_CONSTRUCTOR,
            &[
                info,
                reading,
                text,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                property,
                right_id,
                left_id,
                description,
                0,
                0,
                0,
                0,
            ],
        )?;
        self.delete_std_string(reading)?;
        self.delete_std_string(text)?;
        self.delete_std_string(description)?;
        Ok(info)
    }

    fn candidate_list(
        &mut self,
        function: &str,
        reading: &str,
        context: Option<&str>,
    ) -> Result<Conversion, EngineError> {
        if reading.is_empty() {
            return Ok(Conversion::default());
        }
        if reading.len() > MAX_INPUT_BYTES || reading.contains('\0') {
            return Err(EngineError::new("Invalid conversion reading"));
        }
        let text = self.new_c_string(reading.as_bytes())?;
        let previous = match context {
            Some(context) => self.new_c_string(context.as_bytes())?,
            None => 0,
        };
        let result = self.call(function, &[text, reading.len() as u32, previous]);
        self.free(text)?;
        self.free(previous)?;
        let list = result?;
        if list == 0 {
            return Err(EngineError::new("Engine returned a null candidate list"));
        }
        let conversion = self.read_candidate_list(list);
        self.call(symbol::FREE_CANDIDATES, &[list])?;
        conversion
    }

    fn read_candidate_list(&self, list: u32) -> Result<Conversion, EngineError> {
        use candidate_list::*;
        let guest = &self.native.machine.guest;
        if read_u32(guest, list + STATUS)? == FAILED {
            return Err(EngineError::new("Engine conversion failed"));
        }
        let count = read_u32(guest, list + CANDIDATE_COUNT)?;
        if count > MAX_LIST_ENTRIES {
            return Err(EngineError::new("Invalid candidate count"));
        }
        let entries = read_u32(guest, list + CANDIDATES)?;
        let mut candidates = Vec::new();
        for index in 0..count {
            let entry = entries + index * ENTRY_SIZE;
            let text = read_c_string(guest, read_u32(guest, entry + ENTRY_TEXT)?)?;
            let reading = read_c_string(guest, read_u32(guest, entry + ENTRY_READING)?)?;
            let connection_ids = read_u32(guest, entry + ENTRY_CONNECTION_IDS)?;
            candidates.push(Candidate {
                property: read_u32(guest, entry + ENTRY_PROPERTY)?,
                right_id: connection_ids as u16,
                left_id: (connection_ids >> 16) as u16,
                text: String::from_utf8(text)
                    .map_err(|_| EngineError::new("Invalid candidate text"))?,
                reading: String::from_utf8(reading)
                    .map_err(|_| EngineError::new("Invalid candidate reading"))?,
            });
        }
        let clauses = match candidates.first() {
            Some(sentence) => self.read_clauses(list, sentence)?,
            None => Vec::new(),
        };
        Ok(Conversion {
            candidates,
            clauses,
        })
    }

    /// Splits the first sentence at the UTF-16 clause ends the list reports.
    fn read_clauses(&self, list: u32, sentence: &Candidate) -> Result<Vec<Clause>, EngineError> {
        use candidate_list::*;
        let machine = &self.native.machine;
        let count = read_u32(&machine.guest, list + CLAUSE_COUNT)?;
        if count > MAX_LIST_ENTRIES {
            return Err(EngineError::new("Invalid clause count"));
        }
        let reading = sentence.reading.encode_utf16().collect::<Vec<_>>();
        let text = sentence.text.encode_utf16().collect::<Vec<_>>();
        let reading_ends = read_u32(&machine.guest, list + READING_CLAUSE_ENDS)?;
        let text_ends = read_u32(&machine.guest, list + TEXT_CLAUSE_ENDS)?;
        let read_end = |table: u32, index: u32| -> Result<usize, EngineError> {
            let bytes = machine.read(u64::from(table) + u64::from(index) * 2, 2)?;
            Ok(usize::from(u16::from_le_bytes([bytes[0], bytes[1]])))
        };
        let mut clauses = Vec::new();
        let (mut reading_start, mut text_start) = (0, 0);
        for index in 0..count {
            let reading_end = read_end(reading_ends, index)?;
            let text_end = read_end(text_ends, index)?;
            let clause_reading = reading
                .get(reading_start..reading_end)
                .ok_or_else(|| EngineError::new("Invalid reading clause boundary"))?;
            let clause_text = text
                .get(text_start..text_end)
                .ok_or_else(|| EngineError::new("Invalid text clause boundary"))?;
            clauses.push(Clause {
                reading: String::from_utf16(clause_reading)
                    .map_err(|_| EngineError::new("Invalid reading clause"))?,
                text: String::from_utf16(clause_text)
                    .map_err(|_| EngineError::new("Invalid text clause"))?,
            });
            reading_start = reading_end;
            text_start = text_end;
        }
        Ok(clauses)
    }
}

/// Routes every function import stub to the host.
fn trap_imports(native: &mut LibrarySet, host: &Rc<RefCell<Host>>) -> Result<(), EngineError> {
    let functions: BTreeMap<u64, String> = native
        .imports
        .iter()
        .filter(|import| !import.is_data)
        .map(|import| (u64::from(import.address), import.name.clone()))
        .collect();
    let host = host.clone();
    let last = IMPORT_CODE_BASE + IMPORT_AREA_SIZE - 1;
    native.machine.guest.add_code_hook(
        IMPORT_CODE_BASE.into(),
        last.into(),
        move |guest, address, _| {
            let Some(name) = functions.get(&address) else {
                return;
            };
            let mut host = host.borrow_mut();
            if let Err(error) = host.invoke(guest, name) {
                host.error = Some(error);
                let _ = guest.emu_stop();
            }
        },
    )?;
    Ok(())
}
