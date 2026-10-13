//! Reusable transfer engine for Icy Seas Courier.

pub mod db;
pub mod error;
pub mod events;
pub mod inventory;
pub mod model;
pub mod retry;

pub use db::TransferStore;
pub use error::{CourierError, Result};
pub use events::{TransferErrorClass, TransferEvent, TransferEventDetails};
pub use inventory::{
    InventoryOptions, InventoryProgress, digest_file, inventory_transfer,
    inventory_transfer_observed, verify_source_unchanged,
};
pub use model::{
    FileRecord, FileStatus, HashAlgorithm, PartRecord, PartStatus, RegistrySessionRecord, Transfer,
    TransferFileProgress, TransferFileProgressPage, TransferStatus, TransportMemberRecord,
    TransportObjectKind, TransportObjectRecord,
};
pub use retry::RetryPolicy;
