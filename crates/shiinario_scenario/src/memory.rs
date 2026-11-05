//! Fixed-size shared storage for host resources directly addressed by SCN.
use anyhow::{Result, ensure};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// Hosts and the VM use these regions on the same thread. No borrowed storage
/// escapes an operation, and the size cannot change while pointers are mapped.
#[derive(Clone)]
pub struct SharedMemory(Rc<(RefCell<Vec<u8>>, Cell<u64>)>);
impl SharedMemory {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= 16 * 1024 * 1024,
            "shared memory size exceeds bounds"
        );
        Ok(Self(Rc::new((RefCell::new(bytes), Cell::new(0)))))
    }
    pub fn zeroed(length: usize) -> Result<Self> {
        ensure!(
            length > 0 && length <= 16 * 1024 * 1024,
            "shared memory size exceeds bounds"
        );
        Ok(Self(Rc::new((RefCell::new(vec![0; length]), Cell::new(0)))))
    }
    pub fn revision(&self) -> u64 {
        self.0.1.get()
    }
    pub fn len(&self) -> usize {
        self.0.0.borrow().len()
    }
    pub fn is_empty(&self) -> bool {
        false
    }
    pub fn same_region(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
    pub fn read(&self, offset: usize, length: usize) -> Result<Vec<u8>> {
        let bytes = self.0.0.borrow();
        ensure!(
            offset <= bytes.len() && length <= bytes.len() - offset,
            "shared memory read outside region"
        );
        Ok(bytes[offset..offset + length].to_vec())
    }
    pub fn write(&self, offset: usize, source: &[u8]) -> Result<()> {
        let mut bytes = self.0.0.borrow_mut();
        ensure!(
            offset <= bytes.len() && source.len() <= bytes.len() - offset,
            "shared memory write outside region"
        );
        bytes[offset..offset + source.len()].copy_from_slice(source);
        self.0.1.set(self.0.1.get().wrapping_add(1));
        Ok(())
    }
}
