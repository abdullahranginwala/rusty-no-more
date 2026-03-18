# SSTables — Sorted String Tables

## What is an SSTable?

An SSTable (Sorted String Table) is an **immutable, on-disk file** that stores key-value pairs **sorted by key**. It's one of the most important data structures in modern databases and storage engines.

SSTables are the backbone of **LSM-tree** (Log-Structured Merge-Tree) based systems like:
- Google's **LevelDB** / **BigTable**
- Facebook's **RocksDB**
- Apache **Cassandra**
- ScyllaDB, CockroachDB, and many more

The core idea: **never modify data in place on disk**. Instead, buffer writes in memory, then flush them as a sorted, immutable file. This turns random writes into sequential writes, which is *much* faster for disks.

---

## The Big Picture: How Writes and Reads Work

```
   WRITE PATH                              READ PATH
   ──────────                              ─────────

   put("dog", "loyal")                     get("dog")
         │                                      │
         ▼                                      ▼
   ┌───────────┐                          ┌───────────┐
   │  MemTable  │  ◄── in-memory,         │  MemTable  │  ◄── check here first
   │ (BTreeMap) │      sorted             │ (BTreeMap) │      (fastest)
   └─────┬─────┘                          └─────┬─────┘
         │ when full,                           │ not found?
         │ flush to disk                        ▼
         ▼                                ┌───────────┐
   ┌───────────┐                          │ SSTable 3  │  ◄── newest on disk
   │  SSTable   │  ◄── immutable,         ├───────────┤
   │  (file)    │      sorted             │ SSTable 2  │
   └───────────┘                          ├───────────┤
                                          │ SSTable 1  │  ◄── oldest
                                          └───────────┘
                                          search newest → oldest
```

**Write path:**
1. All writes go to the **MemTable** (an in-memory sorted structure).
2. When the MemTable reaches a size threshold, it's **flushed** to disk as a new SSTable.
3. The SSTable is immutable — once written, it's never modified.

**Read path:**
1. Check the **MemTable** first (it has the newest data).
2. If not found, search SSTables from **newest to oldest**.
3. Within each SSTable, use the **index** to find the key quickly via binary search.

---

## Our SSTable File Format

Here's exactly how our SSTable file is laid out on disk:

```
BYTE OFFSET    CONTENTS
──────────────────────────────────────────────────────

               ┌─── DATA SECTION ───────────────────┐
0x0000         │                                     │
               │  Entry 0:                           │
               │    key_len:   4 bytes (little-endian u32)
               │    value_len: 4 bytes (little-endian u32)
               │    key:       key_len bytes          │
               │    value:     value_len bytes        │
               │                                     │
               │  Entry 1:                           │
               │    key_len | value_len | key | value │
               │                                     │
               │  ... more entries ...               │
               │                                     │
               ├─── INDEX SECTION ──────────────────┤
index_offset   │                                     │
               │  Index Entry 0:                     │
               │    key_len:  4 bytes (u32)          │
               │    offset:   8 bytes (u64)  ────────┼──► points to Entry 0
               │    key:      key_len bytes          │       in data section
               │                                     │
               │  Index Entry 1:                     │
               │    key_len | offset | key   ────────┼──► points to Entry 1
               │                                     │
               │  ... more index entries ...         │
               │                                     │
               ├─── FOOTER (last 12 bytes) ─────────┤
               │  index_offset: 8 bytes (u64)        │
               │  entry_count:  4 bytes (u32)        │
               └─────────────────────────────────────┘
```

**Three sections:**
1. **Data Section** — the actual key-value pairs, laid out sequentially.
2. **Index Section** — for each key, stores the key and its byte offset into the data section.
3. **Footer** — the last 12 bytes of the file. Tells us where the index starts and how many entries there are.

**Why this layout?** Reading starts from the end:
1. Read the last 12 bytes → you know where the index is and how big it is.
2. Seek to the index → load it into memory.
3. Binary search the index for any key → get its offset.
4. Seek to that offset → read the value directly.

This means **looking up any key is O(log n)** — binary search on the index — plus two disk seeks.

---

## The Code, Piece by Piece

### 1. MemTable (`memtable.rs`)

The MemTable is the write buffer. It's just a `BTreeMap` wrapper.

```rust
pub struct MemTable {
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
    size_bytes: usize,
}
```

**Why BTreeMap?** Because it keeps keys sorted automatically. When we flush to disk, we iterate in order and get sorted output for free. A HashMap would be faster for lookups, but we'd need to sort before flushing.

**Key operations:**

```rust
// Insert — O(log n). BTreeMap handles the sorting.
pub fn put(&mut self, key: &[u8], value: &[u8]) {
    self.entries.insert(key.to_vec(), value.to_vec());
}

// Lookup — O(log n). Straightforward BTreeMap lookup.
pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
    self.entries.get(key).map(|v| v.as_slice())
}

// Flush — iterate in sorted order, write each entry, clear.
pub fn flush_to_sstable(&mut self, path: &str) -> io::Result<()> {
    let mut writer = SSTableWriter::new(path)?;
    for (key, value) in &self.entries {  // BTreeMap iterates in sorted order!
        writer.write(key, value)?;
    }
    writer.finish()?;
    self.entries.clear();
    Ok(())
}
```

**`size_bytes` tracking:** We track the total size of all keys and values so that callers can decide when the MemTable is "full" and should be flushed. In a real system, you'd flush at something like 4MB or 64MB.

---

### 2. SSTable Writer (`sstable.rs` — `SSTableWriter`)

The writer takes sorted key-value pairs and produces the file format described above.

```rust
pub struct SSTableWriter {
    writer: BufWriter<File>,
    index: Vec<(Vec<u8>, u64)>,  // (key, byte_offset) for each entry
    offset: u64,                  // current position in the file
}
```

**Writing one entry:**

```rust
pub fn write(&mut self, key: &[u8], value: &[u8]) -> io::Result<()> {
    let entry_offset = self.offset;  // remember where this entry starts

    // Write the entry: key_len | value_len | key | value
    let key_len = key.len() as u32;
    let value_len = value.len() as u32;
    self.writer.write_all(&key_len.to_le_bytes())?;
    self.writer.write_all(&value_len.to_le_bytes())?;
    self.writer.write_all(key)?;
    self.writer.write_all(value)?;

    // Advance our offset tracker
    self.offset += 4 + 4 + key.len() as u64 + value.len() as u64;

    // Remember this key's offset for the index
    self.index.push((key.to_vec(), entry_offset));
    Ok(())
}
```

The crucial detail: we record each key's **byte offset** in `self.index`. This is what makes lookups fast later — instead of scanning the whole file, we can jump directly to the right position.

**Finishing the file:**

```rust
pub fn finish(mut self) -> io::Result<()> {
    let index_offset = self.offset;   // index starts here
    let entry_count = self.index.len() as u32;

    // Write the index: key_len | offset | key (for each entry)
    for (key, offset) in &self.index {
        self.writer.write_all(&(key.len() as u32).to_le_bytes())?;
        self.writer.write_all(&offset.to_le_bytes())?;
        self.writer.write_all(key)?;
    }

    // Write the footer: where the index starts + how many entries
    self.writer.write_all(&index_offset.to_le_bytes())?;
    self.writer.write_all(&entry_count.to_le_bytes())?;
    self.writer.flush()?;
    Ok(())
}
```

Note that `finish` takes `self` by value (not `&mut self`). This **consumes** the writer — you can't accidentally write more entries after finishing. This is Rust's ownership system enforcing correctness at compile time.

---

### 3. SSTable Reader (`sstable.rs` — `SSTableReader`)

The reader opens a file, loads the index into memory, and then serves lookups.

```rust
pub struct SSTableReader {
    path: String,
    index: Vec<IndexEntry>,  // sorted list of (key, offset) — loaded once on open
}

struct IndexEntry {
    key: Vec<u8>,
    offset: u64,
}
```

**Opening a file (reading backwards):**

```rust
pub fn open(path: &str) -> io::Result<Self> {
    let mut file = BufReader::new(File::open(path)?);

    // Step 1: Read the footer — last 12 bytes of the file
    file.seek(SeekFrom::End(-12))?;
    let index_offset = /* read 8 bytes as u64 */;
    let entry_count  = /* read 4 bytes as u32 */;

    // Step 2: Seek to where the index lives, read all entries
    file.seek(SeekFrom::Start(index_offset))?;
    for _ in 0..entry_count {
        // read key_len, offset, key → push to index vec
    }

    Ok(SSTableReader { path, index })
}
```

The pattern is: **read footer → find index → load index**. After this, the entire index is in memory and we never need to scan the file linearly.

**Looking up a key:**

```rust
pub fn get(&self, key: &[u8]) -> io::Result<Option<Vec<u8>>> {
    // Binary search the sorted index
    let result = self.index.binary_search_by(|entry| {
        entry.key.as_slice().cmp(key)
    });

    match result {
        Ok(pos) => {
            // Found! Seek to the offset and read the value.
            let offset = self.index[pos].offset;
            self.read_value_at(offset)
        }
        Err(_) => Ok(None),  // not found
    }
}
```

`binary_search_by` is from Rust's standard library. On a sorted slice, it finds the matching element in **O(log n)** comparisons. For an SSTable with 1 million entries, that's about 20 comparisons — then one disk seek to read the value.

**Range scan:**

```rust
pub fn scan(&self, start_key: &[u8], end_key: &[u8]) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    // Binary search to find where to start
    let start_pos = match self.index.binary_search_by(...) {
        Ok(pos) => pos,   // exact match
        Err(pos) => pos,  // insertion point (first key > start_key)
    };

    // Walk forward through the index until we pass end_key
    for entry in &self.index[start_pos..] {
        if entry.key > end_key { break; }
        // seek to entry.offset, read key+value, push to results
    }
}
```

Because the index is sorted, range scans are efficient — find the start with binary search, then walk forward sequentially.

---

## Rust Concepts Used

### Ownership & Borrowing
- `SSTableWriter::finish(self)` — takes ownership, preventing further use after finalization.
- `MemTable::get(&self, key: &[u8]) -> Option<&[u8]>` — borrows the value, avoiding a copy.
- `flush_to_sstable(&mut self)` — needs mutable access to clear the entries.

### `Vec<u8>` vs `&[u8]`
- **`Vec<u8>`** — owned, heap-allocated byte array. Used when we need to store data (in the index, in the BTreeMap).
- **`&[u8]`** — borrowed byte slice. Used in function parameters when we just need to read the data.
- **`b"hello"`** — a byte string literal, type `&[u8; 5]`, which coerces to `&[u8]`.

### `BufWriter` / `BufReader`
Without buffering, every `write_all` call would be a separate system call to the OS. `BufWriter` batches many small writes into larger chunks (default 8KB buffer). Same idea for `BufReader` on the read side. This is a big performance win for I/O-heavy code.

### Error Handling with `io::Result`
All I/O operations return `io::Result<T>`, which is `Result<T, io::Error>`. We use `?` to propagate errors up to the caller. No panics, no unwraps in library code.

### `to_le_bytes()` / `from_le_bytes()`
We use **little-endian** encoding for all integers on disk. This is explicit — `u32::to_le_bytes()` always produces the same bytes regardless of the CPU architecture. Never assume your machine's native byte order.

---

## What a Real SSTable Would Add

Our implementation is ~200 lines. Production SSTables add:

| Feature | Why |
|---|---|
| **Bloom filters** | Before searching the index, check a probabilistic filter. If it says "definitely not here", skip this SSTable entirely. Saves disk seeks. |
| **Block-based storage** | Instead of one entry per index slot, group entries into ~4KB blocks. The index points to blocks, not individual entries. Smaller index, better compression. |
| **Compression** | Compress each block with Snappy/LZ4/Zstd. Keys often share prefixes, so prefix compression is common too. |
| **Checksums** | CRC32 per block to detect data corruption on disk. |
| **Compaction** | Background process that merges multiple SSTables into one, removing deleted/overwritten entries. Without this, you'd accumulate files forever. |
| **Tombstones** | Special "deleted" markers instead of actually removing entries. Needed because SSTables are immutable — you can't remove from an existing file. |
| **Bloom filter per SSTable** | Quick "is this key possibly in this file?" check before doing any I/O. |
| **WAL (Write-Ahead Log)** | Before writing to the MemTable, log the write to a file. If the process crashes, replay the log to recover the MemTable. |

---

## Try It Yourself

```bash
cd sstable
cargo test
```

Read the tests in `lib.rs` — they demonstrate every operation:
- Writing and reading individual keys
- Flushing a MemTable to an SSTable
- Overwriting and deleting keys in the MemTable
- Range scans
