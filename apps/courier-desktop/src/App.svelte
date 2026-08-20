<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { confirm, open } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import { formatBytes, formatTimestamp, sourceName, statusLabel } from "./lib/format";
  import type { DownloadDataset, RegistryAuthorization, RegistryProject, Transfer } from "./lib/types";

  type Step = "invite" | "source" | "review" | "progress" | "transfers" | "downloads" | "download-progress" | "download-complete";
  interface ProgressEvent {
    transferId: string;
    confirmedBytes: number;
    totalBytes: number;
    currentFile: string;
    status: Transfer["status"];
  }
  interface InventoryProgressEvent {
    transferId: string;
    filesAnalyzed: number;
    totalFiles: number;
    bytesAnalyzed: number;
    totalBytes: number;
    currentPath: string;
  }
  interface TransferSizes {
    originalBytes: number;
    transportBytes: number | null;
  }
  interface DownloadProgressEvent {
    transferId: string;
    receivedBytes: number;
    totalBytes: number;
    restoredFiles: number;
    totalFiles: number;
    currentFile: string;
  }
  interface DownloadResult {
    transferId: string;
    destination: string;
    restoredFiles: number;
    originalBytes: number;
    transportBytes: number;
  }

  let step: Step = "invite";
  let registryUrl = "http://127.0.0.1:8020";
  let invitation = "";
  let projectId = "";
  let projects: RegistryProject[] = [];
  let sourcePath = "";
  let current: Transfer | null = null;
  let transfers: Transfer[] = [];
  let busy = false;
  let error = "";
  let confirmedBytes = 0;
  let currentFile = "";
  let packagedBytes: number | null = null;
  let uploadCommandRunning = false;
  let authorization: RegistryAuthorization | null = null;
  let refreshingTransfers = false;
  let clearingTransfers: "inventorying" | "complete" | null = null;
  let refreshingCurrent = false;
  let inventoryProgress: InventoryProgressEvent | null = null;
  let downloads: DownloadDataset[] = [];
  let selectedDownloadId = "";
  let downloadProgress: DownloadProgressEvent | null = null;
  let downloadResult: DownloadResult | null = null;

  onMount(() => {
    void loadTransfers();
    void loadRegistryEndpoint();
    void restoreAuthorization();
    let unlistenProgress: UnlistenFn | undefined;
    let unlistenInventory: UnlistenFn | undefined;
    let unlistenDownload: UnlistenFn | undefined;
    const statusTimer = window.setInterval(() => {
      if (step === "transfers") void refreshActiveTransfers();
      else void refreshVerificationStatus();
    }, 2000);
    void listen<ProgressEvent>("courier://progress", ({ payload }) => {
      if (!current || payload.transferId !== current.id) return;
      confirmedBytes = payload.confirmedBytes;
      currentFile = payload.currentFile;
      current = { ...current, status: payload.status };
    }).then((dispose) => (unlistenProgress = dispose));
    void listen<InventoryProgressEvent>("courier://inventory-progress", ({ payload }) => {
      if (busy && step === "source") inventoryProgress = payload;
    }).then((dispose) => (unlistenInventory = dispose));
    void listen<DownloadProgressEvent>("courier://download-progress", ({ payload }) => {
      if (payload.transferId === selectedDownloadId) downloadProgress = payload;
    }).then((dispose) => (unlistenDownload = dispose));
    return () => {
      window.clearInterval(statusTimer);
      unlistenProgress?.();
      unlistenInventory?.();
      unlistenDownload?.();
    };
  });

  async function loadRegistryEndpoint() {
    try {
      registryUrl = await invoke<string>("registry_endpoint");
    } catch (reason) {
      error = message(reason);
    }
  }

  async function refreshVerificationStatus() {
    if (refreshingCurrent || !current || (current.status !== "finalizing" && current.status !== "verifying")) return;
    refreshingCurrent = true;
    try {
      current = await invoke<Transfer>("refresh_transfer_status", { transferId: current.id });
      await loadTransfers();
    } catch (reason) {
      error = message(reason);
    } finally {
      refreshingCurrent = false;
    }
  }

  async function restoreAuthorization() {
    try {
      const authorization = await invoke<RegistryAuthorization | null>("current_authorization");
      if (authorization) {
        applyAuthorization(authorization);
        step = authorization.purpose === "download" ? "downloads" : "source";
      }
    } catch (reason) {
      error = message(reason);
    }
  }

  async function loadTransfers() {
    try {
      transfers = await invoke<Transfer[]>("list_transfers");
    } catch (reason) {
      error = message(reason);
    }
  }

  async function acceptInvitation() {
    error = "";
    if (!registryUrl.trim()) {
      error = "Enter the Registry address supplied by Icy Seas.";
      return;
    }
    if (!invitation.trim()) {
      error = "Enter the invitation supplied by Icy Seas.";
      return;
    }
    busy = true;
    try {
      const authorization = await invoke<RegistryAuthorization>("exchange_invitation", {
        registryUrl: registryUrl.trim(),
        invitationCode: invitation.trim(),
      });
      applyAuthorization(authorization);
      invitation = "";
      step = authorization.purpose === "download" ? "downloads" : "source";
    } catch (reason) {
      error = message(reason);
    } finally {
      busy = false;
    }
  }

  function applyAuthorization(value: RegistryAuthorization) {
    authorization = value;
    registryUrl = value.registryUrl;
    projects = value.projects.filter((project) => project.status === "active");
    downloads = value.downloads;
    if (!downloads.some((dataset) => dataset.transfer_id === selectedDownloadId)) {
      selectedDownloadId = downloads[0]?.transfer_id ?? "";
    }
    if (!projects.some((project) => project.project_code === projectId)) {
      projectId = projects[0]?.project_code ?? "";
    }
  }

  async function startDownload(dataset: DownloadDataset) {
    const selected = await open({ directory: true, multiple: false, title: "Choose where to save this dataset" });
    if (typeof selected !== "string") return;
    selectedDownloadId = dataset.transfer_id;
    downloadProgress = null;
    downloadResult = null;
    error = "";
    busy = true;
    step = "download-progress";
    try {
      downloadResult = await invoke<DownloadResult>("download_dataset", {
        transferId: dataset.transfer_id,
        destinationDirectory: selected,
      });
      step = "download-complete";
    } catch (reason) {
      error = message(reason);
      step = "downloads";
    } finally {
      busy = false;
    }
  }

  async function refreshDownloads() {
    error = "";
    busy = true;
    try {
      const currentAuthorization = await invoke<RegistryAuthorization | null>("current_authorization");
      if (!currentAuthorization || currentAuthorization.purpose !== "download") {
        throw new Error("Download authorization is no longer available; enter a new invitation.");
      }
      applyAuthorization(currentAuthorization);
    } catch (reason) {
      error = message(reason);
    } finally {
      busy = false;
    }
  }

  function downloadPercent(): number {
    if (!downloadProgress) return 0;
    if (downloadProgress.totalBytes > 0) return Math.min(100, downloadProgress.receivedBytes / downloadProgress.totalBytes * 100);
    if (downloadProgress.totalFiles > 0) return Math.min(100, downloadProgress.restoredFiles / downloadProgress.totalFiles * 100);
    return 0;
  }

  async function refreshActiveTransfers() {
    if (refreshingTransfers) return;
    const active = transfers.filter((transfer) => transfer.status === "finalizing" || transfer.status === "verifying");
    if (active.length === 0) return;
    refreshingTransfers = true;
    try {
      await Promise.all(active.map((transfer) => invoke<Transfer>("refresh_transfer_status", { transferId: transfer.id })));
      await loadTransfers();
    } catch (reason) {
      error = message(reason);
    } finally {
      refreshingTransfers = false;
    }
  }

  function transferCount(status: "inventorying" | "complete"): number {
    return transfers.filter((transfer) => transfer.status === status).length;
  }

  async function clearTransfers(status: "inventorying" | "complete") {
    const count = transferCount(status);
    if (count === 0 || clearingTransfers) return;
    const description = status === "inventorying"
      ? "abandoned inventory records"
      : "completed transfer history";
    const approved = await confirm(
      `Remove ${count} ${description} from this Mac? Courier's local records and temporary pack cache will be removed. Original datasets and Registry uploads will not be deleted.`,
      { title: "Clear Courier transfers", kind: "warning" },
    );
    if (!approved) return;
    error = "";
    clearingTransfers = status;
    try {
      await invoke<number>("clear_transfers", { status });
      if (current?.status === status) current = null;
      await loadTransfers();
    } catch (reason) {
      error = message(reason);
    } finally {
      clearingTransfers = null;
    }
  }

  async function chooseSource(kind: "file" | "folder") {
    const selected = await open({ directory: kind === "folder", multiple: false, title: `Select dataset ${kind}` });
    if (typeof selected === "string") sourcePath = selected;
  }

  async function inventory() {
    if (!sourcePath) {
      error = "Select a file or folder containing the dataset.";
      return;
    }
    busy = true;
    error = "";
    inventoryProgress = null;
    try {
      current = await invoke<Transfer>("create_inventory", {
        sourcePath,
        projectId: projectId.trim() || null,
        hashAlgorithm: authorization?.hashAlgorithm ?? "sha256",
      });
      await loadTransferSizes(current.id);
      await loadTransfers();
      step = "review";
    } catch (reason) {
      error = message(reason);
    } finally {
      busy = false;
    }
  }

  function startAnother() {
    sourcePath = "";
    current = null;
    inventoryProgress = null;
    packagedBytes = null;
    error = "";
    step = "source";
  }

  function openTransfer(transfer: Transfer) {
    current = transfer;
    confirmedBytes = transfer.status === "finalizing" || transfer.status === "verifying" || transfer.status === "complete" ? transfer.original_bytes : 0;
    currentFile = "";
    packagedBytes = null;
    error = "";
    step = transfer.status === "ready" ? "review" : "progress";
    void loadTransferSizes(transfer.id);
  }

  async function loadTransferSizes(transferId: string) {
    try {
      const sizes = await invoke<TransferSizes>("transfer_sizes", { transferId });
      if (current?.id === transferId) packagedBytes = sizes.transportBytes;
    } catch (reason) {
      error = message(reason);
    }
  }

  function sizeReduction(): string {
    if (!current || packagedBytes === null || current.original_bytes === 0) return "—";
    return `${((1 - packagedBytes / current.original_bytes) * 100).toFixed(1)}%`;
  }

  async function startUpload() {
    if (!current || uploadCommandRunning) return;
    step = "progress";
    current = { ...current, status: "uploading" };
    error = "";
    uploadCommandRunning = true;
    try {
      current = await invoke<Transfer>("start_upload", { transferId: current.id });
      await loadTransfers();
    } catch (reason) {
      error = message(reason);
      await loadTransfers();
    } finally {
      uploadCommandRunning = false;
    }
  }

  async function pauseUpload() {
    if (!current) return;
    error = "";
    try {
      await invoke("pause_upload", { transferId: current.id });
    } catch (reason) {
      error = message(reason);
    }
  }

  function progressPercent(): number {
    if (!current || current.original_bytes === 0) return current && ["finalizing", "verifying", "complete"].includes(current.status) ? 100 : 0;
    return Math.min(100, (confirmedBytes / current.original_bytes) * 100);
  }

  function inventoryPercent(): number {
    if (!inventoryProgress) return 0;
    if (inventoryProgress.totalBytes > 0) return Math.min(100, (inventoryProgress.bytesAnalyzed / inventoryProgress.totalBytes) * 100);
    return inventoryProgress.totalFiles === 0 ? 100 : 0;
  }

  function message(reason: unknown): string {
    return reason instanceof Error ? reason.message : String(reason);
  }
</script>

<svelte:head><title>Icy Seas Courier</title></svelte:head>

<div class="shell">
  <header class="masthead">
    <button class="brand" aria-label="Courier home" onclick={() => (step = "invite")}>
      <span class="mark" aria-hidden="true"><span></span></span>
      <span><strong>Icy Seas</strong><small>Courier</small></span>
    </button>
    {#if authorization?.purpose !== "download"}<button class="text-button" onclick={() => { step = "transfers"; loadTransfers(); }}>
      Transfers <span class="count">{transfers.length}</span>
    </button>{/if}
  </header>

  <main>
    {#if step !== "transfers" && step !== "invite"}
      <nav class="steps" aria-label="Transfer setup progress">
        <span class="done">1 <em>Invitation</em></span>
        <i></i>
        <span class:active={step === "source" || step === "downloads"} class:done={step === "review" || step === "download-progress" || step === "download-complete"}>2 <em>{authorization?.purpose === "download" ? "Dataset" : "Source"}</em></span>
        <i></i>
        <span class:active={step === "review" || step === "progress" || step === "download-progress" || step === "download-complete"}>3 <em>{authorization?.purpose === "download" ? "Retrieve" : step === "progress" ? "Transfer" : "Review"}</em></span>
      </nav>
    {/if}

    {#if error}<div class="notice error dismissible" role="alert"><span>{error}</span><button aria-label="Dismiss error" onclick={() => (error = "")}>×</button></div>{/if}

    {#if step === "invite"}
      <section class="panel compact">
        <p class="eyebrow">Secure data delivery</p>
        <h1>Transfer data with Icy Seas</h1>
        <p class="lede">An invitation can authorize a secure project upload or retrieval of verified project datasets.</p>
        <label for="registry-url">Registry address</label>
        <input id="registry-url" type="url" bind:value={registryUrl} placeholder="https://courier.example.org" autocomplete="url" autocapitalize="none" spellcheck="false" />
        <p class="hint">Use the HTTPS address supplied with your beta invitation. Local development may use localhost.</p>
        <label for="invitation">Invitation code</label>
        <input id="invitation" bind:value={invitation} placeholder="ISC-A7F4-KQ92-XT81" autocomplete="off" onkeydown={(event) => event.key === "Enter" && void acceptInvitation()} />
        <p class="hint">The code determines whether this device may upload or download, and which projects it can access.</p>
        <button class="primary" disabled={busy} onclick={acceptInvitation}>{busy ? "Authorizing…" : "Continue"}</button>
      </section>
    {:else if step === "downloads"}
      <section class="panel wide">
        <div class="section-heading"><div><p class="eyebrow">Project delivery</p><h1>Choose a dataset to retrieve</h1></div><button class="secondary small" disabled={busy} onclick={refreshDownloads}>{busy ? "Refreshing…" : "Refresh datasets"}</button></div>
        {#if authorization}<div class="session-banner"><i></i><span><strong>Read-only project access</strong><small>{projects.map((project) => `${project.project_code} · ${project.name}`).join("; ")} · only completed, verified datasets are shown</small></span></div>{/if}
        {#if downloads.length === 0}
          <div class="empty">No verified datasets are currently available in the authorized project. Use Refresh datasets after a transfer has completed verification.</div>
        {:else}
          <div class="download-list">
            {#each downloads as dataset}
              <article class="download-card">
                <div><span class="eyebrow">{dataset.project_code}</span><h2>{dataset.source_name}</h2><p>{dataset.file_count.toLocaleString()} files · verified {formatTimestamp(dataset.verified_at)} · {dataset.hash_algorithm.toUpperCase()}</p></div>
                <div class="download-size"><strong>{formatBytes(dataset.original_bytes)}</strong><small>{dataset.transport_bytes === null ? "Packaged size unavailable" : `${formatBytes(dataset.transport_bytes)} to transfer`}</small></div>
                <button class="primary" disabled={busy} onclick={() => startDownload(dataset)}>Choose destination</button>
              </article>
            {/each}
          </div>
        {/if}
        <div class="actions"><button class="secondary" onclick={() => (step = "invite")}>Use another invitation</button></div>
      </section>
    {:else if step === "download-progress"}
      <section class="panel">
        <p class="eyebrow centered">Secure project retrieval</p>
        <h1 class="centered">{downloads.find((item) => item.transfer_id === selectedDownloadId)?.source_name ?? selectedDownloadId}</h1>
        <div class="progress-number">{downloadPercent().toFixed(0)}%</div>
        <div class="progress-track" role="progressbar" aria-label="Download progress" aria-valuenow={downloadPercent()} aria-valuemin="0" aria-valuemax="100"><span style={`width: ${downloadPercent()}%`}></span></div>
        <div class="progress-details"><strong>{downloadProgress ? `${formatBytes(downloadProgress.receivedBytes)}${downloadProgress.totalBytes ? ` / ${formatBytes(downloadProgress.totalBytes)}` : ""}` : "Preparing secure download…"}</strong><span>{downloadProgress ? `${downloadProgress.restoredFiles.toLocaleString()} / ${downloadProgress.totalFiles.toLocaleString()} files restored` : "Requesting short-lived access"}</span></div>
        {#if downloadProgress?.currentFile}<div class="current-file"><span>Current activity</span><code>{downloadProgress.currentFile}</code></div>{/if}
        <div class="notice info">Courier downloads the verified transport, safely reconstructs the original paths, and checks every file against the immutable manifest before making the destination visible.</div>
      </section>
    {:else if step === "download-complete" && downloadResult}
      <section class="panel">
        <div class="state-icon ready" aria-hidden="true">✓</div>
        <p class="eyebrow centered">Dataset retrieved and verified</p>
        <h1 class="centered">{downloads.find((item) => item.transfer_id === downloadResult?.transferId)?.source_name ?? downloadResult.transferId}</h1>
        <div class="summary">
          <div><span>Files restored</span><strong>{downloadResult.restoredFiles.toLocaleString()}</strong></div>
          <div><span>Original size</span><strong>{formatBytes(downloadResult.originalBytes)}</strong></div>
          <div><span>Downloaded</span><strong>{formatBytes(downloadResult.transportBytes)}</strong></div>
          <div><span>Integrity</span><strong>Manifest matched</strong></div>
        </div>
        <div class="provenance"><span>Saved to</span><code>{downloadResult.destination}</code></div>
        <div class="actions"><button class="primary" onclick={() => (step = "downloads")}>Retrieve another dataset</button></div>
      </section>
    {:else if step === "source"}
      <section class="panel">
        <p class="eyebrow">Prepare transfer</p>
        <h1>Choose the source dataset</h1>
        {#if authorization}<div class="session-banner"><i></i><span><strong>Registry authorized</strong><small>{projects.length} active {projects.length === 1 ? "project" : "projects"} · {authorization.registryUrl} · session renews automatically</small></span></div>{/if}
        <div class="field-grid">
          <div>
            <label for="project">Authorized project</label>
            <select id="project" bind:value={projectId}>
              {#each projects as project}
                <option value={project.project_code}>{project.project_code} — {project.name}</option>
              {/each}
            </select>
          </div>
          <div>
            <p class="field-label">Source file or folder</p>
            <div class="picker" class:selected={sourcePath !== ""}>
              <span class="folder" aria-hidden="true"></span>
              {#if sourcePath}<span><strong>{sourceName(sourcePath)}</strong><small>{sourcePath}</small></span>{:else}<span><strong>Select a source</strong><small>Files remain in their original location</small></span>{/if}
            </div>
            <div class="source-buttons">
              <button class="secondary" disabled={busy} onclick={() => chooseSource("folder")}>Choose folder</button>
              <button class="secondary" disabled={busy} onclick={() => chooseSource("file")}>Choose file</button>
            </div>
          </div>
        </div>
        {#if busy}
          <div class="analysis-progress" role="status" aria-live="polite">
            <div class="analysis-heading">
              <strong>{inventoryProgress ? `Analyzed ${inventoryProgress.filesAnalyzed.toLocaleString()} of ${inventoryProgress.totalFiles.toLocaleString()} files…` : "Discovering files…"}</strong>
              {#if inventoryProgress}<span>{formatBytes(inventoryProgress.bytesAnalyzed)} / {formatBytes(inventoryProgress.totalBytes)}</span>{/if}
            </div>
            <div class="progress-track" role="progressbar" aria-label="Dataset analysis progress" aria-valuenow={inventoryPercent()} aria-valuemin="0" aria-valuemax="100"><span style={`width: ${inventoryPercent()}%`}></span></div>
            {#if inventoryProgress?.currentPath}<code>{inventoryProgress.currentPath}</code>{/if}
          </div>
        {/if}
        <div class="actions"><button class="secondary" onclick={() => (step = "invite")}>Back</button><button class="primary" disabled={busy} onclick={inventory}>{busy ? "Inventorying…" : "Review dataset"}</button></div>
      </section>
    {:else if step === "review" && current}
      <section class="panel">
        <div class="state-icon ready" aria-hidden="true">✓</div>
        <p class="eyebrow centered">Ready to transfer</p>
        <h1 class="centered">{sourceName(current.source_root)}</h1>
        <div class="summary">
          <div><span>Project</span><strong>{current.project_id ?? "Assigned by invitation"}</strong></div>
          <div><span>Files</span><strong>{current.file_count.toLocaleString()}</strong></div>
          <div><span>Original size</span><strong>{formatBytes(current.original_bytes)}</strong></div>
          <div><span>Packaged size</span><strong>{packagedBytes === null ? "Calculating…" : formatBytes(packagedBytes)}</strong></div>
          <div><span>Size reduction</span><strong>{sizeReduction()}</strong></div>
          <div><span>Integrity</span><strong>{(authorization?.hashAlgorithm ?? "sha256").toUpperCase()} inventoried</strong></div>
        </div>
        <div class="provenance"><span>Source</span><code>{current.source_root}</code></div>
        <div class="notice info">Courier will register the immutable manifest, then upload each part through short-lived Registry authorization. Uploaded data is not marked scientifically verified.</div>
        <div class="actions split"><button class="secondary" onclick={startAnother}>Choose another source</button><button class="primary" onclick={startUpload}>Upload dataset</button></div>
      </section>
    {:else if step === "progress" && current}
      <section class="panel">
        <p class="eyebrow centered">{current.status === "paused" ? "Transfer paused" : current.status === "complete" ? "Dataset verified" : current.status === "verifying" ? "Verifying integrity" : current.status === "finalizing" ? "Upload received" : "Secure data delivery"}</p>
        <h1 class="centered">{sourceName(current.source_root)}</h1>
        <div class="progress-number">{progressPercent().toFixed(0)}%</div>
        <div class="progress-track" role="progressbar" aria-label="Upload progress" aria-valuenow={progressPercent()} aria-valuemin="0" aria-valuemax="100"><span style={`width: ${progressPercent()}%`}></span></div>
        <div class="progress-details"><strong>{formatBytes(confirmedBytes)} / {formatBytes(current.original_bytes)}</strong><span>{statusLabel(current.status)}</span></div>
        {#if currentFile}<div class="current-file"><span>Current file</span><code>{currentFile}</code></div>{/if}
        {#if current.status === "complete"}
          <div class="notice info">The Registry independently reconstructed every logical file and matched its size and {(authorization?.hashAlgorithm ?? "sha256").toUpperCase()} digest to the immutable manifest.</div>
          {#if current.server_transfer_id}<div class="provenance"><span>Registry transfer</span><code>{current.server_transfer_id}</code></div>{/if}
          <div class="actions"><button class="primary" onclick={() => { step = "transfers"; loadTransfers(); }}>View transfers</button></div>
        {:else if current.status === "verifying"}
          <div class="notice info">The upload is complete. The Registry is independently streaming and checking every logical file.</div>
        {:else if current.status === "finalizing"}
          <div class="notice info">The Registry accepted every object and queued the transfer for independent verification. This dataset is not yet marked complete.</div>
          <div class="actions"><button class="primary" onclick={() => { step = "transfers"; loadTransfers(); }}>View transfers</button></div>
        {:else if current.status === "failed"}
          <div class="notice error">Independent verification failed. The Registry retained the evidence for operator review; this dataset is not marked complete.</div>
          <div class="actions"><button class="primary" onclick={() => { step = "transfers"; loadTransfers(); }}>View transfers</button></div>
        {:else if current.status === "paused" || current.status === "interrupted" || (current.status === "uploading" && !uploadCommandRunning)}
          <div class="notice info">Confirmed parts are safely recorded. Resume will reconcile remote state before sending anything else.</div>
          <div class="actions"><button class="primary" disabled={uploadCommandRunning} onclick={startUpload}>{uploadCommandRunning ? "Resuming…" : "Resume"}</button></div>
        {:else}
          <div class="connection"><i></i><span>Connection active</span><small>Completed parts are saved continuously</small></div>
          <div class="actions"><button class="secondary" disabled={!uploadCommandRunning} onclick={pauseUpload}>Pause</button></div>
        {/if}
      </section>
    {:else if step === "transfers"}
      <section class="panel wide">
        <div class="section-heading"><div><p class="eyebrow">Local state</p><h1>Transfers</h1></div><div class="heading-actions"><button class="secondary small" disabled={refreshingTransfers} onclick={refreshActiveTransfers}>{refreshingTransfers ? "Refreshing…" : "Refresh status"}</button><button class="primary small" onclick={() => (step = authorization ? "source" : "invite")}>New transfer</button></div></div>
        {#if transferCount("inventorying") > 0 || transferCount("complete") > 0}
          <div class="cleanup-bar">
            <span>Remove local records</span>
            <div>
              {#if transferCount("inventorying") > 0}<button class="secondary small" disabled={busy || clearingTransfers !== null} onclick={() => clearTransfers("inventorying")}>{clearingTransfers === "inventorying" ? "Clearing…" : `Clear inventorying (${transferCount("inventorying")})`}</button>{/if}
              {#if transferCount("complete") > 0}<button class="secondary small" disabled={clearingTransfers !== null} onclick={() => clearTransfers("complete")}>{clearingTransfers === "complete" ? "Clearing…" : `Clear completed (${transferCount("complete")})`}</button>{/if}
            </div>
          </div>
        {/if}
        {#if transfers.length === 0}<div class="empty"><div class="state-icon">↑</div><h2>No transfers yet</h2><p>Start with an invitation and choose a dataset file or folder.</p></div>{:else}
          <div class="transfer-list">
            {#each transfers as transfer}
              <button class="transfer-row" onclick={() => openTransfer(transfer)}>
                <div><strong>{sourceName(transfer.source_root)}</strong><small>{transfer.project_id ?? "Project pending"} · {transfer.file_count.toLocaleString()} files · {formatBytes(transfer.original_bytes)}</small><small>Updated {formatTimestamp(transfer.updated_at)}{transfer.server_transfer_id ? ` · ${transfer.server_transfer_id}` : ""}</small></div>
                <span class:verified={transfer.status === "complete"} class="status">{statusLabel(transfer.status)}</span>
              </button>
            {/each}
          </div>
        {/if}
      </section>
    {/if}
  </main>

  <footer><span><i></i> Local state available</span><span>Uploads are verified independently by Icy Seas</span></footer>
</div>
