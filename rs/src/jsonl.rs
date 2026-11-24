//! Bounded reads of growing history logs, shared by each wallet's report indexes.

use std::collections::BTreeMap;
use std::fs::{File, Metadata};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

const BLOCK: usize = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    #[cfg(unix)]
    file_id: (u64, u64),
}

impl Stamp {
    fn new(m: Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: m.len(),
            modified: m.modified().ok(),
            created: m.created().ok(),
            #[cfg(unix)]
            file_id: (m.dev(), m.ino()),
        }
    }

    fn replaces(self, old: Self) -> bool {
        #[cfg(unix)]
        if self.file_id != old.file_id {
            return true;
        }
        self.created != old.created || self.len <= old.len
    }
}

#[derive(Default)]
struct Scan<T> {
    stamp: Option<Stamp>,
    offset: u64,
    data: T,
}

impl<T: Default> Scan<T> {
    fn refresh(&mut self, path: &Path, first_read_limit: u64, mut visit: impl FnMut(&mut T, &[u8])) {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                *self = Self::default();
                return;
            }
            Err(_) => return,
        };
        let Ok(meta) = file.metadata() else { return };
        let stamp = Stamp::new(meta);
        if self.stamp == Some(stamp) {
            return;
        }
        if self.stamp.is_some_and(|old| stamp.replaces(old)) {
            *self = Self::default();
        }
        let start = if self.offset == 0 { stamp.len.saturating_sub(first_read_limit) } else { self.offset };
        if file.seek(SeekFrom::Start(start)).is_err() {
            return;
        }
        self.offset = start;
        // Stop at the length observed on open. An unfinished append is retried when it grows.
        let mut input = BufReader::with_capacity(BLOCK, file.take(stamp.len - start));
        let mut line = Vec::new();
        loop {
            line.clear();
            let Ok(n) = input.read_until(b'\n', &mut line) else { return };
            if n == 0 || line.last() != Some(&b'\n') {
                break;
            }
            self.offset += n as u64;
            visit(&mut self.data, &line);
        }
        self.stamp = Some(stamp);
    }
}

/// One incremental index per file. Concurrent reports for the same wallet share the scan.
pub struct Index<T>(Mutex<BTreeMap<PathBuf, Arc<Mutex<Scan<T>>>>>);

impl<T: Default> Index<T> {
    pub const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    pub fn read<R>(&self, path: &Path, first_read_limit: u64, visit: impl FnMut(&mut T, &[u8]), get: impl FnOnce(&mut T) -> R) -> R {
        let file = {
            let mut files = self.0.lock().unwrap();
            Arc::clone(files.entry(path.to_path_buf()).or_default())
        };
        let mut scan = file.lock().unwrap();
        scan.refresh(path, first_read_limit, visit);
        get(&mut scan.data)
    }
}

/// The newest parseable record, reading backwards in blocks. A line can cross any number of blocks.
pub fn last<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let mut file = File::open(path).ok()?;
    let mut end = file.metadata().ok()?.len();
    let mut block = vec![0; BLOCK];
    let mut suffix = Vec::new();
    while end > 0 {
        let start = end.saturating_sub(BLOCK as u64);
        let n = (end - start) as usize;
        file.seek(SeekFrom::Start(start)).ok()?;
        file.read_exact(&mut block[..n]).ok()?;
        let mut rest = &block[..n];
        while let Some(i) = rest.iter().rposition(|b| *b == b'\n') {
            let line = &rest[i + 1..];
            if suffix.is_empty() {
                if let Ok(row) = serde_json::from_slice(line) {
                    return Some(row);
                }
            } else {
                let mut full = line.to_vec();
                full.append(&mut suffix);
                if let Ok(row) = serde_json::from_slice(&full) {
                    return Some(row);
                }
            }
            rest = &rest[..i];
        }
        if !rest.is_empty() {
            let mut prefix = rest.to_vec();
            prefix.append(&mut suffix);
            suffix = prefix;
        }
        end = start;
    }
    serde_json::from_slice(&suffix).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn an_index_reads_an_append_once_and_retries_a_partial_record() {
        let dir = crate::wallets::tests::temp_home("jsonl-append");
        let path = dir.join("history.jsonl");
        let index = Index::<Vec<u64>>::new();
        let read = || index.read(&path, u64::MAX, |rows, line| {
            if let Ok(n) = serde_json::from_slice(line) {
                rows.push(n);
            }
        }, |rows| rows.clone());
        std::fs::write(&path, b"1\n2").unwrap();
        assert_eq!(read(), [1]);
        assert_eq!(read(), [1]);
        let mut file = File::options().append(true).open(&path).unwrap();
        file.write_all(b"3\n4\n").unwrap();
        assert_eq!(read(), [1, 23, 4]);
        drop(file);
        std::fs::write(&path, b"7\n").unwrap();
        assert_eq!(read(), [7]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_tail_reads_across_blocks_and_past_a_partial_append() {
        let dir = crate::wallets::tests::temp_home("jsonl-tail");
        let path = dir.join("history.jsonl");
        let row = "x".repeat(BLOCK * 2);
        std::fs::write(&path, format!("null\r\n\"{row}\"\r\n{{\"unfinished\":")).unwrap();
        assert_eq!(last::<String>(&path), Some(row));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
