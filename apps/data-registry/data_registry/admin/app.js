const state = { key: "", overview: null, projects: [], invitations: [], transfers: [], audit: [] };
const $ = (selector) => document.querySelector(selector);
const $$ = (selector) => [...document.querySelectorAll(selector)];
const escapeHtml = (value) => String(value ?? "").replace(/[&<>'"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[character]);
const bytes = (value) => {
  if (!value) return "0 bytes";
  const units = ["bytes", "KB", "MB", "GB", "TB", "PB"];
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1000)), units.length - 1);
  return `${(value / 1000 ** index).toLocaleString(undefined, { maximumFractionDigits: 1 })} ${units[index]}`;
};
const when = (value) => value ? new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(value)) : "—";
const badge = (status) => `<span class="status ${escapeHtml(status)}">${escapeHtml(status)}</span>`;

async function api(path, options = {}) {
  const response = await fetch(`/api/v1/admin${path}`, {
    ...options,
    headers: { "Content-Type": "application/json", "X-Admin-Key": state.key, ...(options.headers || {}) },
  });
  if (response.status === 204) return null;
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(typeof payload.detail === "string" ? payload.detail : JSON.stringify(payload.detail || payload));
  return payload;
}

async function loadAll() {
  setError("");
  const [overview, projects, invitations, transfers, audit] = await Promise.all([
    api("/overview"), api("/projects"), api("/invitations"), api("/transfers?limit=250"), api("/audit-events?limit=250"),
  ]);
  Object.assign(state, { overview, projects, invitations, transfers, audit });
  renderAll();
  $("#last-refresh").textContent = `Updated ${new Date().toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`;
}

function setError(message, login = false) {
  const element = login ? $("#login-error") : $("#global-error");
  element.textContent = message;
  element.hidden = !message;
}

function toast(message) {
  const element = $("#toast");
  element.textContent = message;
  element.classList.add("show");
  window.setTimeout(() => element.classList.remove("show"), 2600);
}

function renderAll() {
  const metric = (label, value, detail = "") => `<div class="metric"><span>${label}</span><strong>${value}</strong><small>${detail}</small></div>`;
  $("#metrics").innerHTML = [
    metric("Incoming data", bytes(state.overview.original_bytes), `${state.overview.total_transfers} total transfers`),
    metric("Active", state.overview.active_transfers, "Uploading or verifying"),
    metric("Verified", state.overview.completed_transfers, "Integrity checks complete"),
    metric("Needs attention", state.overview.failed_transfers, `${state.overview.active_invitations} active invitations`),
    metric("Digest policy", state.overview.hash_algorithm.toUpperCase(), "Applied to new transfers"),
  ].join("");
  $("#recent-transfers").innerHTML = transferTable(state.transfers.slice(0, 6), false);
  $("#recent-audit").innerHTML = auditList(state.audit.slice(0, 8));
  renderTransfers();
  renderInvitations();
  renderProjects();
  $("#audit-table").innerHTML = auditTable(state.audit);
}

function transferTable(items, full = true) {
  if (!items.length) return `<div class="empty">No transfers match this view.</div>`;
  return `<table><thead><tr><th>Dataset</th><th>Project</th><th>Status</th>${full ? "<th>Size</th><th>Updated</th>" : ""}</tr></thead><tbody>${items.map((item) => `<tr class="clickable" data-transfer="${escapeHtml(item.transfer_id)}"><td><strong>${escapeHtml(item.source_name)}</strong><span class="sub">${escapeHtml(item.transfer_id)} · ${item.file_count.toLocaleString()} files</span></td><td>${escapeHtml(item.project_code)}</td><td>${badge(item.status)}</td>${full ? `<td>${bytes(item.original_bytes)}</td><td>${when(item.verified_at || item.completed_at || item.created_at)}</td>` : ""}</tr>`).join("")}</tbody></table>`;
}

function renderTransfers() {
  const query = $("#transfer-search").value.trim().toLowerCase();
  const status = $("#transfer-filter").value;
  const items = state.transfers.filter((item) => (!status || item.status === status) && (!query || [item.transfer_id, item.source_name, item.project_code].some((value) => value.toLowerCase().includes(query))));
  $("#transfer-table").innerHTML = transferTable(items);
}

function invitationStatus(item) {
  if (item.revoked_at) return "revoked";
  if (new Date(item.expires_at) <= new Date()) return "expired";
  if (item.maximum_uses !== null && item.use_count >= item.maximum_uses) return "used";
  return "active";
}

function renderInvitations() {
  if (!state.invitations.length) { $("#invitation-table").innerHTML = `<div class="empty">No invitations have been issued.</div>`; return; }
  $("#invitation-table").innerHTML = `<table><thead><tr><th>Projects</th><th>Issued by</th><th>Uses</th><th>Expires</th><th>Status</th><th></th></tr></thead><tbody>${state.invitations.map((item) => { const status = invitationStatus(item); return `<tr><td><strong>${item.project_codes.map(escapeHtml).join(", ")}</strong><span class="sub">${escapeHtml(item.id)}</span></td><td>${escapeHtml(item.created_by)}</td><td>${item.use_count} / ${item.maximum_uses ?? "unlimited"}</td><td>${when(item.expires_at)}</td><td>${badge(status)}</td><td>${status === "active" ? `<button class="secondary danger" data-revoke="${escapeHtml(item.id)}">Revoke</button>` : ""}</td></tr>`; }).join("")}</tbody></table>`;
}

function renderProjects() {
  $("#project-grid").innerHTML = state.projects.length ? state.projects.map((item) => `<article class="card project"><code>${escapeHtml(item.project_code)}</code><h3>${escapeHtml(item.name)}</h3><p>${escapeHtml(item.description || "No description provided.")}</p>${badge(item.status)}<span class="sub">Created ${when(item.created_at)}</span></article>`).join("") : `<div class="empty">No projects yet.</div>`;
}

function auditList(items) {
  return items.length ? items.map((item) => `<div class="evidence"><strong>${escapeHtml(item.action.replaceAll(".", " · "))}</strong><span class="sub">${escapeHtml(item.actor)} · ${when(item.timestamp)}</span></div>`).join("") : `<div class="empty">No activity recorded.</div>`;
}

function auditTable(items) {
  return `<table><thead><tr><th>Time</th><th>Action</th><th>Actor</th><th>Object</th></tr></thead><tbody>${items.map((item) => `<tr><td>${when(item.timestamp)}</td><td><strong>${escapeHtml(item.action)}</strong></td><td>${escapeHtml(item.actor)}</td><td>${escapeHtml(item.object_type)}<span class="sub">${escapeHtml(item.object_id)}</span></td></tr>`).join("")}</tbody></table>`;
}

function showView(name) {
  $$(".view").forEach((item) => item.classList.toggle("active", item.id === `view-${name}`));
  $$('nav button[data-view]').forEach((item) => item.classList.toggle("active", item.dataset.view === name));
  $("#view-title").textContent = ({ overview: "Overview", transfers: "Transfers", invitations: "Invitations", projects: "Projects", audit: "Audit log" })[name];
}

async function showTransfer(id) {
  try {
    const item = await api(`/transfers/${encodeURIComponent(id)}`);
    $("#detail-title").textContent = item.source_name;
    const retry = item.status === "failed" ? `<button class="secondary danger" data-retry="${escapeHtml(item.transfer_id)}">Retry verification</button>` : "";
    $("#detail-body").innerHTML = `<div class="detail-grid"><div><span>Registry ID</span><strong>${escapeHtml(item.transfer_id)}</strong></div><div><span>Project</span><strong>${escapeHtml(item.project_code)}</strong></div><div><span>Status</span><strong>${badge(item.status)}</strong></div><div><span>Digest policy</span><strong>${escapeHtml(item.hash_algorithm.toUpperCase())}</strong></div><div><span>Original data</span><strong>${bytes(item.original_bytes)}</strong></div><div><span>Files</span><strong>${item.file_count.toLocaleString()}</strong></div><div><span>Attempts</span><strong>${item.verification_attempt_count}</strong></div><div><span>Verified</span><strong>${when(item.verified_at)}</strong></div></div>${item.verification_error ? `<div class="error">${escapeHtml(item.verification_error)}</div>` : ""}<div class="evidence"><span>Canonical manifest SHA-256</span><code>${escapeHtml(item.manifest_sha256 || "Not submitted")}</code></div><h3>File verification evidence</h3><div class="table-card">${fileTable(item.files)}</div><div class="dialog-actions">${retry}</div>`;
    $("#detail-dialog").showModal();
  } catch (error) { setError(error.message); }
}

function fileTable(files) {
  if (!files.length) return `<div class="empty">Manifest files have not been registered.</div>`;
  return `<table><thead><tr><th>File</th><th>Size</th><th>Status</th><th>Digest evidence</th></tr></thead><tbody>${files.map((file) => `<tr><td><strong>${escapeHtml(file.relative_path)}</strong>${file.verification_error ? `<span class="sub">${escapeHtml(file.verification_error)}</span>` : ""}</td><td>${bytes(file.original_size)}</td><td>${badge(file.status)}</td><td><code>${escapeHtml(file.verified_sha256 || file.original_sha256)}</code><span class="sub">${escapeHtml(file.hash_algorithm.toUpperCase())} · ${file.verified_at ? `matched ${when(file.verified_at)}` : "expected manifest digest"}</span></td></tr>`).join("")}</tbody></table>`;
}

function openProjectForm() {
  $("#dialog-eyebrow").textContent = "Organization"; $("#dialog-title").textContent = "Create project";
  $("#dialog-body").innerHTML = `<div class="fields"><div class="field"><label for="project-code">Project code</label><input id="project-code" required pattern="P[0-9]{5}" placeholder="P26014"><small>Stable identifier: P followed by five digits.</small></div><div class="field"><label for="project-name">Name</label><input id="project-name" required maxlength="300"></div><div class="field"><label for="project-description">Description</label><textarea id="project-description"></textarea></div></div>`;
  $("#form-dialog").dataset.action = "project"; $("#form-dialog").showModal();
}

function openInvitationForm() {
  if (!state.projects.length) { setError("Create an active project before issuing an invitation."); return; }
  const defaultExpiry = new Date(Date.now() + 24 * 60 * 60 * 1000); defaultExpiry.setMinutes(defaultExpiry.getMinutes() - defaultExpiry.getTimezoneOffset());
  $("#dialog-eyebrow").textContent = "Secure access"; $("#dialog-title").textContent = "Issue upload invitation";
  $("#dialog-body").innerHTML = `<div class="fields"><div class="field"><label>Authorized projects</label><div class="checks">${state.projects.filter((item) => item.status === "active").map((item) => `<label class="check"><input type="checkbox" name="invite-project" value="${escapeHtml(item.project_code)}"> ${escapeHtml(item.project_code)} · ${escapeHtml(item.name)}</label>`).join("")}</div></div><div class="field"><label for="invite-expiry">Expires</label><input id="invite-expiry" type="datetime-local" value="${defaultExpiry.toISOString().slice(0, 16)}" required></div><div class="field"><label for="invite-uses">Maximum uses</label><input id="invite-uses" type="number" value="1" min="1" required></div><div class="field"><label for="invite-size">Maximum transfer size in GB (optional)</label><input id="invite-size" type="number" min="0" step="0.1"></div><div class="field"><label for="invite-by">Issued by</label><input id="invite-by" type="email" value="registry-admin@icyseas.co" required></div></div>`;
  $("#form-dialog").dataset.action = "invitation"; $("#form-dialog").showModal();
}

async function submitDialog(event) {
  if (event.submitter?.value === "cancel") return;
  event.preventDefault();
  try {
    if ($("#form-dialog").dataset.action === "project") {
      await api("/projects", { method: "POST", body: JSON.stringify({ project_code: $("#project-code").value.trim(), name: $("#project-name").value.trim(), description: $("#project-description").value.trim() || null }) });
      toast("Project created");
    } else {
      const projectCodes = $$('input[name="invite-project"]:checked').map((item) => item.value);
      if (!projectCodes.length) throw new Error("Select at least one project.");
      const size = $("#invite-size").value;
      const invitation = await api("/invitations", { method: "POST", body: JSON.stringify({ project_codes: projectCodes, expires_at: new Date($("#invite-expiry").value).toISOString(), maximum_uses: Number($("#invite-uses").value), maximum_transfer_bytes: size ? Math.round(Number(size) * 1_000_000_000) : null, created_by: $("#invite-by").value.trim() }) });
      window.prompt("Copy this invitation now. For security, it cannot be shown again.", invitation.invitation_code);
      toast("Invitation issued");
    }
    $("#form-dialog").close(); await loadAll();
  } catch (error) { setError(error.message); }
}

$("#login-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const submit = $("#login-submit");
  submit.disabled = true;
  submit.textContent = "Opening…";
  setError("", true);
  state.key = $("#admin-key").value.trim();
  try {
    await loadAll();
    $("#login").hidden = true;
    $("#console").hidden = false;
  } catch (error) {
    state.key = "";
    setError(error.message, true);
  } finally {
    submit.disabled = false;
    submit.textContent = "Open console";
  }
});
$("#logout").addEventListener("click", () => { state.key = ""; $("#admin-key").value = ""; $("#console").hidden = true; $("#login").hidden = false; });
$("#refresh").addEventListener("click", () => loadAll().catch((error) => setError(error.message)));
$("#transfer-search").addEventListener("input", renderTransfers); $("#transfer-filter").addEventListener("change", renderTransfers);
$("#new-project").addEventListener("click", openProjectForm); $("#new-invitation").addEventListener("click", openInvitationForm); $("#form-dialog form").addEventListener("submit", submitDialog);
$$('nav button[data-view]').forEach((item) => item.addEventListener("click", () => showView(item.dataset.view)));
$$('[data-go]').forEach((item) => item.addEventListener("click", () => showView(item.dataset.go)));
document.addEventListener("click", async (event) => {
  const transfer = event.target.closest("[data-transfer]"); if (transfer) return showTransfer(transfer.dataset.transfer);
  const revoke = event.target.closest("[data-revoke]"); if (revoke && window.confirm("Revoke this invitation? Existing sessions will no longer be accepted.")) { try { await api(`/invitations/${revoke.dataset.revoke}`, { method: "DELETE" }); toast("Invitation revoked"); await loadAll(); } catch (error) { setError(error.message); } }
  const retry = event.target.closest("[data-retry]"); if (retry && window.confirm("Queue this transfer for another independent verification attempt?")) { try { await api(`/transfers/${encodeURIComponent(retry.dataset.retry)}/retry`, { method: "POST" }); $("#detail-dialog").close(); toast("Verification retry queued"); await loadAll(); } catch (error) { setError(error.message); } }
});
window.setInterval(() => { if (state.key && !document.hidden) loadAll().catch((error) => setError(error.message)); }, 15000);
