<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";
  import { formatBytes, formatTimestamp, sourceName, statusLabel } from "./lib/format";
  import type { RegistryAuthorization, RegistryProject, Transfer } from "./lib/types";

  type Step = "invite" | "source" | "review" | "progress" | "transfers";
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

  let step: Step = "invite";
  let registryUrl = "http://127.0.0.1:8010";
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
  let uploadCommandRunning = false;
  let authorization: RegistryAuthorization | null = null;
  let refreshingTransfers = false;
  let inventoryProgress: InventoryProgressEvent | null = null;

  onMount(() => {
    void loadTransfers();
    void loadRegistryEndpoint();
    void restoreAuthorization();
    let unlistenProgress: UnlistenFn | undefined;
    let unlistenInventory: UnlistenFn | undefined;
    const statusTimer = window.setInterval(() => {
      void refreshVerificationStatus();
      if (step === "transfers") void refreshActiveTransfers();
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
    return () => {
      window.clearInterval(statusTimer);
      unlistenProgress?.();
      unlistenInventory?.();
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
    if (!current || (current.status !== "finalizing" && current.status !== "verifying")) return;
    try {
      current = await invoke<Transfer>("refresh_transfer_status", { transferId: current.id });
      await loadTransfers();
    } catch (reason) {
      error = message(reason);
    }
  }

  async function restoreAuthorization() {
    try {
      const authorization = await invoke<RegistryAuthorization | null>("current_authorization");
      if (authorization) {
        applyAuthorization(authorization);
        step = "source";
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
      error = "Enter the upload invitation supplied by Icy Seas.";
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
      step = "source";
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
    if (!projects.some((project) => project.project_code === projectId)) {
      projectId = projects[0]?.project_code ?? "";
    }
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
    error = "";
    step = "source";
  }

  function openTransfer(transfer: Transfer) {
    current = transfer;
    confirmedBytes = transfer.status === "finalizing" || transfer.status === "verifying" || transfer.status === "complete" ? transfer.original_bytes : 0;
    currentFile = "";
    error = "";
    step = transfer.status === "ready" ? "review" : "progress";
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
    <button class="text-button" onclick={() => { step = "transfers"; loadTransfers(); }}>
      Transfers <span class="count">{transfers.length}</span>
    </button>
  </header>

  <main>
    {#if step !== "transfers"}
      <nav class="steps" aria-label="Transfer setup progress">
        <span class:active={step === "invite"} class:done={step !== "invite"}>1 <em>Invitation</em></span>
        <i></i>
        <span class:active={step === "source"} class:done={step === "review"}>2 <em>Source</em></span>
        <i></i>
        <span class:active={step === "review" || step === "progress"}>3 <em>{step === "progress" ? "Transfer" : "Review"}</em></span>
      </nav>
    {/if}

    {#if error}<div class="notice error dismissible" role="alert"><span>{error}</span><button aria-label="Dismiss error" onclick={() => (error = "")}>×</button></div>{/if}

    {#if step === "invite"}
      <section class="panel compact">
        <p class="eyebrow">Secure data delivery</p>
        <h1>Send a dataset to Icy Seas</h1>
        <p class="lede">Courier keeps a durable local record so large scientific transfers can recover from interrupted connections.</p>
        <label for="registry-url">Registry address</label>
        <input id="registry-url" type="url" bind:value={registryUrl} placeholder="https://courier.example.org" autocomplete="url" autocapitalize="none" spellcheck="false" />
        <p class="hint">Use the HTTPS address supplied with your beta invitation. Local development may use localhost.</p>
        <label for="invitation">Upload invitation</label>
        <input id="invitation" bind:value={invitation} placeholder="ISC-A7F4-KQ92-XT81" autocomplete="off" onkeydown={(event) => event.key === "Enter" && void acceptInvitation()} />
        <p class="hint">The invitation authorizes a new upload. It does not provide access to other project data.</p>
        <button class="primary" disabled={busy} onclick={acceptInvitation}>{busy ? "Authorizing…" : "Continue"}</button>
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
