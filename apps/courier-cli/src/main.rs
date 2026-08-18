use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use courier_core::{
    HashAlgorithm, InventoryOptions, RetryPolicy, Transfer, TransferStatus, TransferStore,
    inventory_transfer,
};
use courier_transfer::{
    MultipartLimits, S3MultipartStore, S3StoreConfig, complete_uploaded_file, plan_parts,
    upload_missing_parts,
};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "courier",
    version,
    about = "Reliable scientific data transfer for Icy Seas"
)]
struct Cli {
    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Override the local state database.
    #[arg(long, global = true, env = "COURIER_STATE_DB")]
    state_db: Option<PathBuf>,
    /// Logical-file digest algorithm. Must match the target Registry policy.
    #[arg(
        long,
        global = true,
        env = "COURIER_HASH_ALGORITHM",
        default_value = "sha256"
    )]
    hash_algorithm: HashAlgorithm,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inventory and persist a file or directory as a new transfer.
    Upload {
        source: PathBuf,
        #[arg(long)]
        project: Option<String>,
    },
    /// List all locally known transfers.
    Transfers,
    /// Show transfers that need attention or can be resumed.
    Status,
    /// Send a ready transfer to the configured S3-compatible store.
    Send { transfer_id: Uuid },
    /// Inspect one transfer.
    Inspect { transfer_id: Uuid },
    /// Pause an uploading transfer.
    Pause { transfer_id: Uuid },
    /// Resume a paused or interrupted transfer.
    Resume { transfer_id: Uuid },
    /// Retry an interrupted transfer.
    Retry { transfer_id: Uuid },
    /// Cancel a transfer without deleting its state.
    Cancel { transfer_id: Uuid },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let db_path = cli.state_db.unwrap_or(default_db_path()?);
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut store = TransferStore::open(&db_path)
        .with_context(|| format!("open state database {}", db_path.display()))?;

    match cli.command {
        Command::Upload { source, project } => {
            let source = source
                .canonicalize()
                .with_context(|| format!("resolve source {}", source.display()))?;
            let transfer = Transfer::draft(source.clone(), project);
            store.create_transfer(&transfer)?;
            store.transition(transfer.id, TransferStatus::Inventorying)?;
            match inventory_transfer(
                transfer.id,
                &source,
                &InventoryOptions {
                    hash_algorithm: cli.hash_algorithm,
                    ..InventoryOptions::default()
                },
            ) {
                Ok(files) => {
                    store.replace_inventory(transfer.id, &files)?;
                    for file in &files {
                        let parts = plan_parts(file.id, file.size, MultipartLimits::default())?;
                        store.replace_part_plan(file.id, &parts)?;
                    }
                    store.transition(transfer.id, TransferStatus::Ready)?;
                    let ready = store.get_transfer(transfer.id)?.expect("created transfer");
                    print_transfer(&ready, cli.json)?;
                    if !cli.json {
                        println!(
                            "Inventory complete. Run `courier send {}` to begin.",
                            ready.id
                        );
                    }
                }
                Err(error) => {
                    store.transition(transfer.id, TransferStatus::Failed)?;
                    return Err(error.into());
                }
            }
        }
        Command::Transfers => print_transfers(&store.list_transfers()?, cli.json)?,
        Command::Status => print_transfers(&store.incomplete_transfers()?, cli.json)?,
        Command::Send { transfer_id } => {
            send_transfer(&store, transfer_id).await?;
            print_transfer(&required(&store, transfer_id)?, cli.json)?;
        }
        Command::Inspect { transfer_id } => {
            print_transfer(&required(&store, transfer_id)?, cli.json)?
        }
        Command::Pause { transfer_id } => {
            change(&store, transfer_id, TransferStatus::Paused, cli.json)?
        }
        Command::Resume { transfer_id } | Command::Retry { transfer_id } => {
            change(&store, transfer_id, TransferStatus::Uploading, cli.json)?
        }
        Command::Cancel { transfer_id } => {
            change(&store, transfer_id, TransferStatus::Cancelled, cli.json)?
        }
    }
    Ok(())
}

async fn send_transfer(store: &TransferStore, id: Uuid) -> Result<()> {
    let transfer = required(store, id)?;
    match transfer.status {
        TransferStatus::Ready | TransferStatus::Paused | TransferStatus::Interrupted => {
            store.transition(id, TransferStatus::Uploading)?;
        }
        TransferStatus::Uploading => {}
        status => anyhow::bail!("transfer {id} cannot upload from state {status}"),
    }

    let endpoint =
        env::var("COURIER_S3_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:8333".into());
    let bucket = env::var("COURIER_S3_BUCKET").unwrap_or_else(|_| "icy-seas-incoming".into());
    let mut config = S3StoreConfig::seaweedfs(bucket, endpoint);
    if let Ok(region) = env::var("COURIER_S3_REGION") {
        config.region = region;
    }
    let remote = S3MultipartStore::from_config(config).await;
    let retry = RetryPolicy::default();

    let result: Result<()> = async {
        remote.ensure_bucket().await?;
        for file in store.files_for_transfer(id)? {
            if file.status == courier_core::FileStatus::Uploaded {
                continue;
            }
            upload_missing_parts(store, &remote, &file, &retry).await?;
            complete_uploaded_file(store, &remote, &file, &retry).await?;
            store.mark_file_uploaded(file.id)?;
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            store.transition(id, TransferStatus::Finalizing)?;
            println!(
                "All objects uploaded directly. This development CLI does not register the transfer for Registry verification."
            );
            Ok(())
        }
        Err(error) => {
            store.transition(id, TransferStatus::Interrupted)?;
            Err(error)
        }
    }
}

fn default_db_path() -> Result<PathBuf> {
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".local/state")))
        .context("could not determine state directory; pass --state-db")?;
    Ok(base.join("icy-seas-courier/courier.db"))
}

fn required(store: &TransferStore, id: Uuid) -> Result<Transfer> {
    store
        .get_transfer(id)?
        .with_context(|| format!("transfer not found: {id}"))
}

fn change(store: &TransferStore, id: Uuid, status: TransferStatus, json: bool) -> Result<()> {
    store.transition(id, status)?;
    print_transfer(&required(store, id)?, json)
}

fn print_transfers(transfers: &[Transfer], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(transfers)?);
    } else if transfers.is_empty() {
        println!("No transfers found.");
    } else {
        for transfer in transfers {
            println!(
                "{}  {:<12}  {:>6} files  {} bytes  {}",
                transfer.id,
                transfer.status,
                transfer.file_count,
                transfer.original_bytes,
                transfer.source_root.display()
            );
        }
    }
    Ok(())
}

fn print_transfer(transfer: &Transfer, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(transfer)?);
    } else {
        println!("Transfer: {}", transfer.id);
        println!("Status:   {}", transfer.status);
        println!(
            "Project:  {}",
            transfer.project_id.as_deref().unwrap_or("not assigned")
        );
        println!("Source:   {}", transfer.source_root.display());
        println!("Files:    {}", transfer.file_count);
        println!("Bytes:    {}", transfer.original_bytes);
    }
    Ok(())
}
