<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { confirm, open } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import { formatBytes, formatTimestamp, sourceName, statusLabel } from "./lib/format";
  import type { DownloadDataset, RegistryAuthorization, RegistryProject, Transfer } from "./lib/types";

  type Step = "home" | "invite" | "source" | "review" | "progress" | "transfers" | "downloads" | "download-progress" | "download-complete";
  interface DeviceAccessStatus {
    hasStoredAuthorization: boolean;
    biometricAvailable: boolean;
    biometricLabel: string;
    authenticationRequired: boolean;
  }
  interface ProgressEvent {
    transferId: string;
    confirmedBytes: number;
    sentBytes: number;
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
    phase: "analyzing" | "packaging";
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
  interface ActivityState {
    title: string;
    detail: string;
    startedAt: number;
    updatedAt: number;
    active: boolean;
    outcome: "working" | "success" | "warning";
  }

  let step: Step = "home";
  let registryUrl = "https://courier.icyseascolab.io";
  let invitation = "";
  let projectId = "";
  let projects: RegistryProject[] = [];
  let sourcePath = "";
  let current: Transfer | null = null;
  let transfers: Transfer[] = [];
  let busy = false;
  let error = "";
  let confirmedBytes = 0;
  let sentBytes = 0;
  let uploadRate = 0;
  let lastUploadSample: { bytes: number; at: number } | null = null;
  let currentFile = "";
  let packagedBytes: number | null = null;
  let uploadCommandRunning = false;
  let authorization: RegistryAuthorization | null = null;
  let refreshingTransfers = false;
  let clearingTransfers: "inventorying" | "complete" | null = null;
  let clearingIncomplete = false;
  let refreshingCurrent = false;
  let inventoryProgress: InventoryProgressEvent | null = null;
  let downloads: DownloadDataset[] = [];
  let selectedDownloadId = "";
  let downloadProgress: DownloadProgressEvent | null = null;
  let downloadResult: DownloadResult | null = null;
  let downloadRate = 0;
  let lastDownloadSample: { bytes: number; at: number } | null = null;
  let verificationChecks = 0;
  let autoStartUpload = false;
  let clock = Date.now();
  let activity: ActivityState | null = null;
  let deviceAccess: DeviceAccessStatus | null = null;
  let authorizationLocked = false;
  let unlocking = false;
  let pauseRequested = false;

  onMount(() => {
    autoStartUpload = window.localStorage.getItem("courier.autoStartUpload") === "true";
    void loadTransfers();
    void loadRegistryEndpoint();
    void initializeDeviceAccess();
    let unlistenProgress: UnlistenFn | undefined;
    let unlistenInventory: UnlistenFn | undefined;
    let unlistenDownload: UnlistenFn | undefined;
    const heartbeatTimer = window.setInterval(() => (clock = Date.now()), 1000);
    const statusTimer = window.setInterval(() => {
      if (step === "transfers") void refreshActiveTransfers();
      else void refreshVerificationStatus();
    }, 2000);
    void listen<ProgressEvent>("courier://progress", ({ payload }) => {
      if (!current || payload.transferId !== current.id) return;
      const sampleTime = Date.now();
      if (lastUploadSample && payload.sentBytes >= lastUploadSample.bytes) {
        const elapsedSeconds = (sampleTime - lastUploadSample.at) / 1000;
        if (elapsedSeconds > 0 && payload.sentBytes > lastUploadSample.bytes) {
          const instantaneous = (payload.sentBytes - lastUploadSample.bytes) / elapsedSeconds;
          uploadRate = uploadRate === 0 ? instantaneous : uploadRate * 0.7 + instantaneous * 0.3;
        }
      }
      lastUploadSample = { bytes: payload.sentBytes, at: sampleTime };
      confirmedBytes = payload.confirmedBytes;
      sentBytes = payload.sentBytes;
      currentFile = payload.currentFile;
      current = { ...current, status: payload.status };
      if (payload.status === "uploading") {
        touchActivity("Uploading dataset", payload.currentFile || "Transferring the next confirmed part");
      } else if (payload.status === "finalizing") {
        touchActivity("Upload received", "Waiting for independent Registry verification");
      } else if (payload.status === "paused") {
        finishActivity("Upload paused", "Confirmed parts are safely recorded", "warning");
      } else if (payload.status === "interrupted") {
        finishActivity("Upload interrupted", "Confirmed parts remain available for resume", "warning");
      }
    }).then((dispose) => (unlistenProgress = dispose));
    void listen<InventoryProgressEvent>("courier://inventory-progress", ({ payload }) => {
      if (busy && step === "source") {
        inventoryProgress = payload;
        touchActivity(
          payload.phase === "packaging" ? "Packaging dataset" : "Analyzing dataset",
          payload.currentPath || (payload.phase === "packaging" ? "Creating compressed transport packages" : "Discovering files and computing integrity digests"),
        );
      }
    }).then((dispose) => (unlistenInventory = dispose));
    void listen<DownloadProgressEvent>("courier://download-progress", ({ payload }) => {
      if (payload.transferId === selectedDownloadId) {
        const sampleTime = Date.now();
        if (lastDownloadSample && payload.receivedBytes >= lastDownloadSample.bytes) {
          const elapsedSeconds = (sampleTime - lastDownloadSample.at) / 1000;
          if (elapsedSeconds > 0 && payload.receivedBytes > lastDownloadSample.bytes) {
            const instantaneous = (payload.receivedBytes - lastDownloadSample.bytes) / elapsedSeconds;
            downloadRate = downloadRate === 0 ? instantaneous : downloadRate * 0.7 + instantaneous * 0.3;
          }
        }
        lastDownloadSample = { bytes: payload.receivedBytes, at: sampleTime };
        downloadProgress = payload;
        touchActivity("Retrieving and verifying dataset", payload.currentFile || "Downloading verified transport");
      }
    }).then((dispose) => (unlistenDownload = dispose));
    return () => {
      window.clearInterval(statusTimer);
      window.clearInterval(heartbeatTimer);
      unlistenProgress?.();
      unlistenInventory?.();
      unlistenDownload?.();
    };
  });

  function beginActivity(title: string, detail: string) {
    const timestamp = Date.now();
    activity = { title, detail, startedAt: timestamp, updatedAt: timestamp, active: true, outcome: "working" };
  }

  function touchActivity(title: string, detail: string) {
    const timestamp = Date.now();
    activity = {
      title,
      detail,
      startedAt: activity?.active ? activity.startedAt : timestamp,
      updatedAt: timestamp,
      active: true,
      outcome: "working",
    };
  }

  function finishActivity(title: string, detail: string, outcome: "success" | "warning" = "success") {
    const timestamp = Date.now();
    activity = { title, detail, startedAt: activity?.startedAt ?? timestamp, updatedAt: timestamp, active: false, outcome };
  }

  function duration(seconds: number): string {
    const value = Math.max(0, Math.floor(seconds));
    if (value < 60) return `${value}s`;
    const minutes = Math.floor(value / 60);
    const remainder = value % 60;
    return `${minutes}m ${remainder.toString().padStart(2, "0")}s`;
  }

  function activityAge(): string {
    if (!activity) return "";
    return duration((clock - activity.updatedAt) / 1000);
  }

  function activityElapsed(): string {
    if (!activity) return "";
    return duration((clock - activity.startedAt) / 1000);
  }

  function saveAutoStartPreference() {
    window.localStorage.setItem("courier.autoStartUpload", String(autoStartUpload));
  }

  function goHome() {
    step = "home";
    error = "";
    activity = null;
  }

  async function initializeDeviceAccess() {
    try {
      deviceAccess = await invoke<DeviceAccessStatus>("device_access_status");
      authorizationLocked = deviceAccess.hasStoredAuthorization && deviceAccess.authenticationRequired;
      if (deviceAccess.hasStoredAuthorization && !authorizationLocked) await restoreAuthorization();
    } catch (reason) {
      error = message(reason);
    }
  }

  async function unlockSavedAccess() {
    if (unlocking) return;
    unlocking = true;
    error = "";
    beginActivity(`Waiting for ${deviceAccess?.biometricLabel ?? "device authentication"}`, "Confirm your identity using the system prompt");
    try {
      await invoke("authenticate_device");
      authorizationLocked = false;
      await restoreAuthorization();
      finishActivity("Saved access unlocked", "Courier can resume authorized project work");
    } catch (reason) {
      error = message(reason);
      finishActivity("Saved access remains locked", error, "warning");
    } finally {
      unlocking = false;
    }
  }

  function continueAuthorizedWork() {
    step = authorization?.purpose === "download" ? "downloads" : "source";
  }

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
      touchActivity("Independent verification", "Checking Registry verification status");
      current = await invoke<Transfer>("refresh_transfer_status", { transferId: current.id });
      verificationChecks += 1;
      if (current.status === "complete") {
        finishActivity("Dataset verified", "Every logical file matched the immutable manifest");
      } else if (current.status === "failed") {
        finishActivity("Verification failed", "Open the transfer record for verification evidence", "warning");
      } else {
        touchActivity("Independent verification", `Status check ${verificationChecks} · Registry is reconstructing and checking the uploaded dataset`);
      }
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
    beginActivity("Authorizing Courier", "Connecting securely to the Registry");
    try {
      const authorization = await invoke<RegistryAuthorization>("exchange_invitation", {
        registryUrl: registryUrl.trim(),
        invitationCode: invitation.trim(),
      });
      applyAuthorization(authorization);
      invitation = "";
      step = authorization.purpose === "download" ? "downloads" : "source";
      finishActivity("Courier authorized", `${projects.length} project ${projects.length === 1 ? "scope" : "scopes"} available`);
    } catch (reason) {
      error = message(reason);
      finishActivity("Authorization failed", error, "warning");
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
    downloadRate = 0;
    lastDownloadSample = null;
    error = "";
    busy = true;
    step = "download-progress";
    beginActivity("Preparing dataset retrieval", "Requesting the verified manifest and destination plan");
    try {
      downloadResult = await invoke<DownloadResult>("download_dataset", {
        transferId: dataset.transfer_id,
        destinationDirectory: selected,
      });
      step = "download-complete";
      finishActivity("Dataset retrieved and verified", `${downloadResult.restoredFiles.toLocaleString()} files restored successfully`);
    } catch (reason) {
      error = message(reason);
      step = "downloads";
      finishActivity("Dataset retrieval stopped", error, "warning");
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

  function incompleteTransferCount(): number {
    return transfers.filter((transfer) => transfer.status !== "complete").length;
  }

  async function clearIncompleteTransfers() {
    const count = incompleteTransferCount();
    if (count === 0 || clearingIncomplete) return;
    const approved = await confirm(
      `Purge ${count} incomplete transfer${count === 1 ? "" : "s"} from this Mac? Local records and temporary pack caches will be removed; original datasets and Registry uploads will not be deleted.`,
      { title: "Purge incomplete transfers", kind: "warning" },
    );
    if (!approved) return;
    error = "";
    clearingIncomplete = true;
    try {
      await invoke<number>("clear_incomplete_transfers");
      current = null;
      await loadTransfers();
    } catch (reason) {
      error = message(reason);
    } finally {
      clearingIncomplete = false;
    }
  }

  async function restartTransfer(transfer: Transfer) {
    if (busy || transfer.status === "complete") return;
    const approved = await confirm(
      `Restart analysis for ${sourceName(transfer.source_root)}? The local transfer record and any cached transport packages will be removed; the source data will remain untouched.`,
      { title: "Restart transfer", kind: "warning" },
    );
    if (!approved) return;
    error = "";
    try {
      await invoke<boolean>("clear_transfer", { transferId: transfer.id });
      current = null;
      sourcePath = transfer.source_root;
      projectId = transfer.project_id ?? projectId;
      step = authorization?.purpose === "upload" ? "source" : "invite";
      await loadTransfers();
    } catch (reason) {
      error = message(reason);
    }
  }

  async function clearOneTransfer(transfer: Transfer) {
    if (busy || transfer.status === "complete") return;
    const approved = await confirm(
      `Clear ${sourceName(transfer.source_root)} from this Mac? Local records and cached transport packages will be removed; the source data and Registry upload will remain untouched.`,
      { title: "Clear transfer", kind: "warning" },
    );
    if (!approved) return;
    error = "";
    try {
      await invoke<boolean>("clear_transfer", { transferId: transfer.id });
      if (current?.id === transfer.id) current = null;
      await loadTransfers();
    } catch (reason) {
      error = message(reason);
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
    beginActivity("Analyzing dataset", "Discovering files and computing integrity digests");
    try {
      current = await invoke<Transfer>("create_inventory", {
        sourcePath,
        projectId: projectId.trim() || null,
        hashAlgorithm: authorization?.hashAlgorithm ?? "sha256",
      });
      await loadTransferSizes(current.id);
      await loadTransfers();
      if (autoStartUpload) {
        touchActivity("Analysis complete", "Automatic upload is starting");
        await startUpload();
      } else {
        finishActivity("Analysis complete", `${current.file_count.toLocaleString()} files are ready for review`);
        step = "review";
      }
    } catch (reason) {
      error = message(reason);
      finishActivity("Analysis stopped", error, "warning");
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
    activity = null;
    step = "source";
  }

  function openTransfer(transfer: Transfer) {
    current = transfer;
    confirmedBytes = transfer.status === "finalizing" || transfer.status === "verifying" || transfer.status === "complete" ? transfer.original_bytes : 0;
    sentBytes = confirmedBytes;
    uploadRate = 0;
    lastUploadSample = null;
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
    if (authorizationLocked) {
      error = `Unlock saved access with ${deviceAccess?.biometricLabel ?? "device authentication"} before resuming this transfer.`;
      step = "home";
      return;
    }
    step = "progress";
    current = { ...current, status: "uploading" };
    error = "";
    uploadCommandRunning = true;
    pauseRequested = false;
    sentBytes = confirmedBytes;
    uploadRate = 0;
    lastUploadSample = null;
    verificationChecks = 0;
    beginActivity("Preparing secure upload", "Registering the dataset and reconciling any confirmed parts");
    try {
      current = await invoke<Transfer>("start_upload", { transferId: current.id });
      if (current.status === "finalizing" || current.status === "verifying") {
        touchActivity("Upload received", "Registry verification is starting; status is checked every two seconds");
      } else if (current.status === "complete") {
        finishActivity("Dataset verified", "Every logical file matched the immutable manifest");
      }
      await loadTransfers();
    } catch (reason) {
      error = message(reason);
      finishActivity("Upload stopped", error, "warning");
      await loadTransfers();
    } finally {
      uploadCommandRunning = false;
      pauseRequested = false;
    }
  }

  async function pauseUpload() {
    if (!current || pauseRequested) return;
    const previousStatus = current.status;
    error = "";
    pauseRequested = true;
    current = { ...current, status: "paused" };
    sentBytes = confirmedBytes;
    finishActivity("Pausing upload", "Stopping the active request; confirmed parts remain resumable", "warning");
    try {
      await invoke("pause_upload", { transferId: current.id });
    } catch (reason) {
      error = message(reason);
      pauseRequested = false;
      current = { ...current, status: previousStatus };
    }
  }

  function progressPercent(): number {
    if (!current || current.original_bytes === 0) return current && ["finalizing", "verifying", "complete"].includes(current.status) ? 100 : 0;
    return Math.min(100, (sentBytes / current.original_bytes) * 100);
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
    <button class="brand" aria-label="Courier home" onclick={goHome}>
      <span class="mark" aria-hidden="true"><span></span></span>
      <span><strong>Icy Seas</strong><small>Courier</small></span>
    </button>
    {#if authorization?.purpose !== "download"}<button class="text-button" onclick={() => { step = "transfers"; loadTransfers(); }}>
      Transfers <span class="count">{transfers.length}</span>
    </button>{/if}
  </header>

  <main>
    {#if step !== "home" && step !== "transfers" && step !== "invite"}
      <nav class="steps" aria-label="Transfer setup progress">
        <span class="done">1 <em>Invitation</em></span>
        <i></i>
        <span class:active={step === "source" || step === "downloads"} class:done={step === "review" || step === "download-progress" || step === "download-complete"}>2 <em>{authorization?.purpose === "download" ? "Dataset" : "Source"}</em></span>
        <i></i>
        <span class:active={step === "review" || step === "progress" || step === "download-progress" || step === "download-complete"}>3 <em>{authorization?.purpose === "download" ? "Retrieve" : step === "progress" ? "Transfer" : "Review"}</em></span>
      </nav>
    {/if}

    {#if error}<div class="notice error dismissible" role="alert"><span>{error}</span><button aria-label="Dismiss error" onclick={() => (error = "")}>×</button></div>{/if}

    {#if activity}
      <aside class:working={activity.active} class:success={activity.outcome === "success"} class:warning={activity.outcome === "warning"} class="activity-monitor" aria-live="polite">
        <span class="activity-indicator" aria-hidden="true">{activity.active ? "" : activity.outcome === "success" ? "✓" : "!"}</span>
        <div class="activity-copy"><strong>{activity.title}</strong><span>{activity.detail}</span></div>
        <div class="activity-time">
          {#if activity.active}<strong>{activityAge() === "0s" ? "Working now" : `Still working · update ${activityAge()} ago`}</strong><span>Active for {activityElapsed()}</span>{:else}<strong>{activity.outcome === "success" ? "Complete" : "Attention needed"}</strong><span>{formatTimestamp(new Date(activity.updatedAt).toISOString())}</span>{/if}
        </div>
      </aside>
    {/if}

    {#if step === "home"}
      <section class="home">
        <div class="home-hero">
          <div>
            <p class="eyebrow">Secure project delivery</p>
            <h1>Your Courier workspace</h1>
            <p class="lede">Send, resume, and retrieve project datasets with visible progress and verified transfer history.</p>
          </div>
          {#if authorizationLocked}
            <div class="access-card locked">
              <span class="access-icon" aria-hidden="true">◎</span>
              <div><strong>Saved project access is locked</strong><small>Use {deviceAccess?.biometricLabel ?? "device authentication"} instead of entering your Mac password.</small></div>
              <button class="primary" disabled={unlocking} onclick={unlockSavedAccess}>{unlocking ? "Waiting…" : `Unlock with ${deviceAccess?.biometricLabel ?? "Touch ID"}`}</button>
            </div>
          {:else if authorization}
            <div class="access-card">
              <span class="access-dot" aria-hidden="true"></span>
              <div><strong>{authorization.purpose === "download" ? "Project retrieval ready" : "Project upload ready"}</strong><small>{projects.map((project) => `${project.project_code} · ${project.name}`).join("; ")}</small></div>
              <button class="primary" onclick={continueAuthorizedWork}>{authorization.purpose === "download" ? "Browse datasets" : "Add dataset"}</button>
            </div>
          {:else}
            <div class="access-card new-user">
              <span class="access-icon" aria-hidden="true">→</span>
              <div><strong>Connect this device</strong><small>Use an invitation from Icy Seas to access the correct project and delivery direction.</small></div>
              <button class="primary" onclick={() => (step = "invite")}>Enter invitation</button>
            </div>
          {/if}
        </div>

        <div class="home-grid">
          <article class="home-card">
            <p class="eyebrow">Continue work</p>
            <div class="card-heading"><h2>Recent transfers</h2><button class="text-button" onclick={() => { step = "transfers"; loadTransfers(); }}>View all <span class="count">{transfers.length}</span></button></div>
            {#if transfers.length === 0}
              <div class="home-empty"><strong>No transfer history yet</strong><span>Your analyzed, paused, and completed datasets will appear here.</span></div>
            {:else}
              <div class="transfer-list compact-list">
                {#each transfers.slice(0, 4) as transfer}
                  <button class="transfer-row" onclick={() => openTransfer(transfer)}>
                    <div><strong>{sourceName(transfer.source_root)}</strong><small>{transfer.project_id ?? "Project pending"} · {formatBytes(transfer.original_bytes)} · {formatTimestamp(transfer.updated_at)}</small></div>
                    <span class:verified={transfer.status === "complete"} class="status">{statusLabel(transfer.status)}</span>
                  </button>
                {/each}
              </div>
            {/if}
          </article>

          <aside class="home-card quick-start">
            <p class="eyebrow">How Courier works</p>
            <h2>Three clear stages</h2>
            <ol>
              <li><span>1</span><div><strong>Connect</strong><small>An invitation limits access to the intended project.</small></div></li>
              <li><span>2</span><div><strong>Choose</strong><small>Add a source dataset or select a verified dataset to retrieve.</small></div></li>
              <li><span>3</span><div><strong>Transfer</strong><small>Follow live progress; pause and resume without restarting.</small></div></li>
            </ol>
            {#if authorization}<button class="secondary" onclick={() => (step = "invite")}>Use another invitation</button>{/if}
          </aside>
        </div>
      </section>
    {:else if step === "invite"}
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
        <div class="actions"><button class="secondary" onclick={goHome}>Back home</button><button class="secondary" onclick={() => (step = "invite")}>Use another invitation</button></div>
      </section>
    {:else if step === "download-progress"}
      <section class="panel">
        <p class="eyebrow centered">Secure project retrieval</p>
        <h1 class="centered">{downloads.find((item) => item.transfer_id === selectedDownloadId)?.source_name ?? selectedDownloadId}</h1>
        <div class="progress-number">{downloadPercent().toFixed(0)}%</div>
        <div class="progress-track" role="progressbar" aria-label="Download progress" aria-valuenow={downloadPercent()} aria-valuemin="0" aria-valuemax="100"><span style={`width: ${downloadPercent()}%`}></span></div>
        <div class="progress-details"><strong>{downloadProgress ? `${formatBytes(downloadProgress.receivedBytes)}${downloadProgress.totalBytes ? ` / ${formatBytes(downloadProgress.totalBytes)}` : ""}` : "Preparing secure download…"}</strong><span>{downloadRate > 0 ? `${formatBytes(downloadRate)}/s · ` : ""}{downloadProgress ? `${downloadProgress.restoredFiles.toLocaleString()} / ${downloadProgress.totalFiles.toLocaleString()} files restored` : "Requesting short-lived access"}</span></div>
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
              <strong>{inventoryProgress?.phase === "packaging" ? "Creating compressed transfer packages…" : inventoryProgress ? `Analyzed ${inventoryProgress.filesAnalyzed.toLocaleString()} of ${inventoryProgress.totalFiles.toLocaleString()} files…` : "Discovering files…"}</strong>
              {#if inventoryProgress}<span>{formatBytes(inventoryProgress.bytesAnalyzed)} / {formatBytes(inventoryProgress.totalBytes)}</span>{/if}
            </div>
            <div class="progress-track" role="progressbar" aria-label="Dataset analysis progress" aria-valuenow={inventoryPercent()} aria-valuemin="0" aria-valuemax="100"><span style={`width: ${inventoryPercent()}%`}></span></div>
            {#if inventoryProgress?.currentPath}<code>{inventoryProgress.currentPath}</code>{/if}
          </div>
        {/if}
        <label class="preference-toggle"><input type="checkbox" bind:checked={autoStartUpload} onchange={saveAutoStartPreference} disabled={busy}><span><strong>Start upload automatically after analysis</strong><small>Skip the review pause when inventory, hashing, and packaging finish successfully.</small></span></label>
        <div class="actions"><button class="secondary" onclick={goHome}>Back home</button><button class="primary" disabled={busy} onclick={inventory}>{busy ? (inventoryProgress?.phase === "packaging" ? "Packaging…" : "Analyzing…") : autoStartUpload ? "Analyze and upload" : "Review dataset"}</button></div>
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
        <div class="progress-details"><strong>{formatBytes(sentBytes)} / {formatBytes(current.original_bytes)}</strong><span>{uploadRate > 0 && current.status === "uploading" ? `${formatBytes(uploadRate)}/s` : statusLabel(current.status)}</span></div>
        {#if current.status === "uploading"}<div class="transfer-telemetry"><span><strong>{formatBytes(confirmedBytes)}</strong><small>Durably confirmed</small></span><span><strong>{formatBytes(Math.max(0, sentBytes - confirmedBytes))}</strong><small>Current request</small></span><span><strong>{activityAge()}</strong><small>Since last byte update</small></span></div>{/if}
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
          <div class="actions"><button class="primary" disabled={uploadCommandRunning} onclick={startUpload}>{pauseRequested ? "Pausing…" : uploadCommandRunning ? "Finishing pause…" : "Resume"}</button></div>
        {:else}
          <div class="connection"><i></i><span>Connection active</span><small>Completed parts are saved continuously</small></div>
          <div class="actions"><button class="secondary" disabled={!uploadCommandRunning || pauseRequested} onclick={pauseUpload}>{pauseRequested ? "Pausing…" : "Pause now"}</button></div>
        {/if}
      </section>
    {:else if step === "transfers"}
      <section class="panel wide">
        <div class="section-heading"><div><p class="eyebrow">Local state</p><h1>Transfers</h1></div><div class="heading-actions"><button class="secondary small" onclick={goHome}>Home</button><button class="secondary small" disabled={refreshingTransfers} onclick={refreshActiveTransfers}>{refreshingTransfers ? "Refreshing…" : "Refresh status"}</button><button class="primary small" onclick={() => (step = authorization ? (authorization.purpose === "download" ? "downloads" : "source") : "invite")}>{authorization?.purpose === "download" ? "Browse datasets" : "New transfer"}</button></div></div>
        {#if incompleteTransferCount() > 0 || transferCount("inventorying") > 0 || transferCount("complete") > 0}
          <div class="cleanup-bar">
            <span>Manage local records</span>
            <div>
              {#if incompleteTransferCount() > 0}<button class="secondary small danger" disabled={busy || clearingIncomplete || clearingTransfers !== null} onclick={clearIncompleteTransfers}>{clearingIncomplete ? "Purging…" : `Purge incomplete (${incompleteTransferCount()})`}</button>{/if}
              {#if transferCount("inventorying") > 0}<button class="secondary small" disabled={busy || clearingTransfers !== null} onclick={() => clearTransfers("inventorying")}>{clearingTransfers === "inventorying" ? "Clearing…" : `Clear inventorying (${transferCount("inventorying")})`}</button>{/if}
              {#if transferCount("complete") > 0}<button class="secondary small" disabled={clearingTransfers !== null} onclick={() => clearTransfers("complete")}>{clearingTransfers === "complete" ? "Clearing…" : `Clear completed (${transferCount("complete")})`}</button>{/if}
            </div>
          </div>
        {/if}
        {#if transfers.length === 0}<div class="empty"><div class="state-icon">↑</div><h2>No transfers yet</h2><p>Start with an invitation and choose a dataset file or folder.</p></div>{:else}
          <div class="transfer-list">
            {#each transfers as transfer}
              <div class="transfer-entry">
                <button class="transfer-row" onclick={() => openTransfer(transfer)}>
                  <div><strong>{sourceName(transfer.source_root)}</strong><small>{transfer.project_id ?? "Project pending"} · {transfer.file_count.toLocaleString()} files · {formatBytes(transfer.original_bytes)}</small><small>Updated {formatTimestamp(transfer.updated_at)}{transfer.server_transfer_id ? ` · ${transfer.server_transfer_id}` : ""}</small></div>
                  <span class:verified={transfer.status === "complete"} class="status">{statusLabel(transfer.status)}</span>
                </button>
                {#if transfer.status !== "complete"}<div class="transfer-actions">{#if authorization?.purpose === "upload"}<button class="secondary small" disabled={busy} onclick={() => restartTransfer(transfer)}>Restart analysis</button>{/if}<button class="secondary small danger" disabled={busy || clearingIncomplete} onclick={() => clearOneTransfer(transfer)}>Clear</button></div>{/if}
              </div>
            {/each}
          </div>
        {/if}
      </section>
    {/if}
  </main>

  <footer><span><i></i> Local state available</span><span>Uploads are verified independently by Icy Seas</span></footer>
</div>
