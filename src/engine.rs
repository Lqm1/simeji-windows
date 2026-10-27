//! Conversion through the bundled Simeji 23.6.1 engine.

use crate::{
    arm::ArmError,
    elf::LibrarySet,
    host::{Host, string, word, write_word},
};
use std::{cell::RefCell, collections::BTreeMap, path::Path, rc::Rc};

/// A conversion result copied out of guest memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub reading: String,
}

/// Owns one engine session. Use it on a dedicated worker thread.
pub struct Engine {
    libraries: LibrarySet,
    host: Rc<RefCell<Host>>,
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
        let mut engine = Self { libraries, host };
        for address in engine.libraries.initializers.clone() {
            engine.call_at(address, &[])?;
        }
        engine.call(
            "SetJpImeShellProperties",
            &[1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 1, 0],
        )?;
        engine.call("SetSwitch", &[0, 0])?;
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
        Ok(engine)
    }
    fn call_at(&mut self, address: u32, args: &[u32]) -> Result<u32, ArmError> {
        let result = self
            .libraries
            .machine
            .call_with_limits(address, args, 30_000_000, 0);
        if let Some(error) = self.host.borrow_mut().error.take() {
            return Err(error);
        }
        result
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

    /// Converts a reading using the original native dictionary and scoring.
    pub fn convert(&mut self, reading: &str) -> Result<Vec<Candidate>, ArmError> {
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
        self.request("GetBunsetsuCandidateList", reading, Some(context))
    }

    fn request(
        &mut self,
        function: &str,
        reading: &str,
        context: Option<&str>,
    ) -> Result<Vec<Candidate>, ArmError> {
        if reading.is_empty() {
            return Ok(Vec::new());
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
    fn copy_candidates(&self, result: u32) -> Result<Vec<Candidate>, ArmError> {
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
            let reading = string(u, word(u, p + 16)?)?;
            candidates.push(Candidate {
                text: String::from_utf8(text)
                    .map_err(|_| ArmError::new("Invalid candidate text"))?,
                reading: String::from_utf8(reading)
                    .map_err(|_| ArmError::new("Invalid candidate reading"))?,
            });
        }
        Ok(candidates)
    }
}
