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
  expiresAt: string;
  projects: RegistryProject[];
  hashAlgorithm: "sha256" | "xxhash3" | "blake3";
}
