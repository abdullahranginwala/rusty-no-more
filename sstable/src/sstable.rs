use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};

// ──────────────────────────────────────────────
//  FILE FORMAT
// ──────────────────────────────────────────────
//
//  ┌──────────────────────────────────┐
//  │         DATA SECTION             │
//  │  ┌────────────────────────────┐  │
//  │  │ key_len (4 bytes, LE)      │  │
//  │  │ value_len (4 bytes, LE)    │  │
//  │  │ key (key_len bytes)        │  │
//  │  │ value (value_len bytes)    │  │
//  │  └────────────────────────────┘  │
//  │  ... repeated for each entry ... │
//  ├──────────────────────────────────┤
//  │         INDEX SECTION            │
//  │  ┌────────────────────────────┐  │
//  │  │ key_len (4 bytes, LE)      │  │
//  │  │ offset (8 bytes, LE)       │  │
//  │  │ key (key_len bytes)        │  │
//  │  └────────────────────────────┘  │
//  │  ... repeated for each entry ... │
//  ├──────────────────────────────────┤
//  │         FOOTER (12 bytes)        │
//  │  index_offset (8 bytes, LE)      │
//  │  entry_count  (4 bytes, LE)      │
//  └──────────────────────────────────┘

const FOOTER_SIZE: usize = 12; // 8 (index_offset) + 4 (entry_count)

// ──────────────────────────────────────────────
//  WRITER
// ──────────────────────────────────────────────

/// Writes sorted key-value pairs to a new SSTable file.
///
/// IMPORTANT: keys must be written in sorted (ascending) order.
/// The writer does not sort for you — that's the memtable's job.
pub struct SSTableWriter {
    writer: BufWriter<File>,
    /// Stores (key, offset) for every entry written — used to build the index.
    index: Vec<(Vec<u8>, u64)>,
    /// Current write position in the file.
    offset: u64,
}

impl SSTableWriter {
    pub fn new(path: &str) -> io::Result<Self> {
        let file = File::create(path)?;
        Ok(SSTableWriter {
            writer: BufWriter::new(file),
            index: Vec::new(),
            offset: 0,
        })
    }

    /// Write a single key-value entry. Keys MUST be in sorted order.
    pub fn write(&mut self, key: &[u8], value: &[u8]) -> io::Result<()> {
        // Remember where this entry starts (for the index)
        let entry_offset = self.offset;

        // Write: key_len (4 bytes) | value_len (4 bytes) | key | value
        let key_len = key.len() as u32;
        let value_len = value.len() as u32;

        self.writer.write_all(&key_len.to_le_bytes())?;
        self.writer.write_all(&value_len.to_le_bytes())?;
        self.writer.write_all(key)?;
        self.writer.write_all(value)?;

        self.offset += 4 + 4 + key.len() as u64 + value.len() as u64;

        // Save to our in-memory index
        self.index.push((key.to_vec(), entry_offset));

        Ok(())
    }

    /// Finalize the SSTable: write the index section and footer.
    pub fn finish(mut self) -> io::Result<()> {
        let index_offset = self.offset;
        let entry_count = self.index.len() as u32;

        // Write index section: for each entry, store key_len | offset | key
        for (key, offset) in &self.index {
            let key_len = key.len() as u32;
            self.writer.write_all(&key_len.to_le_bytes())?;
            self.writer.write_all(&offset.to_le_bytes())?;
            self.writer.write_all(key)?;
        }

        // Write footer: index_offset (8 bytes) | entry_count (4 bytes)
        self.writer.write_all(&index_offset.to_le_bytes())?;
        self.writer.write_all(&entry_count.to_le_bytes())?;

        self.writer.flush()?;
        Ok(())
    }
}

// ──────────────────────────────────────────────
//  READER
// ──────────────────────────────────────────────

/// An index entry: maps a key to its byte offset in the data section.
struct IndexEntry {
    key: Vec<u8>,
    offset: u64,
}

/// Reads key-value pairs from an SSTable file on disk.
///
/// On open, it reads the footer and full index into memory.
/// Individual lookups then seek directly to the right offset.
pub struct SSTableReader {
    path: String,
    index: Vec<IndexEntry>,
}

impl SSTableReader {
    /// Open an SSTable file: reads the footer and index into memory.
    pub fn open(path: &str) -> io::Result<Self> {
        let mut file = BufReader::new(File::open(path)?);

        // Step 1: Read the footer (last 12 bytes of the file)
        file.seek(SeekFrom::End(-(FOOTER_SIZE as i64)))?;

        let mut buf8 = [0u8; 8];
        let mut buf4 = [0u8; 4];

        file.read_exact(&mut buf8)?;
        let index_offset = u64::from_le_bytes(buf8);

        file.read_exact(&mut buf4)?;
        let entry_count = u32::from_le_bytes(buf4) as usize;

        // Step 2: Seek to the index section and read all index entries
        file.seek(SeekFrom::Start(index_offset))?;

        let mut index = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            file.read_exact(&mut buf4)?;
            let key_len = u32::from_le_bytes(buf4) as usize;

            file.read_exact(&mut buf8)?;
            let offset = u64::from_le_bytes(buf8);

            let mut key = vec![0u8; key_len];
            file.read_exact(&mut key)?;

            index.push(IndexEntry { key, offset });
        }

        Ok(SSTableReader {
            path: path.to_string(),
            index,
        })
    }

    /// Look up a key using binary search on the index, then read from disk.
    pub fn get(&self, key: &[u8]) -> io::Result<Option<Vec<u8>>> {
        // Binary search the in-memory index for the key
        let result = self.index.binary_search_by(|entry| entry.key.as_slice().cmp(key));

        match result {
            Ok(pos) => {
                // Found it — seek to the offset and read the value
                let offset = self.index[pos].offset;
                self.read_value_at(offset)
            }
            Err(_) => Ok(None), // Key not found
        }
    }

    /// Scan all entries where start_key <= key <= end_key.
    pub fn scan(&self, start_key: &[u8], end_key: &[u8]) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut results = Vec::new();

        // Find the first index entry >= start_key
        let start_pos = match self.index.binary_search_by(|e| e.key.as_slice().cmp(start_key)) {
            Ok(pos) => pos,
            Err(pos) => pos, // insertion point = first key > start_key
        };

        let mut file = BufReader::new(File::open(&self.path)?);
        let mut buf4 = [0u8; 4];

        for entry in &self.index[start_pos..] {
            if entry.key.as_slice() > end_key {
                break;
            }

            // Read the full key-value pair from disk
            file.seek(SeekFrom::Start(entry.offset))?;

            file.read_exact(&mut buf4)?;
            let key_len = u32::from_le_bytes(buf4) as usize;

            file.read_exact(&mut buf4)?;
            let value_len = u32::from_le_bytes(buf4) as usize;

            let mut key = vec![0u8; key_len];
            file.read_exact(&mut key)?;

            let mut value = vec![0u8; value_len];
            file.read_exact(&mut value)?;

            results.push((key, value));
        }

        Ok(results)
    }

    /// Read the value at a specific byte offset in the data section.
    fn read_value_at(&self, offset: u64) -> io::Result<Option<Vec<u8>>> {
        let mut file = BufReader::new(File::open(&self.path)?);
        file.seek(SeekFrom::Start(offset))?;

        let mut buf4 = [0u8; 4];

        // Read key_len
        file.read_exact(&mut buf4)?;
        let key_len = u32::from_le_bytes(buf4) as usize;

        // Read value_len
        file.read_exact(&mut buf4)?;
        let value_len = u32::from_le_bytes(buf4) as usize;

        // Skip over the key bytes
        file.seek(SeekFrom::Current(key_len as i64))?;

        // Read the value
        let mut value = vec![0u8; value_len];
        file.read_exact(&mut value)?;

        Ok(Some(value))
    }
}
