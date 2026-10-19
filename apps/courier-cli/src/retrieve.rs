use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use courier_core::{HashAlgorithm, digest_file};
use courier_pack::decode_pack;
use courier_registry::{RegistryClient, RegistryDownloadPlan, RegistryInvitationPurpose};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
struct Manifest {
    files: Vec<ManifestFile>,
}

#[derive(Deserialize)]
struct ManifestFile {
    path: String,
    size: u64,
    mtime: DateTime<Utc>,
    digest: Digest,
    transport: Transport,
}

#[derive(Deserialize)]
struct Digest {
    algorithm: HashAlgorithm,
    value: String,
}

#[derive(Deserialize)]
struct Transport {
    object_id: Uuid,
    member_index: u32,
}

fn safe_path(root: &Path, value: &str) -> Result<PathBuf> {
    ensure!(
        !value.is_empty()
            && !value.contains('\\')
            && !value.contains('\0')
            && value
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "unsafe manifest path: {value}"
    );
    let path = Path::new(value);
    ensure!(
        path.components()
            .all(|part| matches!(part, std::path::Component::Normal(_))),
        "unsafe manifest path: {value}"
    );
    Ok(root.join(path))
}

fn verify(path: &Path, file: &ManifestFile) -> Result<()> {
    ensure!(
        fs::metadata(path)?.len() == file.size,
        "size mismatch: {}",
        file.path
    );
    ensure!(
        digest_file(path, file.digest.algorithm)? == file.digest.value,
        "digest mismatch: {}",
        file.path
    );
    filetime::set_file_mtime(
        path,
        filetime::FileTime::from_unix_time(
            file.mtime.timestamp(),
            file.mtime.timestamp_subsec_nanos(),
        ),
    )?;
    Ok(())
}

fn restore_object(
    object_kind: &str,
    cache: &Path,
    partial: &Path,
    expected: &[&ManifestFile],
) -> Result<()> {
    match object_kind {
        "file" => {
            ensure!(
                expected.len() == 1 && expected[0].transport.member_index == 0,
                "invalid standalone object membership"
            );
            let file = expected[0];
            let output = safe_path(partial, &file.path)?;
            fs::create_dir_all(output.parent().context("missing file parent")?)?;
            fs::copy(cache, &output)?;
            verify(&output, file)?;
        }
        "pack" => {
            let mut seen = 0;
            decode_pack(fs::File::open(cache)?, |header, reader| {
                let file = expected.get(seen).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "pack has extra members")
                })?;
                if header.path != file.path
                    || header.size != file.size
                    || header.digest_algorithm != file.digest.algorithm
                    || header.digest != file.digest.value
                    || file.transport.member_index as usize != seen
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "pack member does not match the manifest",
                    )
                    .into());
                }
                let output = safe_path(partial, &file.path)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                fs::create_dir_all(output.parent().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "missing file parent")
                })?)?;
                let mut writer = fs::File::create(&output)?;
                let copied = io::copy(reader, &mut writer)?;
                writer.flush()?;
                if copied != file.size {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "pack member size mismatch",
                    )
                    .into());
                }
                verify(&output, file)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                seen += 1;
                Ok(())
            })?;
            ensure!(seen == expected.len(), "pack omitted manifest members");
        }
        other => bail!("unsupported transport object kind: {other}"),
    }
    Ok(())
}

async fn restore(
    client: &RegistryClient,
    plan: &RegistryDownloadPlan,
    partial: &Path,
    cache: &Path,
) -> Result<()> {
    let manifest: Manifest = serde_json::from_value(plan.manifest.clone())?;
    ensure!(
        manifest.files.len() as u64 == plan.dataset.file_count,
        "manifest file count differs from verified dataset"
    );
    let mut paths = HashSet::new();
    let mut by_object: HashMap<Uuid, Vec<&ManifestFile>> = HashMap::new();
    for file in &manifest.files {
        safe_path(partial, &file.path)?;
        ensure!(
            paths.insert(file.path.to_lowercase()),
            "duplicate manifest path: {}",
            file.path
        );
        by_object
            .entry(file.transport.object_id)
            .or_default()
            .push(file);
    }
    for files in by_object.values_mut() {
        files.sort_by_key(|file| file.transport.member_index);
    }
    for (index, object) in plan.objects.iter().enumerate() {
        let files = by_object
            .remove(&object.object_id)
            .with_context(|| format!("manifest does not reference object {}", object.object_id))?;
        eprintln!(
            "Retrieving object {} of {} ({} files)",
            index + 1,
            plan.objects.len(),
            files.len()
        );
        let authorized = client
            .authorize_download_object(&plan.dataset.transfer_id, object.object_id)
            .await?;
        let url = authorized
            .url
            .context("Registry omitted authorized object URL")?;
        let cache_path = cache.join(object.object_id.to_string());
        let received = client
            .download_object_resumable(
                &url,
                &cache_path,
                object.transport_bytes,
                Arc::new(AtomicBool::new(false)),
                |_| {},
            )
            .await?;
        if let Some(expected) = object.transport_bytes {
            ensure!(
                received == expected,
                "downloaded object has unexpected size"
            );
        }
        restore_object(&object.kind, &cache_path, partial, &files)?;
        fs::remove_file(cache_path)?;
    }
    ensure!(
        by_object.is_empty(),
        "download plan omitted manifest objects"
    );
    Ok(())
}

pub async fn run(registry: &str, transfer: &str, destination: &Path, json: bool) -> Result<()> {
    let url = url::Url::parse(registry).context("invalid Registry URL")?;
    ensure!(
        url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))),
        "Registry URL must use HTTPS except on loopback"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Registry URL must not contain credentials, a query, or a fragment"
    );
    ensure!(
        !destination.exists(),
        "destination already exists: {}",
        destination.display()
    );
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    ensure!(
        parent.is_dir(),
        "destination parent does not exist: {}",
        parent.display()
    );
    let name = destination
        .file_name()
        .context("destination must name a new directory")?
        .to_string_lossy();
    let invitation = env::var("COURIER_DOWNLOAD_INVITATION")
        .context("set COURIER_DOWNLOAD_INVITATION to a read-only download invitation")?;
    let anonymous = RegistryClient::unauthenticated(registry);
    let session = anonymous
        .exchange_invitation(invitation.trim(), "courier-cli-retrieve")
        .await?;
    ensure!(
        session.purpose == RegistryInvitationPurpose::Download,
        "the invitation does not grant read-only download access"
    );
    let client = RegistryClient::renewable(
        registry,
        session.access_token,
        session.refresh_token,
        Arc::new(|_| Ok(())),
    );
    let plan = client.download_plan(transfer).await?;
    ensure!(
        plan.dataset.transfer_id == transfer,
        "Registry returned a different transfer"
    );
    let partial = parent.join(format!(".{name}.courier-partial-{}", Uuid::new_v4()));
    let cache = parent.join(format!(".{name}.courier-cache-{}", Uuid::new_v4()));
    fs::create_dir(&partial)?;
    fs::create_dir(&cache)?;
    let result = restore(&client, &plan, &partial, &cache).await;
    let _ = fs::remove_dir_all(&cache);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&partial);
        return Err(error);
    }
    if let Err(error) = fs::rename(&partial, destination) {
        let _ = fs::remove_dir_all(&partial);
        return Err(error.into());
    }
    if json {
        println!(
            "{}",
            serde_json::json!({
                "transfer_id": plan.dataset.transfer_id,
                "destination": destination.display().to_string(),
                "file_count": plan.dataset.file_count,
                "verified": true,
            })
        );
    } else {
        println!(
            "Restored and verified {} files to {}",
            plan.dataset.file_count,
            destination.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use courier_core::{InventoryOptions, inventory_transfer};
    use courier_pack::encode_pack;

    #[test]
    fn manifest_paths_reject_traversal_and_duplicates() {
        let root = Path::new("/tmp/dataset");
        for path in [
            "",
            "../escape",
            "/absolute",
            "a/../b",
            "a\\b",
            "a//b",
            "a/./b",
        ] {
            assert!(safe_path(root, path).is_err(), "{path}");
        }
        assert!(safe_path(root, "folder/file.bin").is_ok());
    }

    #[test]
    fn restores_pack_and_rejects_changed_logical_bytes() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("nested/a.txt"), b"original").unwrap();
        let files =
            inventory_transfer(Uuid::new_v4(), &source, &InventoryOptions::default()).unwrap();
        let mut pack = Vec::new();
        encode_pack(&files.iter().collect::<Vec<_>>(), &mut pack, 3).unwrap();
        let cache = root.path().join("pack.zst");
        fs::write(&cache, pack).unwrap();
        let file = ManifestFile {
            path: "nested/a.txt".into(),
            size: files[0].size,
            mtime: Utc::now(),
            digest: Digest {
                algorithm: files[0].hash_algorithm,
                value: files[0].sha256.clone(),
            },
            transport: Transport {
                object_id: Uuid::new_v4(),
                member_index: 0,
            },
        };
        restore_object("pack", &cache, &destination, &[&file]).unwrap();
        assert_eq!(
            fs::read(destination.join("nested/a.txt")).unwrap(),
            b"original"
        );
        fs::remove_file(destination.join("nested/a.txt")).unwrap();
        let changed = ManifestFile {
            digest: Digest {
                algorithm: file.digest.algorithm,
                value: "0".repeat(64),
            },
            ..file
        };
        assert!(restore_object("pack", &cache, &destination, &[&changed]).is_err());
    }
}
