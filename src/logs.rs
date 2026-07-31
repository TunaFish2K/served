use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use chrono::Local;

use crate::protocol::HistoryRecord;

pub const MEMORY_LOG_LIMIT: usize = 64 * 1024;
pub const MAX_ARCHIVED_LOGS: usize = 100;
pub const DEFAULT_CHUNK_LIMIT: u32 = 48 * 1024;
pub const ATTACH_CACHE_LINES: usize = 48;

const ATTACH_CACHE_MAX_BYTES: usize = 16 * 1024;

const LATEST_FILE: &str = "latest.log";
const STARTED_FILE: &str = ".latest.started";

#[derive(Debug)]
struct MemoryLog {
    id: String,
    bytes: VecDeque<u8>,
}

#[derive(Debug)]
pub struct LogStore {
    directory: PathBuf,
    current: VecDeque<u8>,
    current_started: Option<String>,
    current_persisted: bool,
    disk_file: Option<File>,
    disk_bytes: u64,
    memory_archives: VecDeque<MemoryLog>,
    line_counts: HashMap<String, u64>,
}

#[derive(Debug, Clone)]
pub struct LogChunk {
    pub next_offset: u64,
    pub total: u64,
    pub total_lines: u64,
    pub eof: bool,
    pub content: String,
}

impl LogStore {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            current: VecDeque::with_capacity(MEMORY_LOG_LIMIT.min(8192)),
            current_started: None,
            current_persisted: false,
            disk_file: None,
            disk_bytes: 0,
            memory_archives: VecDeque::new(),
            line_counts: HashMap::new(),
        }
    }

    pub fn begin_run(&mut self, persist: bool) -> Vec<String> {
        let mut warnings = Vec::new();
        self.archive_memory_current();
        self.close_disk_file();
        self.current.clear();
        self.disk_bytes = 0;
        self.current_persisted = false;
        self.line_counts.remove("latest");
        self.line_counts.remove(LATEST_FILE);
        let started = timestamp_label();
        self.current_started = Some(started.clone());

        if let Err(error) = self.rotate_latest(&started) {
            warnings.push(format!("rotate existing latest log: {error}"));
        }

        if persist {
            match self.open_persistent_run(&started) {
                Ok(file) => {
                    self.disk_file = Some(file);
                    self.current_persisted = true;
                }
                Err(error) => warnings.push(format!("open persistent log: {error}")),
            }
        }

        if let Err(error) = self.prune_disk_archives() {
            warnings.push(format!("prune archived logs: {error}"));
        }
        warnings
    }

    pub fn append(&mut self, bytes: &[u8]) -> Option<String> {
        self.line_counts.remove("latest");
        self.line_counts.remove(LATEST_FILE);
        for byte in bytes {
            self.current.push_back(*byte);
        }
        while self.current.len() > MEMORY_LOG_LIMIT {
            self.current.pop_front();
        }

        let file = self.disk_file.as_mut()?;
        if let Err(error) = file.write_all(bytes) {
            self.close_disk_file();
            self.current_persisted = false;
            return Some(format!("persistent log write failed: {error}"));
        }
        self.disk_bytes = self.disk_bytes.saturating_add(bytes.len() as u64);
        None
    }

    pub fn output_tail(&self) -> String {
        let raw: Vec<u8> = self.current.iter().copied().collect();
        let cleaned = sanitize_log(&raw);
        let max_chars = 2000;
        let length = cleaned.chars().count();
        if length > max_chars {
            cleaned.chars().skip(length - max_chars).collect()
        } else {
            cleaned
        }
    }

    pub fn attach_snapshot(&self) -> Vec<u8> {
        let raw: Vec<u8> = self.current.iter().copied().collect();
        attach_snapshot_from_bytes(&raw)
    }

    pub fn latest_log_path(&self) -> Option<PathBuf> {
        (self.current_started.is_some() && self.current_persisted)
            .then(|| self.directory.join(LATEST_FILE))
    }

    pub fn records(&self) -> Vec<HistoryRecord> {
        let mut records = Vec::new();
        if self.current_started.is_some() {
            records.push(HistoryRecord {
                id: "latest".to_owned(),
                bytes: self.current_size(),
                current: true,
                persisted: self.current_persisted,
            });
        }

        for archive in &self.memory_archives {
            records.push(HistoryRecord {
                id: archive.id.clone(),
                bytes: archive.bytes.len() as u64,
                current: false,
                persisted: false,
            });
        }

        if let Ok(entries) = fs::read_dir(&self.directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(_) => continue,
                };
                if !file_type.is_file()
                    || path.extension().and_then(|value| value.to_str()) != Some("log")
                {
                    continue;
                }
                let Some(id) = path.file_name().and_then(|value| value.to_str()) else {
                    continue;
                };
                if id == LATEST_FILE || !valid_archive_id(id) {
                    continue;
                }
                let bytes = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
                records.push(HistoryRecord {
                    id: id.to_owned(),
                    bytes,
                    current: false,
                    persisted: true,
                });
            }
        }

        records.sort_by(|left, right| match (left.current, right.current) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => right.id.cmp(&left.id),
        });
        records
    }

    pub fn read_chunk(
        &mut self,
        id: &str,
        offset: u64,
        requested_limit: u32,
    ) -> io::Result<LogChunk> {
        let limit = requested_limit.clamp(1, DEFAULT_CHUNK_LIMIT) as usize;
        if id == "latest" {
            if self.current_started.is_some() && !self.current_persisted {
                let raw: Vec<u8> = self.current.iter().copied().collect();
                let total_lines = self.cached_line_count("latest", &raw);
                return memory_chunk(&raw, offset, limit, total_lines);
            }
            return self.read_disk_chunk(LATEST_FILE, offset, limit);
        }

        if self.memory_archives.iter().any(|archive| archive.id == id) {
            let raw: Vec<u8> = self
                .memory_archives
                .iter()
                .find(|archive| archive.id == id)
                .map(|archive| archive.bytes.iter().copied().collect())
                .unwrap_or_default();
            let total_lines = self.cached_line_count(id, &raw);
            return memory_chunk(&raw, offset, limit, total_lines);
        }
        if !valid_archive_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid history record id",
            ));
        }
        self.read_disk_chunk(id, offset, limit)
    }

    fn current_size(&self) -> u64 {
        if self.current_persisted {
            self.disk_bytes
        } else {
            self.current.len() as u64
        }
    }

    fn archive_memory_current(&mut self) {
        let Some(started) = self.current_started.take() else {
            return;
        };
        if self.current_persisted {
            return;
        }
        let id = self.unique_archive_id(&started);
        self.memory_archives.push_front(MemoryLog {
            id,
            bytes: std::mem::take(&mut self.current),
        });
        self.memory_archives.truncate(MAX_ARCHIVED_LOGS);
    }

    fn rotate_latest(&self, fallback_started: &str) -> io::Result<()> {
        let latest = self.directory.join(LATEST_FILE);
        let sidecar = self.directory.join(STARTED_FILE);
        let metadata = match fs::symlink_metadata(&latest) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                remove_if_exists(&sidecar)?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not a regular file", latest.display()),
            ));
        }

        let started = fs::read_to_string(&sidecar)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| valid_archive_stem(value))
            .unwrap_or_else(|| fallback_started.to_owned());
        let archive = self.directory.join(self.unique_archive_id(&started));
        fs::rename(&latest, archive)?;
        remove_if_exists(&sidecar)?;
        Ok(())
    }

    fn open_persistent_run(&self, started: &str) -> io::Result<File> {
        fs::create_dir_all(&self.directory)?;
        fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))?;
        let latest = self.directory.join(LATEST_FILE);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let file = options.open(&latest)?;
        let sidecar = self.directory.join(STARTED_FILE);
        let mut sidecar_options = OpenOptions::new();
        sidecar_options.write(true).create_new(true).mode(0o600);
        let mut sidecar_file = sidecar_options.open(sidecar)?;
        sidecar_file.write_all(started.as_bytes())?;
        sidecar_file.write_all(b"\n")?;
        Ok(file)
    }

    fn prune_disk_archives(&self) -> io::Result<()> {
        let mut archives = fs::read_dir(&self.directory)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| {
                        let path = entry.path();
                        let name = path.file_name()?.to_str()?.to_owned();
                        let file_type = entry.file_type().ok()?;
                        if file_type.is_file() && valid_archive_id(&name) {
                            Some(name)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        archives.sort();
        while archives.len() > MAX_ARCHIVED_LOGS {
            let oldest = archives.remove(0);
            fs::remove_file(self.directory.join(oldest))?;
        }
        Ok(())
    }

    fn read_disk_chunk(&mut self, id: &str, offset: u64, limit: usize) -> io::Result<LogChunk> {
        if id != LATEST_FILE && !valid_archive_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid history record id",
            ));
        }
        let path = self.directory.join(id);
        let total_lines = self.disk_line_count(id)?;
        let mut file = File::open(&path)?;
        let total = file.metadata()?.len();
        if offset >= total {
            return Ok(LogChunk {
                next_offset: total,
                total,
                total_lines,
                eof: true,
                content: String::new(),
            });
        }
        file.seek(SeekFrom::Start(offset))?;
        let read_length = (total - offset).min(limit as u64) as usize;
        let mut raw = vec![0_u8; read_length];
        let length = file.read(&mut raw)?;
        raw.truncate(length);
        let next_offset = offset.saturating_add(length as u64);
        Ok(LogChunk {
            next_offset,
            total,
            total_lines,
            eof: next_offset >= total,
            content: sanitize_log(&raw),
        })
    }

    fn disk_line_count(&mut self, id: &str) -> io::Result<u64> {
        if let Some(count) = self.line_counts.get(id) {
            return Ok(*count);
        }
        let raw = fs::read(self.directory.join(id))?;
        let count = logical_line_count(&raw);
        self.line_counts.insert(id.to_owned(), count);
        Ok(count)
    }

    fn cached_line_count(&mut self, id: &str, raw: &[u8]) -> u64 {
        if let Some(count) = self.line_counts.get(id) {
            return *count;
        }
        let count = logical_line_count(raw);
        self.line_counts.insert(id.to_owned(), count);
        count
    }

    fn unique_archive_id(&self, started: &str) -> String {
        let base = if valid_archive_stem(started) {
            started.to_owned()
        } else {
            timestamp_label()
        };
        let candidate = format!("{base}.log");
        if !self.archive_id_taken(&candidate) {
            return candidate;
        }
        for index in 1.. {
            let candidate = format!("{base}-{index}.log");
            if !self.archive_id_taken(&candidate) {
                return candidate;
            }
        }
        unreachable!("archive id search must find a free name")
    }

    fn archive_id_taken(&self, id: &str) -> bool {
        self.directory.join(id).exists()
            || self.memory_archives.iter().any(|archive| archive.id == id)
    }

    fn close_disk_file(&mut self) {
        self.disk_file = None;
    }
}

fn memory_chunk(bytes: &[u8], offset: u64, limit: usize, total_lines: u64) -> io::Result<LogChunk> {
    let total = bytes.len() as u64;
    if offset >= total {
        return Ok(LogChunk {
            next_offset: total,
            total,
            total_lines,
            eof: true,
            content: String::new(),
        });
    }
    let raw: Vec<u8> = bytes
        .iter()
        .skip(offset as usize)
        .take(limit)
        .copied()
        .collect();
    let next_offset = offset.saturating_add(raw.len() as u64);
    Ok(LogChunk {
        next_offset,
        total,
        total_lines,
        eof: next_offset >= total,
        content: sanitize_log(&raw),
    })
}

fn logical_line_count(bytes: &[u8]) -> u64 {
    sanitize_log(bytes).lines().count() as u64
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn timestamp_label() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

fn valid_archive_stem(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_digit() || character == '-')
}

fn valid_archive_id(value: &str) -> bool {
    value.ends_with(".log") && valid_archive_stem(value.trim_end_matches(".log"))
}

pub fn sanitize_log(bytes: &[u8]) -> String {
    let mut clean = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == 0x1b {
            index += 1;
            if index < bytes.len() && bytes[index] == b']' {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            } else if index < bytes.len() && bytes[index] == b'[' {
                index += 1;
                while index < bytes.len() {
                    let final_byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&final_byte) {
                        break;
                    }
                }
            } else if index < bytes.len() {
                index += 1;
            }
            continue;
        }
        if byte == 0x9b {
            index += 1;
            while index < bytes.len() {
                let final_byte = bytes[index];
                index += 1;
                if (0x40..=0x7e).contains(&final_byte) {
                    break;
                }
            }
            continue;
        }
        if byte == b'\n' || byte == b'\r' || byte == b'\t' || (byte >= 0x20 && byte != 0x7f) {
            clean.push(byte);
        }
        index += 1;
    }
    String::from_utf8_lossy(&clean).into_owned()
}

pub fn attach_snapshot_from_bytes(bytes: &[u8]) -> Vec<u8> {
    let cleaned = normalize_line_endings(&sanitize_log(bytes));
    let ended_with_newline = cleaned.ends_with('\n');
    let mut lines: Vec<&str> = cleaned.split('\n').collect();
    if ended_with_newline {
        lines.pop();
    }
    let start = lines.len().saturating_sub(ATTACH_CACHE_LINES);
    let mut snapshot = lines[start..].join("\r\n");
    snapshot = tail_chars(&snapshot, ATTACH_CACHE_MAX_BYTES);
    if !snapshot.is_empty() {
        snapshot.push_str("\r\n");
    }
    snapshot.into_bytes()
}

fn normalize_line_endings(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn tail_chars(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut used = 0_usize;
    let mut start = value.len();
    for (index, character) in value.char_indices().rev() {
        let length = character.len_utf8();
        if used.saturating_add(length) > max_bytes {
            break;
        }
        used += length;
        start = index;
    }
    value[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sanitizer_removes_ansi_sequences_but_keeps_text() {
        assert_eq!(sanitize_log(b"\x1b[31mred\x1b[0m\n"), "red\n");
        assert_eq!(sanitize_log(b"a\0b\tc\n"), "ab\tc\n");
    }

    #[test]
    fn attach_snapshot_joins_multiple_output_chunks_and_keeps_recent_lines() {
        let directory = tempdir().expect("tempdir");
        let mut store = LogStore::new(directory.path().to_path_buf());
        store.begin_run(false);
        store.append(b"old-1\nold-2\n");
        store.append(b"\x1b[31mnew-1\x1b[0m\r\nnew-2");

        assert_eq!(
            String::from_utf8(store.attach_snapshot()).expect("snapshot utf8"),
            "old-1\r\nold-2\r\nnew-1\r\nnew-2\r\n"
        );
    }

    #[test]
    fn attach_snapshot_limits_logical_lines_and_bytes() {
        let directory = tempdir().expect("tempdir");
        let mut store = LogStore::new(directory.path().to_path_buf());
        store.begin_run(false);
        for index in 0..(ATTACH_CACHE_LINES + 10) {
            store.append(format!("line-{index}\n").as_bytes());
        }

        let snapshot = String::from_utf8(store.attach_snapshot()).expect("snapshot utf8");
        assert!(!snapshot.contains("line-0\r\n"));
        assert!(snapshot.contains("line-10\r\n"));
        assert!(snapshot.contains("line-57\r\n"));
        assert!(snapshot.len() <= ATTACH_CACHE_MAX_BYTES + 2);
    }

    #[test]
    fn latest_log_path_exists_only_for_a_persisted_current_run() {
        let directory = tempdir().expect("tempdir");
        let mut store = LogStore::new(directory.path().to_path_buf());

        store.begin_run(false);
        assert_eq!(store.latest_log_path(), None);

        assert!(store.begin_run(true).is_empty());
        assert_eq!(
            store.latest_log_path(),
            Some(directory.path().join(LATEST_FILE))
        );
    }

    #[test]
    fn history_chunks_report_logical_lines_and_invalidate_current_cache() {
        for persist in [false, true] {
            let directory = tempdir().expect("tempdir");
            let mut store = LogStore::new(directory.path().to_path_buf());
            store.begin_run(persist);

            assert_eq!(
                store
                    .read_chunk("latest", 0, 1024)
                    .expect("empty chunk")
                    .total_lines,
                0
            );

            store.append(b"one\n");
            assert_eq!(
                store
                    .read_chunk("latest", 0, 1024)
                    .expect("first chunk")
                    .total_lines,
                1
            );

            store.append(b"two\nthree");
            assert_eq!(
                store
                    .read_chunk("latest", 0, 1024)
                    .expect("second chunk")
                    .total_lines,
                3
            );

            store.append(b"\n");
            assert_eq!(
                store
                    .read_chunk("latest", 0, 1024)
                    .expect("terminated chunk")
                    .total_lines,
                3
            );
        }
    }

    #[test]
    fn persistent_runs_rotate_latest_and_keep_sidecar() {
        let directory = tempdir().expect("tempdir");
        let mut store = LogStore::new(directory.path().to_path_buf());
        assert!(store.begin_run(true).is_empty());
        store.append(b"first\n");
        fs::write(directory.path().join(STARTED_FILE), "20240101-010203\n").expect("sidecar");
        store.begin_run(true);
        assert_eq!(
            fs::read_to_string(directory.path().join("20240101-010203.log")).expect("archive"),
            "first\n"
        );
        assert!(directory.path().join(LATEST_FILE).exists());
        assert!(directory.path().join(STARTED_FILE).exists());
    }

    #[test]
    fn memory_runs_are_bounded_and_archived() {
        let directory = tempdir().expect("tempdir");
        let mut store = LogStore::new(directory.path().to_path_buf());
        store.begin_run(false);
        store.append(&vec![b'x'; MEMORY_LOG_LIMIT + 10]);
        store.begin_run(false);
        let records = store.records();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].bytes, MEMORY_LOG_LIMIT as u64);
        assert!(!directory.path().join(LATEST_FILE).exists());
    }

    #[test]
    fn a_new_non_persistent_manager_run_archives_stale_latest() {
        let directory = tempdir().expect("tempdir");
        let mut first = LogStore::new(directory.path().to_path_buf());
        first.begin_run(true);
        first.append(b"before-manager-restart\n");
        drop(first);

        let mut second = LogStore::new(directory.path().to_path_buf());
        second.begin_run(false);
        let archives: Vec<_> = fs::read_dir(directory.path())
            .expect("log directory")
            .flatten()
            .filter(|entry| valid_archive_id(&entry.file_name().to_string_lossy()))
            .collect();
        assert_eq!(archives.len(), 1);
        assert_eq!(
            fs::read_to_string(archives[0].path()).expect("stale archive"),
            "before-manager-restart\n"
        );
        assert!(!directory.path().join(LATEST_FILE).exists());
    }

    #[test]
    fn persistent_archive_retention_keeps_one_hundred_files() {
        let directory = tempdir().expect("tempdir");
        fs::create_dir_all(directory.path()).expect("log directory");
        for index in 0..=MAX_ARCHIVED_LOGS {
            fs::write(
                directory
                    .path()
                    .join(format!("20240101-0000{index:02}.log")),
                "archive\n",
            )
            .expect("archive");
        }
        let mut store = LogStore::new(directory.path().to_path_buf());
        store.begin_run(false);
        let count = fs::read_dir(directory.path())
            .expect("log directory")
            .flatten()
            .filter(|entry| valid_archive_id(&entry.file_name().to_string_lossy()))
            .count();
        assert_eq!(count, MAX_ARCHIVED_LOGS);
    }
}
