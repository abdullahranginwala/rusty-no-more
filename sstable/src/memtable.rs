use std::collections::BTreeMap;
use std::io;

use crate::SSTableWriter;

/// An in-memory sorted key-value store that can be flushed to an SSTable.
///
/// Uses a BTreeMap internally, so keys are always sorted. This is the
/// "write buffer" in an LSM-tree — all writes go here first, and when
/// it gets big enough, you flush it to disk as an immutable SSTable.
pub struct MemTable {
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
    size_bytes: usize,
}

impl MemTable {
    pub fn new() -> Self {
        MemTable {
            entries: BTreeMap::new(),
            size_bytes: 0,
        }
    }

    /// Insert or overwrite a key-value pair.
    pub fn put(&mut self, key: &[u8], value: &[u8]) {
        // If key already exists, subtract old size
        if let Some(old_value) = self.entries.get(key) {
            self.size_bytes -= key.len() + old_value.len();
        }
        self.size_bytes += key.len() + value.len();
        self.entries.insert(key.to_vec(), value.to_vec());
    }

    /// Look up a key. Returns None if not found.
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.entries.get(key).map(|v| v.as_slice())
    }

    /// Remove a key.
    pub fn delete(&mut self, key: &[u8]) {
        if let Some(old_value) = self.entries.remove(key) {
            self.size_bytes -= key.len() + old_value.len();
        }
    }

    /// How many bytes of key+value data are stored.
    pub fn size(&self) -> usize {
        self.size_bytes
    }

    /// How many entries are stored.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Flush all entries to disk as a new SSTable, then clear the memtable.
    /// The BTreeMap guarantees keys come out in sorted order — exactly
    /// what the SSTable writer expects.
    pub fn flush_to_sstable(&mut self, path: &str) -> io::Result<()> {
        let mut writer = SSTableWriter::new(path)?;

        for (key, value) in &self.entries {
            writer.write(key, value)?;
        }

        writer.finish()?;

        // Clear the memtable after successful flush
        self.entries.clear();
        self.size_bytes = 0;

        Ok(())
    }
}
