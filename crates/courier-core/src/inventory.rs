use std::{
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use sha2::{Digest, Sha256};
use uuid::Uuid;
use walkdir::WalkDir;
use xxhash_rust::xxh3::Xxh3;

use crate::{CourierError, FileRecord, FileStatus, HashAlgorithm, Result};

#[derive(Debug, Clone)]
pub struct InventoryOptions {
    pub buffer_size: usize,
    pub hash_algorithm: HashAlgorithm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryProgress {
    pub files_analyzed: u64,
    pub total_files: u64,
    pub bytes_analyzed: u64,
    pub total_bytes: u64,
    pub current_path: PathBuf,
}

impl Default for InventoryOptions {
    fn default() -> Self {
        Self {
            buffer_size: 8 * 1024 * 1024,
            hash_algorithm: HashAlgorithm::Sha256,
        }
    }
}

pub fn inventory_transfer(
    transfer_id: Uuid,
    source: &Path,
    options: &InventoryOptions,
) -> Result<Vec<FileRecord>> {
    inventory_transfer_observed(transfer_id, source, options, |_| {})
}

pub fn inventory_transfer_observed<F>(
    transfer_id: Uuid,
    source: &Path,
    options: &InventoryOptions,
    mut observer: F,
) -> Result<Vec<FileRecord>>
where
    F: FnMut(&InventoryProgress),
{
    if !source.exists() {
        return Err(CourierError::InvalidSource(source.to_path_buf()));
    }
    let root = if source.is_file() {
        source.parent().unwrap_or_else(|| Path::new("."))
    } else {
        source
    };
    let paths: Vec<PathBuf> = if source.is_file() {
        vec![source.to_path_buf()]
    } else {
        WalkDir::new(source)
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .collect()
    };
    let total_files = paths.len() as u64;
    let total_bytes = paths.iter().try_fold(0_u64, |total, path| {
        path.metadata()
            .map(|metadata| total.saturating_add(metadata.len()))
            .map_err(|source| CourierError::Io {
                path: path.clone(),
                source,
            })
    })?;
    let mut bytes_analyzed = 0_u64;
    let mut records = Vec::with_capacity(paths.len());
    observer(&InventoryProgress {
        files_analyzed: 0,
        total_files,
        bytes_analyzed: 0,
        total_bytes,
        current_path: PathBuf::new(),
    });
    for path in paths {
        let metadata = path.metadata().map_err(|source| CourierError::Io {
            path: path.clone(),
            source,
        })?;
        let relative_path = path
            .strip_prefix(root)
            .map_err(|_| CourierError::InvalidRelativePath(path.clone()))?
            .to_path_buf();
        let mtime_ns = modified_ns(&metadata)?;
        let files_analyzed = records.len() as u64;
        let bytes_before_file = bytes_analyzed;
        let digest = hash_file(
            &path,
            options.buffer_size,
            options.hash_algorithm,
            |file_bytes_analyzed| {
                observer(&InventoryProgress {
                    files_analyzed,
                    total_files,
                    bytes_analyzed: bytes_before_file.saturating_add(file_bytes_analyzed),
                    total_bytes,
                    current_path: relative_path.clone(),
                });
            },
        )?;
        bytes_analyzed = bytes_analyzed.saturating_add(metadata.len());
        records.push(FileRecord {
            id: Uuid::new_v4(),
            transfer_id,
            relative_path,
            absolute_path: path,
            size: metadata.len(),
            mtime_ns,
            hash_algorithm: options.hash_algorithm,
            sha256: digest,
            status: FileStatus::Ready,
            bytes_completed: 0,
        });
        observer(&InventoryProgress {
            files_analyzed: records.len() as u64,
            total_files,
            bytes_analyzed,
            total_bytes,
            current_path: records
                .last()
                .map(|record| record.relative_path.clone())
                .unwrap_or_default(),
        });
    }
    records.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(records)
}

pub fn verify_source_unchanged(file: &FileRecord) -> Result<()> {
    let metadata = file
        .absolute_path
        .metadata()
        .map_err(|source| CourierError::Io {
            path: file.absolute_path.clone(),
            source,
        })?;
    if metadata.len() != file.size || modified_ns(&metadata)? != file.mtime_ns {
        return Err(CourierError::SourceModified(file.absolute_path.clone()));
    }
    Ok(())
}

fn hash_file<F>(
    path: &Path,
    buffer_size: usize,
    algorithm: HashAlgorithm,
    mut observer: F,
) -> Result<String>
where
    F: FnMut(u64),
{
    let file = File::open(path).map_err(|source| CourierError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::with_capacity(buffer_size, file);
    let mut buffer = vec![0_u8; buffer_size];
    let mut sha256 = Sha256::new();
    let mut xxhash3 = Xxh3::new();
    let mut blake3 = blake3::Hasher::new();
    let mut bytes_analyzed = 0_u64;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|source| CourierError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if count == 0 {
            break;
        }
        match algorithm {
            HashAlgorithm::Sha256 => sha256.update(&buffer[..count]),
            HashAlgorithm::XxHash3 => xxhash3.update(&buffer[..count]),
            HashAlgorithm::Blake3 => {
                blake3.update(&buffer[..count]);
            }
        }
        bytes_analyzed = bytes_analyzed.saturating_add(count as u64);
        observer(bytes_analyzed);
    }
    Ok(match algorithm {
        HashAlgorithm::Sha256 => hex::encode(sha256.finalize()),
        HashAlgorithm::XxHash3 => format!("{:032x}", xxhash3.digest128()),
        HashAlgorithm::Blake3 => blake3.finalize().to_hex().to_string(),
    })
}

fn modified_ns(metadata: &std::fs::Metadata) -> Result<i64> {
    let duration = metadata
        .modified()
        .map_err(|source| CourierError::Io {
            path: PathBuf::from("<metadata>"),
            source,
        })?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CourierError::InvalidSystemTime)?;
    Ok((duration.as_secs() as i128 * 1_000_000_000_i128 + duration.subsec_nanos() as i128) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, thread, time::Duration};

    #[test]
    fn inventories_nested_and_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("nested/data.txt"), b"ocean").unwrap();
        fs::write(dir.path().join("empty"), b"").unwrap();
        let files =
            inventory_transfer(Uuid::new_v4(), dir.path(), &InventoryOptions::default()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files.iter().map(|f| f.size).sum::<u64>(), 5);
        assert!(files.iter().all(|f| f.sha256.len() == 64));
        assert!(
            files
                .iter()
                .all(|f| f.hash_algorithm == HashAlgorithm::Sha256)
        );
    }

    #[test]
    fn supports_each_configured_hash_algorithm() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        fs::write(&path, b"ocean").unwrap();
        for (algorithm, expected_length) in [
            (HashAlgorithm::Sha256, 64),
            (HashAlgorithm::XxHash3, 32),
            (HashAlgorithm::Blake3, 64),
        ] {
            let files = inventory_transfer(
                Uuid::new_v4(),
                &path,
                &InventoryOptions {
                    buffer_size: 1024,
                    hash_algorithm: algorithm,
                },
            )
            .unwrap();
            assert_eq!(files[0].hash_algorithm, algorithm);
            assert_eq!(files[0].sha256.len(), expected_length);
        }
    }

    #[test]
    fn reports_inventory_progress_for_a_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        fs::write(&path, b"ocean").unwrap();
        let mut progress = Vec::new();

        let files = inventory_transfer_observed(
            Uuid::new_v4(),
            &path,
            &InventoryOptions {
                buffer_size: 2,
                ..InventoryOptions::default()
            },
            |event| progress.push(event.clone()),
        )
        .unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, PathBuf::from("sample.txt"));
        assert_eq!(progress.first().unwrap().files_analyzed, 0);
        assert_eq!(progress.last().unwrap().files_analyzed, 1);
        assert_eq!(progress.last().unwrap().bytes_analyzed, 5);
        assert_eq!(progress.last().unwrap().total_files, 1);
        assert_eq!(progress.last().unwrap().total_bytes, 5);
    }

    #[test]
    fn detects_source_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        fs::write(&path, b"first").unwrap();
        let file = inventory_transfer(Uuid::new_v4(), &path, &InventoryOptions::default())
            .unwrap()
            .remove(0);
        thread::sleep(Duration::from_millis(2));
        fs::write(&path, b"changed").unwrap();
        assert!(matches!(
            verify_source_unchanged(&file),
            Err(CourierError::SourceModified(_))
        ));
    }
}
