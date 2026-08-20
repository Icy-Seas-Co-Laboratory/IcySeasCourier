export type TransferStatus =
  | "draft"
  | "inventorying"
  | "ready"
  | "uploading"
  | "paused"
  | "interrupted"
  | "finalizing"
  | "verifying"
  | "complete"
  | "failed"
  | "cancelled";

export interface Transfer {
  id: string;
  server_transfer_id: string | null;
  project_id: string | null;
  source_root: string;
  created_at: string;
  updated_at: string;
  status: TransferStatus;
  file_count: number;
  original_bytes: number;
}

export interface RegistryProject {
  id: string;
  project_code: string;
  name: string;
  description: string | null;
  status: string;
  created_at: string;
}

export interface RegistryAuthorization {
  registryUrl: string;
  expiresAt: string;
  projects: RegistryProject[];
  hashAlgorithm: "sha256" | "xxhash3" | "blake3";
  purpose: "upload" | "download";
  downloads: DownloadDataset[];
}

export interface DownloadDataset {
  transfer_id: string;
  project_code: string;
  source_name: string;
  file_count: number;
  original_bytes: number;
  transport_bytes: number | null;
  verified_at: string;
  hash_algorithm: "sha256" | "xxhash3" | "blake3";
}
