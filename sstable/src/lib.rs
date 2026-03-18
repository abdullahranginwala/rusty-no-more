mod memtable;
mod sstable;

pub use memtable::MemTable;
pub use sstable::{SSTableReader, SSTableWriter};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn cleanup(path: &str) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn write_and_read_sstable() {
        let path = "/tmp/test_sstable_basic.db";
        cleanup(path);

        // Write some sorted key-value pairs
        let mut writer = SSTableWriter::new(path).unwrap();
        writer.write(b"apple", b"red").unwrap();
        writer.write(b"banana", b"yellow").unwrap();
        writer.write(b"cherry", b"dark red").unwrap();
        writer.write(b"grape", b"purple").unwrap();
        writer.write(b"mango", b"orange").unwrap();
        writer.finish().unwrap();

        // Read them back
        let reader = SSTableReader::open(path).unwrap();
        assert_eq!(reader.get(b"apple").unwrap(), Some(b"red".to_vec()));
        assert_eq!(reader.get(b"banana").unwrap(), Some(b"yellow".to_vec()));
        assert_eq!(reader.get(b"cherry").unwrap(), Some(b"dark red".to_vec()));
        assert_eq!(reader.get(b"grape").unwrap(), Some(b"purple".to_vec()));
        assert_eq!(reader.get(b"mango").unwrap(), Some(b"orange".to_vec()));
        assert_eq!(reader.get(b"watermelon").unwrap(), None);

        cleanup(path);
    }

    #[test]
    fn memtable_flush_to_sstable() {
        let path = "/tmp/test_sstable_memtable.db";
        cleanup(path);

        // Insert keys in any order — memtable sorts them
        let mut mem = MemTable::new();
        mem.put(b"zebra", b"striped");
        mem.put(b"ant", b"tiny");
        mem.put(b"dog", b"loyal");

        assert_eq!(mem.get(b"dog"), Some(b"loyal".as_slice()));
        assert_eq!(mem.get(b"cat"), None);

        // Flush memtable to disk as an SSTable
        mem.flush_to_sstable(path).unwrap();

        // Memtable is empty after flush
        assert_eq!(mem.get(b"dog"), None);

        // SSTable has everything, sorted
        let reader = SSTableReader::open(path).unwrap();
        assert_eq!(reader.get(b"ant").unwrap(), Some(b"tiny".to_vec()));
        assert_eq!(reader.get(b"dog").unwrap(), Some(b"loyal".to_vec()));
        assert_eq!(reader.get(b"zebra").unwrap(), Some(b"striped".to_vec()));

        cleanup(path);
    }

    #[test]
    fn memtable_delete_and_overwrite() {
        let mut mem = MemTable::new();
        mem.put(b"key1", b"value1");
        mem.put(b"key1", b"value2"); // overwrite
        assert_eq!(mem.get(b"key1"), Some(b"value2".as_slice()));

        mem.delete(b"key1");
        assert_eq!(mem.get(b"key1"), None);
    }

    #[test]
    fn scan_range() {
        let path = "/tmp/test_sstable_scan.db";
        cleanup(path);

        let mut writer = SSTableWriter::new(path).unwrap();
        writer.write(b"a", b"1").unwrap();
        writer.write(b"b", b"2").unwrap();
        writer.write(b"c", b"3").unwrap();
        writer.write(b"d", b"4").unwrap();
        writer.write(b"e", b"5").unwrap();
        writer.finish().unwrap();

        let reader = SSTableReader::open(path).unwrap();
        let results = reader.scan(b"b", b"d").unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[0], (b"b".to_vec(), b"2".to_vec()));
        assert_eq!(results[1], (b"c".to_vec(), b"3".to_vec()));
        assert_eq!(results[2], (b"d".to_vec(), b"4".to_vec()));

        cleanup(path);
    }
}
