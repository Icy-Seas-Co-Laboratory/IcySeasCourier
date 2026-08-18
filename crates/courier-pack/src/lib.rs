//! Deterministic, streaming transport packs for collections of small logical files.

use std::{
    fs::File,
    io::{self, BufReader, Read, Write},
    path::{Component, Path},
};

use courier_core::{FileRecord, HashAlgorithm, verify_source_unchanged};
use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 8] = b"ISCPACK1";

#[derive(Debug, Clone, Copy)]
pub struct PackOptions {
    pub maximum_member_size: u64,
    pub target_pack_size: u64,
    pub zstd_level: i32,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            maximum_member_size: 8 * 1024 * 1024,
            target_pack_size: 128 * 1024 * 1024,
            zstd_level: 3,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PackPlan<'a> {
    pub packs: Vec<Vec<&'a FileRecord>>,
    pub standalone: Vec<&'a FileRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackMemberHeader {
    pub path: String,
    pub size: u64,
    pub digest_algorithm: HashAlgorithm,
    pub digest: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error(transparent)]
    Courier(#[from] courier_core::CourierError),
    #[error("pack I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("pack metadata serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("pack options require positive member and target sizes")]
    InvalidOptions,
    #[error("file path is not safely relative: {0}")]
    UnsafePath(String),
    #[error("source size changed while packing: {0}")]
    SizeChanged(String),
}

pub fn plan_packs<'a>(
    files: &'a [FileRecord],
    options: PackOptions,
) -> Result<PackPlan<'a>, PackError> {
    if options.maximum_member_size == 0 || options.target_pack_size == 0 {
        return Err(PackError::InvalidOptions);
    }
    let mut candidates: Vec<_> = files
        .iter()
        .filter(|file| file.size <= options.maximum_member_size)
        .collect();
    let mut standalone: Vec<_> = files
        .iter()
        .filter(|file| file.size > options.maximum_member_size)
        .collect();
    candidates.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    standalone.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    let mut packs = Vec::new();
    let mut current = Vec::new();
    let mut current_size = 0_u64;
    for file in candidates {
        if !current.is_empty() && current_size.saturating_add(file.size) > options.target_pack_size
        {
            packs.push(std::mem::take(&mut current));
            current_size = 0;
        }
        current_size = current_size.saturating_add(file.size);
        current.push(file);
    }
    if !current.is_empty() {
        packs.push(current);
    }
    Ok(PackPlan { packs, standalone })
}

/// Writes one independently retryable Zstandard frame without materializing an
/// uncompressed archive. Members are revalidated immediately before reading.
pub fn encode_pack(
    members: &[&FileRecord],
    destination: impl Write,
    zstd_level: i32,
) -> Result<u64, PackError> {
    let mut encoder = zstd::stream::write::Encoder::new(destination, zstd_level)?;
    encoder.include_checksum(true)?;
    encoder.write_all(MAGIC)?;
    let mut written = MAGIC.len() as u64;
    for member in members {
        verify_source_unchanged(member)?;
        let header = PackMemberHeader {
            path: portable_path(&member.relative_path)?,
            size: member.size,
            digest_algorithm: member.hash_algorithm,
            digest: member.sha256.clone(),
        };
        let encoded = serde_json::to_vec(&header)?;
        let header_length = u32::try_from(encoded.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "pack header too large"))?;
        encoder.write_all(&header_length.to_le_bytes())?;
        encoder.write_all(&encoded)?;
        encoder.write_all(&member.size.to_le_bytes())?;
        written = written
            .saturating_add(4)
            .saturating_add(encoded.len() as u64)
            .saturating_add(8)
            .saturating_add(member.size);

        let mut source = BufReader::new(File::open(&member.absolute_path)?);
        let copied = io::copy(&mut source.by_ref().take(member.size), &mut encoder)?;
        if copied != member.size {
            return Err(PackError::SizeChanged(header.path));
        }
    }
    encoder.write_all(&0_u32.to_le_bytes())?;
    written = written.saturating_add(4);
    encoder.finish()?;
    Ok(written)
}

pub fn decode_pack(
    source: impl Read,
    mut member: impl FnMut(PackMemberHeader, &mut dyn Read) -> Result<(), PackError>,
) -> Result<(), PackError> {
    let mut decoder = zstd::stream::read::Decoder::new(source)?;
    let mut magic = [0_u8; 8];
    decoder.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(
            io::Error::new(io::ErrorKind::InvalidData, "invalid Courier pack magic").into(),
        );
    }
    loop {
        let mut length = [0_u8; 4];
        decoder.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length == 0 {
            return Ok(());
        }
        let mut encoded = vec![0; length];
        decoder.read_exact(&mut encoded)?;
        let header: PackMemberHeader = serde_json::from_slice(&encoded)?;
        let mut content_length = [0_u8; 8];
        decoder.read_exact(&mut content_length)?;
        let content_length = u64::from_le_bytes(content_length);
        if content_length != header.size {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "pack member size mismatch").into(),
            );
        }
        let mut content = decoder.by_ref().take(content_length);
        member(header, &mut content)?;
        if io::copy(&mut content, &mut io::sink())? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pack member was not fully consumed",
            )
            .into());
        }
    }
}

fn portable_path(path: &Path) -> Result<String, PackError> {
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => Ok(value.to_string_lossy().into_owned()),
            _ => Err(PackError::UnsafePath(path.display().to_string())),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if components.is_empty() {
        return Err(PackError::UnsafePath(path.display().to_string()));
    }
    Ok(components.join("/"))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use courier_core::{FileStatus, InventoryOptions, inventory_transfer};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn plans_stable_bounded_packs_and_leaves_large_files_standalone() {
        let transfer_id = Uuid::new_v4();
        let files = [
            fixture(transfer_id, "c", 3),
            fixture(transfer_id, "a", 3),
            fixture(transfer_id, "large", 11),
            fixture(transfer_id, "b", 3),
        ];
        let plan = plan_packs(
            &files,
            PackOptions {
                maximum_member_size: 10,
                target_pack_size: 6,
                zstd_level: 3,
            },
        )
        .unwrap();
        assert_eq!(plan.packs.len(), 2);
        assert_eq!(plan.packs[0][0].relative_path, PathBuf::from("a"));
        assert_eq!(plan.packs[0][1].relative_path, PathBuf::from("b"));
        assert_eq!(plan.packs[1][0].relative_path, PathBuf::from("c"));
        assert_eq!(plan.standalone[0].relative_path, PathBuf::from("large"));
    }

    #[test]
    fn round_trips_unicode_empty_and_nested_members() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("nested/温度.csv"), b"1,2,3\n").unwrap();
        fs::write(directory.path().join("empty.txt"), b"").unwrap();
        let files = inventory_transfer(
            Uuid::new_v4(),
            directory.path(),
            &InventoryOptions::default(),
        )
        .unwrap();
        let members = files.iter().collect::<Vec<_>>();
        let mut encoded = Vec::new();
        encode_pack(&members, &mut encoded, 3).unwrap();

        let mut decoded = Vec::new();
        decode_pack(encoded.as_slice(), |header, reader| {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes)?;
            decoded.push((header, bytes));
            Ok(())
        })
        .unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].0.path, "empty.txt");
        assert_eq!(decoded[1].0.path, "nested/温度.csv");
        assert_eq!(decoded[1].1, b"1,2,3\n");
    }

    fn fixture(transfer_id: Uuid, path: &str, size: u64) -> FileRecord {
        FileRecord {
            id: Uuid::new_v4(),
            transfer_id,
            relative_path: path.into(),
            absolute_path: PathBuf::from(path),
            size,
            mtime_ns: 0,
            hash_algorithm: HashAlgorithm::Sha256,
            sha256: "0".repeat(64),
            status: FileStatus::Ready,
            bytes_completed: 0,
        }
    }
}
