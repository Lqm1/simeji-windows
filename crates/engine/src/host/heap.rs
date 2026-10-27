//! The guest heap: first-fit allocation with coalescing free blocks.

use crate::EngineError;
use std::collections::BTreeMap;

pub(crate) const HEAP_START: u32 = 0x1000_0000;
pub(crate) const HEAP_END: u32 = 0x2000_0000;
/// The first page of the heap holds `errno`.
pub(super) const ERRNO_ADDRESS: u32 = HEAP_START;
const FIRST_ALLOCATION: u32 = HEAP_START + 4096;
const MIN_BLOCK: u32 = 16;
const ALIGNMENT: u32 = 16;

pub(super) struct Heap {
    end: u32,
    allocations: BTreeMap<u32, u32>,
    free_blocks: BTreeMap<u32, u32>,
}

impl Heap {
    pub fn new() -> Self {
        Self {
            end: FIRST_ALLOCATION,
            allocations: BTreeMap::new(),
            free_blocks: BTreeMap::new(),
        }
    }

    pub fn allocate(&mut self, size: u32) -> Result<u32, EngineError> {
        let size = size
            .max(MIN_BLOCK)
            .checked_add(ALIGNMENT - 1)
            .map(|size| size & !(ALIGNMENT - 1))
            .ok_or_else(|| EngineError::new("Guest allocation overflow"))?;
        let reusable = self
            .free_blocks
            .iter()
            .find(|(_, block_size)| **block_size >= size)
            .map(|(address, block_size)| (*address, *block_size));
        let address = if let Some((address, block_size)) = reusable {
            self.free_blocks.remove(&address);
            if block_size > size {
                self.free_blocks.insert(address + size, block_size - size);
            }
            address
        } else {
            let address = self.end;
            self.end = self
                .end
                .checked_add(size)
                .filter(|end| *end <= HEAP_END)
                .ok_or_else(|| EngineError::new("Guest heap exhausted"))?;
            address
        };
        self.allocations.insert(address, size);
        Ok(address)
    }

    /// Freeing the null address does nothing, as in C.
    pub fn free(&mut self, address: u32) -> Result<(), EngineError> {
        if address == 0 {
            return Ok(());
        }
        let mut size = self
            .allocations
            .remove(&address)
            .ok_or_else(|| EngineError::new(format!("Invalid guest free: {address:#x}")))?;
        let mut start = address;
        if let Some((&before, &before_size)) = self.free_blocks.range(..address).next_back()
            && before + before_size == address
        {
            start = before;
            size += before_size;
            self.free_blocks.remove(&before);
        }
        if let Some(after_size) = self.free_blocks.remove(&(start + size)) {
            size += after_size;
        }
        self.free_blocks.insert(start, size);
        Ok(())
    }

    pub fn size_of(&self, address: u32) -> Option<u32> {
        self.allocations.get(&address).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reclaims_and_coalesces_guest_allocations() {
        let mut heap = Heap::new();
        let first = heap.allocate(32).unwrap();
        let second = heap.allocate(64).unwrap();
        let third = heap.allocate(16).unwrap();
        heap.free(second).unwrap();
        heap.free(first).unwrap();
        heap.free(third).unwrap();
        assert_eq!(heap.allocate(112).unwrap(), first);
        assert!(heap.free(second).is_err());
        assert!(heap.allocate(u32::MAX).is_err());
    }
}
