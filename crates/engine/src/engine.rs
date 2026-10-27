//! Conversion through the bundled Simeji 23.6.1 engine.

use crate::{
    arm::ArmError,
    elf::LibrarySet,
    host::{Host, string, word, write_word},
    user_dictionary::UserDictionary,
};
use simeji_protocol::{Candidate, Clause, Conversion};
use std::{cell::RefCell, collections::BTreeMap, path::Path, rc::Rc};

/// Owns one engine session. Use it on a dedicated worker thread.
pub struct Engine {
    libraries: LibrarySet,
    host: Rc<RefCell<Host>>,
    user_dictionary: UserDictionary,
    faulted: bool,
}

impl Engine {
    pub fn open(dictionary_directory: &Path, learning_directory: &Path) -> Result<Self, ArmError> {
        let mut libraries = LibrarySet::bundled()?;
        libraries.machine.map(0x1000_0000, 0x1000_0000)?;
        let host = Rc::new(RefCell::new(Host::new(
            dictionary_directory,
            learning_directory,
            libraries.unwind.clone(),
        )?));
        let names: BTreeMap<_, _> = libraries
            .imports
            .iter()
            .filter(|i| !i.is_data)
            .map(|i| (u64::from(i.address), i.name.clone()))
            .collect();
        let state = host.clone();
        libraries
            .machine
            .emulator
            .add_code_hook(0x7200_0000, 0x7200_ffff, move |u, pc, _| {
                let Some(name) = names.get(&pc) else {
                    return;
                };
                let mut host = state.borrow_mut();
                if let Err(error) = host.invoke(u, name) {
                    host.error = Some(error);
                    let _ = u.emu_stop();
                }
            })?;
        let mut engine = Self {
            libraries,
            host,
            user_dictionary: UserDictionary::load(learning_directory)?,
            faulted: false,
        };
        for address in engine.libraries.initializers.clone() {
            engine.call_at(address, &[])?;
        }
        engine.call(
            "SetJpImeShellProperties",
            &[1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 1, 0],
        )?;
        engine.call("SetSwitch", &[64 | 256 | 512 | 1024, 0])?;
        let dict = engine.put(b"/dict/")?;
        let learn = engine.put(b"/learn/")?;
        engine.call("_Z14InitializeDictPKcS0_S0_PFvjEPFvNSt6__ndk112basic_stringIcNS3_11char_traitsIcEENS3_9allocatorIcEEEEE",&[dict,learn,dict,0,0])?;
        engine.call("_Z16InitializeKernelv", &[])?;
        let cpp_path = engine.cpp_string(b"/dict/")?;
        engine.call("_Z16InitializeAssistRKNSt6__ndk112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEEE",&[cpp_path])?;
        engine.drop_cpp_string(cpp_path)?;
        engine.release(dict)?;
        engine.release(learn)?;
        // These offsets belong to the exact embedded 23.6.1 shell ABI.
        // Initialize its history object and readiness flags without Android JNI.
        let u = &mut engine.libraries.machine.emulator;
        let state = 0x320b_315eu32.wrapping_add(word(u, 0x320b_3290)?);
        let history = engine.host.borrow_mut().alloc(36)?;
        u.mem_write(u64::from(history), &[0; 36])?;
        write_word(u, state + 0x10, history)?;
        write_word(u, state, 1)?;
        write_word(u, state + 4, 1)?;
        let got = 0x320b_32fau32.wrapping_add(word(u, 0x320b_33fc)?);
        let ready = word(u, got)?;
        u.mem_write(u64::from(ready), &[1])?;
        for entry in engine.user_dictionary.entries().to_vec() {
            engine.register_word(&entry.reading, &entry.text)?;
        }
        Ok(engine)
    }
    fn call_at(&mut self, address: u32, args: &[u32]) -> Result<u32, ArmError> {
        if self.faulted {
            return Err(ArmError::new(
                "Engine must be reopened after a native execution failure",
            ));
        }
        let result = self
            .libraries
            .machine
            .call_with_limits(address, args, 30_000_000, 0);
        if let Some(error) = self.host.borrow_mut().error.take() {
            self.faulted = true;
            return Err(error);
        }
        if result.is_err() {
            self.faulted = true;
        }
        result
    }

    pub fn is_faulted(&self) -> bool {
        self.faulted
    }
    fn call(&mut self, name: &str, args: &[u32]) -> Result<u32, ArmError> {
        let address = self.libraries.symbol(name)?;
        self.call_at(address, args)
    }
    fn put(&mut self, b: &[u8]) -> Result<u32, ArmError> {
        self.host
            .borrow_mut()
            .put(&mut self.libraries.machine.emulator, b)
    }
    fn release(&mut self, p: u32) -> Result<(), ArmError> {
        self.host.borrow_mut().release(p)
    }
    fn cpp_string(&mut self, b: &[u8]) -> Result<u32, ArmError> {
        let p = self.host.borrow_mut().alloc(12)?;
        self.libraries.machine.write(u64::from(p), &[0; 12])?;
        let text = self.put(b)?;
        self.call(
            "_ZNSt6__ndk112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE6__initEPKcj",
            &[p, text, b.len() as u32],
        )?;
        self.release(text)?;
        Ok(p)
    }
    fn drop_cpp_string(&mut self, p: u32) -> Result<(), ArmError> {
        self.call(
            "_ZNSt6__ndk112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEED1Ev",
            &[p],
        )?;
        self.release(p)
    }

    /// Uses the engine's romaji mapping for the composition text.
    pub fn transliterate(&mut self, romaji: &str) -> Result<String, ArmError> {
        if romaji.len() > 4096 || romaji.contains('\0') {
            return Err(ArmError::new("Invalid romaji input"));
        }
        let input = self.cpp_string(romaji.as_bytes())?;
        let output = self.cpp_string(b"")?;
        self.call("_ZN11jpimeassist20ConvRomajiToHiraganaERKNSt6__ndk112basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEEERS6_",&[input,output])?;
        let u = &self.libraries.machine.emulator;
        let header = u.mem_read_as_vec(u64::from(output), 1)?[0];
        let (p, len) = if header & 1 == 0 {
            (output + 1, u32::from(header >> 1))
        } else {
            (word(u, output + 8)?, word(u, output + 4)?)
        };
        if len > 16384 {
            return Err(ArmError::new("Invalid transliteration length"));
        }
        let bytes = u.mem_read_as_vec(u64::from(p), len as usize)?;
        self.drop_cpp_string(input)?;
        self.drop_cpp_string(output)?;
        String::from_utf8(bytes).map_err(|_| ArmError::new("Invalid transliteration text"))
    }

    fn learning_candidate(&mut self, candidate: Option<&Candidate>) -> Result<u32, ArmError> {
        let reading =
            self.cpp_string(candidate.map_or(b"".as_slice(), |c| c.reading.as_bytes()))?;
        let text = self.cpp_string(candidate.map_or(b"".as_slice(), |c| c.text.as_bytes()))?;
        let description = self.cpp_string(b"")?;
        let p = self.host.borrow_mut().alloc(160)?;
        self.call("_ZN14candidate_infoC2ENSt6__ndk112basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEEES6_jjjjjyjttS6_PNS0_6vectorIiNS4_IiEEEESA_PNS7_IS6_NS4_IS6_EEEESD_",&[p,reading,text,0,0,0,0,0,0,0,candidate.map_or(0,|c|c.property),u32::from(candidate.map_or(0,|c|c.right_id)),u32::from(candidate.map_or(0,|c|c.left_id)),description,0,0,0,0])?;
        self.drop_cpp_string(reading)?;
        self.drop_cpp_string(text)?;
        self.drop_cpp_string(description)?;
        Ok(p)
    }

    /// Learns an accepted native candidate and saves the engine's learning files.
    pub fn learn(
        &mut self,
        candidate: &Candidate,
        previous: Option<&Candidate>,
    ) -> Result<(), ArmError> {
        let current = self.learning_candidate(Some(candidate))?;
        let previous = self.learning_candidate(previous)?;
        let context = self.cpp_string(b"")?;
        let result = self.call("Learn", &[previous, current, 1, context]);
        self.call("_ZN14candidate_infoD2Ev", &[current])?;
        self.call("_ZN14candidate_infoD2Ev", &[previous])?;
        self.release(current)?;
        self.release(previous)?;
        self.drop_cpp_string(context)?;
        let code = result?;
        if (code as i32) < 0 {
            return Err(ArmError::new(format!("Learning failed: {code}")));
        }
        self.call("FlushLearn", &[1])?;
        Ok(())
    }

    /// Registers a word in the native user dictionary and saves it.
    pub fn add_word(&mut self, reading: &str, text: &str) -> Result<(), ArmError> {
        self.register_word(reading, text)?;
        self.user_dictionary.insert(reading, text)
    }

    fn register_word(&mut self, reading: &str, text: &str) -> Result<(), ArmError> {
        for value in [reading, text] {
            if value.is_empty() || value.len() > 1024 || value.contains('\0') {
                return Err(ArmError::new("Invalid user dictionary entry"));
            }
        }
        let reading = self.cpp_string(reading.as_bytes())?;
        let text = self.cpp_string(text.as_bytes())?;
        let p = self.host.borrow_mut().alloc(64)?;
        self.call("_ZN10CustomTermC2ERKNSt6__ndk112basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEEES8_yjt",&[p,reading,text,0,0,0,0,0])?;
        let result = self.call("CustomDictAdd", &[p]);
        self.call("_ZN10CustomTermD2Ev", &[p])?;
        self.release(p)?;
        self.drop_cpp_string(reading)?;
        self.drop_cpp_string(text)?;
        let code = result?;
        if (code as i32) < 0 {
            return Err(ArmError::new(format!(
                "User dictionary registration failed: {code}"
            )));
        }
        Ok(())
    }

    /// Converts a reading using the original native dictionary and scoring.
    pub fn convert(&mut self, reading: &str) -> Result<Vec<Candidate>, ArmError> {
        Ok(self.convert_sentence(reading)?.candidates)
    }

    pub fn convert_sentence(&mut self, reading: &str) -> Result<Conversion, ArmError> {
        self.request("GetConvertedCandidateList", reading, None)
    }

    /// Returns alternatives for a clause, taking the preceding text as context.
    pub fn clause_candidates(
        &mut self,
        reading: &str,
        context: &str,
    ) -> Result<Vec<Candidate>, ArmError> {
        if context.len() > 4096 || context.contains('\0') {
            return Err(ArmError::new("Invalid conversion context"));
        }
        Ok(self
            .request("GetBunsetsuCandidateList", reading, Some(context))?
            .candidates)
    }

    fn request(
        &mut self,
        function: &str,
        reading: &str,
        context: Option<&str>,
    ) -> Result<Conversion, ArmError> {
        if reading.is_empty() {
            return Ok(Conversion::default());
        }
        if reading.len() > 4096 || reading.contains('\0') {
            return Err(ArmError::new("Invalid conversion reading"));
        }
        let text = self.put(reading.as_bytes())?;
        let previous = match context {
            Some(context) => self.put(context.as_bytes())?,
            None => 0,
        };
        let result = self.call(function, &[text, reading.len() as u32, previous]);
        self.release(text)?;
        if previous != 0 {
            self.release(previous)?;
        }
        let result = result?;
        if result == 0 {
            return Err(ArmError::new("Engine returned a null candidate list"));
        }
        let copied = self.copy_candidates(result);
        self.call("FreeCandidateList", &[result])?;
        copied
    }
    fn copy_candidates(&self, result: u32) -> Result<Conversion, ArmError> {
        let u = &self.libraries.machine.emulator;
        if word(u, result)? == u32::MAX {
            return Err(ArmError::new("Engine conversion failed"));
        }
        let count = word(u, result + 20)?;
        if count > 4096 {
            return Err(ArmError::new("Invalid candidate count"));
        }
        let array = word(u, result + 24)?;
        let mut candidates = Vec::new();
        for i in 0..count {
            let p = array + i * 48;
            let text = string(u, word(u, p)?)?;
            let reading = string(u, word(u, p + 20)?)?;
            candidates.push(Candidate {
                property: word(u, p + 36)?,
                right_id: word(u, p + 28)? as u16,
                left_id: (word(u, p + 28)? >> 16) as u16,
                text: String::from_utf8(text)
                    .map_err(|_| ArmError::new("Invalid candidate text"))?,
                reading: String::from_utf8(reading)
                    .map_err(|_| ArmError::new("Invalid candidate reading"))?,
            });
        }
        let mut clauses = Vec::new();
        if let Some(sentence) = candidates.first() {
            let count = word(u, result + 4)?;
            if count > 4096 {
                return Err(ArmError::new("Invalid clause count"));
            }
            let reading = sentence.reading.encode_utf16().collect::<Vec<_>>();
            let text = sentence.text.encode_utf16().collect::<Vec<_>>();
            let reading_offsets = word(u, result + 12)?;
            let text_offsets = word(u, result + 16)?;
            let (mut read_start, mut text_start) = (0, 0);
            for i in 0..count {
                let r = u.mem_read_as_vec(u64::from(reading_offsets) + u64::from(i) * 2, 2)?;
                let t = u.mem_read_as_vec(u64::from(text_offsets) + u64::from(i) * 2, 2)?;
                let read_end = usize::from(u16::from_le_bytes([r[0], r[1]]));
                let text_end = usize::from(u16::from_le_bytes([t[0], t[1]]));
                let r = reading
                    .get(read_start..read_end)
                    .ok_or_else(|| ArmError::new("Invalid reading clause boundary"))?;
                let t = text
                    .get(text_start..text_end)
                    .ok_or_else(|| ArmError::new("Invalid text clause boundary"))?;
                clauses.push(Clause {
                    reading: String::from_utf16(r)
                        .map_err(|_| ArmError::new("Invalid reading clause"))?,
                    text: String::from_utf16(t)
                        .map_err(|_| ArmError::new("Invalid text clause"))?,
                });
                read_start = read_end;
                text_start = text_end;
            }
        }
        Ok(Conversion {
            candidates,
            clauses,
        })
    }
}
