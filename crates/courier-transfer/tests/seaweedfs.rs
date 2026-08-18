use std::{
    fs,
    net::{TcpStream, ToSocketAddrs},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use courier_core::{InventoryOptions, RetryPolicy, Transfer, TransferStore, inventory_transfer};
use courier_transfer::{
    CompletionOutcome, MultipartLimits, MultipartStore, S3MultipartStore, S3StoreConfig,
    complete_uploaded_file, plan_parts, upload_missing_parts,
};
use uuid::Uuid;

/// Exercises the critical recovery path against the repository's real
/// SeaweedFS service. The first part reaches remote storage, but its ETag is
/// intentionally not committed locally. Both the local database connection and
/// SeaweedFS are then restarted before Courier resumes.
#[tokio::test]
#[ignore = "requires Docker and the local SeaweedFS development service"]
async fn resumes_after_local_reopen_and_seaweedfs_restart() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("source");
    fs::create_dir(&source_dir).unwrap();
    let source_path = source_dir.join("large.bin");
    fs::File::create(&source_path)
        .unwrap()
        .set_len(11 * 1024 * 1024)
        .unwrap();
    let database_path = dir.path().join("courier.db");

    let mut database = TransferStore::open(&database_path).unwrap();
    let transfer = Transfer::draft(source_dir.clone(), Some("P26014".into()));
    database.create_transfer(&transfer).unwrap();
    let files = inventory_transfer(transfer.id, &source_dir, &InventoryOptions::default()).unwrap();
    database.replace_inventory(transfer.id, &files).unwrap();
    let file = &files[0];
    let limits = MultipartLimits {
        target_part_size: 5 * 1024 * 1024,
        minimum_part_size: 5 * 1024 * 1024,
        maximum_part_size: 5 * 1024 * 1024 * 1024,
        maximum_parts: 10_000,
    };
    database
        .replace_part_plan(file.id, &plan_parts(file.id, file.size, limits).unwrap())
        .unwrap();

    let bucket = format!("courier-recovery-{}", Uuid::new_v4().simple());
    let remote =
        S3MultipartStore::from_config(S3StoreConfig::seaweedfs(bucket, "http://127.0.0.1:8333"))
            .await;
    remote.ensure_bucket().await.unwrap();
    let session = remote
        .begin(&format!("incoming/{}/payload", file.id.simple()))
        .await
        .unwrap();
    database
        .set_upload_session(file.id, &session.object_key, &session.upload_id)
        .unwrap();

    // Simulate death after S3 accepted part 1 but before SQLite committed it.
    remote
        .upload_part(&session, 1, vec![0; 5 * 1024 * 1024])
        .await
        .unwrap();
    drop(database);

    restart_seaweedfs();
    wait_for_seaweedfs(Duration::from_secs(30));

    let database = TransferStore::open(&database_path).unwrap();
    let retry = RetryPolicy {
        base: Duration::from_millis(100),
        maximum: Duration::from_secs(1),
        max_attempts: 30,
    };
    let progress = upload_missing_parts(&database, &remote, file, &retry)
        .await
        .unwrap();
    assert_eq!(progress.parts_already_present, 1);
    assert_eq!(progress.parts_uploaded, 2);
    assert_eq!(progress.source_bytes_confirmed, file.size);

    let outcome = complete_uploaded_file(&database, &remote, file, &retry)
        .await
        .unwrap();
    assert_eq!(outcome, CompletionOutcome::Completed);
    assert!(remote.object_exists(&session.object_key).await.unwrap());
}

fn restart_seaweedfs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = Command::new("docker-compose")
        .args(["restart", "seaweedfs"])
        .current_dir(root)
        .status()
        .expect("run docker-compose restart");
    assert!(status.success(), "SeaweedFS restart failed");
}

fn wait_for_seaweedfs(timeout: Duration) {
    let address = "127.0.0.1:8333".to_socket_addrs().unwrap().next().unwrap();
    let started = Instant::now();
    while started.elapsed() < timeout {
        if TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(250));
    }
    panic!("SeaweedFS did not become ready within {timeout:?}");
}
