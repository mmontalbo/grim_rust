use std::fs::File;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use memmap2::{Mmap, MmapOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabTypeId(pub [u8; 4]);

impl LabTypeId {
    pub const NULL: LabTypeId = LabTypeId([0; 4]);

    pub fn as_str(self) -> Option<String> {
        let bytes = self.0;
        if bytes.iter().all(|&b| b == 0) {
            return None;
        }

        if bytes.iter().any(|&b| b == 0) {
            // Trim trailing zeros but ensure non-empty
            let mut len = 4;
            while len > 0 && bytes[len - 1] == 0 {
                len -= 1;
            }
            if len == 0 {
                return None;
            }
            return Some(String::from_utf8_lossy(&bytes[..len]).into_owned());
        }

        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[derive(Debug, Clone)]
pub struct LabEntry {
    pub name: String,
    pub offset: u64,
    pub size: u32,
    pub type_id: LabTypeId,
}

impl LabEntry {
    pub fn data_range(&self) -> Range<usize> {
        let start = self.offset as usize;
        let end = start + self.size as usize;
        start..end
    }
}

#[derive(Debug)]
pub struct LabArchive {
    path: PathBuf,
    mmap: Mmap,
    entries: Vec<LabEntry>,
}

impl LabArchive {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        let file = File::open(&path_buf)
            .with_context(|| format!("opening LAB archive at {}", path_buf.display()))?;
        let mmap = unsafe { MmapOptions::new().map(&file) }
            .with_context(|| format!("memory-mapping LAB archive {}", path_buf.display()))?;

        let entries = parse_entries(&mmap)
            .with_context(|| format!("parsing LAB archive {}", path_buf.display()))?;

        Ok(LabArchive {
            path: path_buf,
            mmap,
            entries,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &[LabEntry] {
        &self.entries
    }

    pub fn find_entry(&self, name: &str) -> Option<&LabEntry> {
        self.entries
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
    }

    pub fn read_entry_bytes(&self, entry: &LabEntry) -> &[u8] {
        let range = entry.data_range();
        &self.mmap[range]
    }

    pub fn extract_entry<P: AsRef<Path>>(&self, entry: &LabEntry, dest: P) -> Result<()> {
        let range = entry.data_range();
        let bytes = &self.mmap[range];
        let mut file = File::create(dest.as_ref())
            .with_context(|| format!("creating {}", dest.as_ref().display()))?;
        file.write_all(bytes)
            .with_context(|| format!("writing {}", dest.as_ref().display()))?;
        Ok(())
    }
}

fn parse_entries(mmap: &Mmap) -> Result<Vec<LabEntry>> {
    const HEADER_SIZE: usize = 16;
    const ENTRY_SIZE: usize = 16;

    ensure!(
        mmap.len() >= HEADER_SIZE,
        "LAB archive is too small to contain a header"
    );

    let header = &mmap[..HEADER_SIZE];
    if &header[0..4] != b"LABN" {
        bail!("LAB archive missing LABN signature");
    }

    let file_count = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    let name_list_len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;

    let entries_bytes_len = file_count
        .checked_mul(ENTRY_SIZE)
        .ok_or_else(|| anyhow!("LAB archive entry count overflow"))?;

    let names_offset = HEADER_SIZE + entries_bytes_len;
    let names_end = names_offset
        .checked_add(name_list_len)
        .ok_or_else(|| anyhow!("LAB archive name table overflow"))?;

    ensure!(
        names_end <= mmap.len(),
        "LAB archive truncated before name table"
    );

    let entries_block = &mmap[HEADER_SIZE..HEADER_SIZE + entries_bytes_len];
    let names_block = &mmap[names_offset..names_end];

    let mut entries = Vec::with_capacity(file_count);

    for index in 0..file_count {
        let base = index * ENTRY_SIZE;
        let entry_bytes = &entries_block[base..base + ENTRY_SIZE];

        let name_offset = u32::from_le_bytes(entry_bytes[0..4].try_into().unwrap()) as usize;
        let data_offset = u32::from_le_bytes(entry_bytes[4..8].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(entry_bytes[8..12].try_into().unwrap());
        let type_id = LabTypeId(entry_bytes[12..16].try_into().unwrap());

        ensure!(
            name_offset < name_list_len,
            "LAB entry {index} has invalid name offset {name_offset}"
        );

        let end = data_offset
            .checked_add(size as usize)
            .ok_or_else(|| anyhow!("LAB entry {index} size overflow"))?;
        ensure!(
            end <= mmap.len(),
            "LAB entry {index} data extends beyond file"
        );

        let name = read_c_string(names_block, name_offset)
            .with_context(|| format!("reading name for entry {index}"))?;

        entries.push(LabEntry {
            name,
            offset: data_offset as u64,
            size,
            type_id,
        });
    }

    Ok(entries)
}

fn read_c_string(table: &[u8], offset: usize) -> Result<String> {
    if offset >= table.len() {
        bail!("name offset beyond table length");
    }

    let mut end = offset;
    while end < table.len() && table[end] != 0 {
        end += 1;
    }

    ensure!(end > offset, "empty LAB entry name");

    let bytes = &table[offset..end];
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn parses_single_entry_archive() {
        let mut file = NamedTempFile::new().unwrap();
        let mut data = Vec::new();
        // header: LABN, unknown(0), count(1), names len(10)
        data.extend_from_slice(b"LABN");
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&10u32.to_le_bytes());
        // entry: name offset 0, data offset 42, size 4, type 'TEST'
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&42u32.to_le_bytes());
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(b"TEST");
        // names table "demo\0pad\0\0" (10 bytes)
        data.extend_from_slice(b"demo\0pad\0\0");
        // pad to data offset
        while data.len() < 42 {
            data.push(0);
        }
        data.extend_from_slice(b"ABCD");

        file.write_all(&data).unwrap();
        // Sanity check our manual buffer layout.
        assert_eq!(&data[42..46], b"ABCD");

        let archive = LabArchive::open(file.path()).unwrap();
        assert_eq!(archive.entries().len(), 1);
        let entry = &archive.entries()[0];
        assert_eq!(entry.name, "demo");
        assert_eq!(entry.offset, 42);
        assert_eq!(entry.size, 4);
        assert_eq!(entry.type_id.as_str().as_deref(), Some("TEST"));
        assert_eq!(archive.read_entry_bytes(entry), b"ABCD");
    }
}
