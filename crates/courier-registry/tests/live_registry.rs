use std::time::{Duration, Instant};
use std::{env, fs};

use courier_core::{
    InventoryOptions, RetryPolicy, Transfer, TransferStore, TransportMemberRecord,
    TransportObjectKind, TransportObjectRecord, inventory_transfer,
};
use courier_registry::{
    ManifestTransportPlan, RegistryClient, RegistryMultipartStore, RegistryObjectBinding,
};
use courier_transfer::{MultipartLimits, complete_uploaded_file, plan_parts, upload_missing_parts};

#[tokio::test]
#[ignore = "requires the local Registry stack and COURIER_TEST_INVITATION"]
async fn rust_client_completes_registry_authorized_upload() {
    let base_url =
        env::var("COURIER_REGISTRY_URL").unwrap_or_else(|_| "http://127.0.0.1:8010".into());
    let invitation = env::var("COURIER_TEST_INVITATION").expect("test invitation is required");
    let exchanged = RegistryClient::unauthenticated(&base_url)
        .exchange_invitation(&invitation, "courier-rust-e2e")
        .await
        .unwrap();
    let session = RegistryClient::unauthenticated(&base_url)
        .refresh_session(&exchanged.refresh_token)
        .await
        .unwrap();
    let project = &session.projects[0].project_code;
    let client = RegistryClient::authenticated(&base_url, session.access_token);
    let hash_algorithm = client.system_config().await.unwrap().hash_algorithm;

    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("cast.csv"),
        b"temperature,salinity\n-1.2,31.4\n",
    )
    .unwrap();
    let mut store = TransferStore::open_in_memory().unwrap();
    let transfer = Transfer::draft(directory.path().to_owned(), Some(project.clone()));
    store.create_transfer(&transfer).unwrap();
    let files = inventory_transfer(
        transfer.id,
        directory.path(),
        &InventoryOptions {
            hash_algorithm,
            ..InventoryOptions::default()
        },
    )
    .unwrap();
    store.replace_inventory(transfer.id, &files).unwrap();
    let objects = files
        .iter()
        .map(|file| TransportObjectRecord {
            id: file.id,
            transfer_id: transfer.id,
            kind: TransportObjectKind::File,
            compression: "none".into(),
            encoding_version: 1,
            original_bytes: file.size,
            transport_bytes: Some(file.size),
            cache_path: None,
        })
        .collect::<Vec<_>>();
    let members = files
        .iter()
        .map(|file| TransportMemberRecord {
            object_id: file.id,
            file_id: file.id,
            member_index: 0,
        })
        .collect::<Vec<_>>();
    store
        .replace_transport_plan(transfer.id, &objects, &members)
        .unwrap();
    for file in &files {
        store
            .replace_part_plan(
                file.id,
                &plan_parts(file.id, file.size, MultipartLimits::default()).unwrap(),
            )
            .unwrap();
    }
    let transfer = store.get_transfer(transfer.id).unwrap().unwrap();
    let registered = client
        .register_transfer(&transfer, project, "rust-e2e", hash_algorithm)
        .await
        .unwrap();
    let receipt = client
        .submit_manifest(
            &transfer,
            &registered.public_id,
            project,
            "rust-e2e",
            &files,
            ManifestTransportPlan {
                objects: &objects,
                members: &members,
            },
        )
        .await
        .unwrap();
    let bindings = receipt
        .transport_objects
        .iter()
        .map(|object| RegistryObjectBinding {
            server_object_id: object.id,
            object_key: object.object_key.clone(),
        })
        .collect::<Vec<_>>();
    let remote = RegistryMultipartStore::new(client.clone(), &registered.public_id, bindings);
    for local in &files {
        let registered_object = receipt
            .transport_objects
            .iter()
            .find(|object| object.id == local.id)
            .unwrap();
        store
            .bind_registry_object(
                local.id,
                registered_object.id,
                &registered_object.object_key,
            )
            .unwrap();
        upload_missing_parts(&store, &remote, local, &RetryPolicy::default())
            .await
            .unwrap();
        complete_uploaded_file(&store, &remote, local, &RetryPolicy::default())
            .await
            .unwrap();
    }
    client
        .finalize_transfer(&registered.public_id)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = client.transfer_status(&registered.public_id).await.unwrap();
        if status.status == "complete" {
            break;
        }
        assert_ne!(status.status, "failed", "{:?}", status.verification_error);
        assert!(Instant::now() < deadline, "verification timed out");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
