// koi tray popover logic.
//
// SECURITY: proposal fields (path, dest, rationale) are derived from real
// filenames on disk, which are attacker-influenceable (e.g. a downloaded file
// named `<img src=x onerror=...>.pdf`). Every value interpolated into innerHTML
// MUST go through esc(); the webview holds __TAURI__.invoke, so an injection
// here would reach the IPC layer. A strict CSP in tauri.conf.json is the second
// layer of defence.

const invoke = window.__TAURI__.core.invoke;
const $ = (id) => document.getElementById(id);

const esc = (s) =>
  String(s).replace(/[&<>"'`]/g, (c) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
    "`": "&#96;",
  }[c]));

function statusClass(s) {
  s = (s || "").toLowerCase();
  return ["healthy", "warning", "critical"].includes(s) ? s : "unknown";
}

function shortPath(p) {
  const parts = String(p).split(/[\\/]/);
  return parts.length > 3 ? "…/" + parts.slice(-2).join("/") : p;
}

function renderMonitors(monitors) {
  const el = $("monitors");
  if (!monitors || monitors.length === 0) {
    el.innerHTML =
      '<div class="empty">No reports yet — run <b>koi check</b> or start the daemon.</div>';
    return;
  }
  el.innerHTML = monitors
    .map((m) => {
      const cls = statusClass(m.status); // whitelisted enum → safe as class
      const name = esc(m.monitor.replace(/Monitor$/, ""));
      return `<div class="row">
          <span class="dot ${cls}"></span>
          <span class="name">${name}</span>
          <span class="meta">${esc(m.elapsed_ms)}ms</span>
        </div>`;
    })
    .join("");
}

function renderProposals(proposals) {
  const el = $("proposals");
  $("proposal-count").textContent = proposals ? proposals.length : 0;
  if (!proposals || proposals.length === 0) {
    el.innerHTML = '<div class="empty">Nothing awaiting consent.</div>';
    return;
  }
  el.innerHTML = proposals
    .map((p) => {
      const dest = p.dest
        ? `<span class="arrow">→</span> ${esc(shortPath(p.dest))}`
        : esc(p.action_kind);
      const conf = Math.round((p.confidence || 0) * 100);
      const monitor = esc(p.monitor.replace(/Monitor$/, ""));
      return `<div class="proposal" data-id="${esc(p.id)}">
          <div class="path">${esc(shortPath(p.path))} ${dest}</div>
          <div class="rationale">${esc(p.rationale)} · ${conf}% · ${monitor}</div>
          <div class="actions">
            <button class="approve" data-act="approve" data-id="${esc(p.id)}">Approve</button>
            <button class="reject" data-act="reject" data-id="${esc(p.id)}">Reject</button>
          </div>
        </div>`;
    })
    .join("");
}

async function refresh() {
  try {
    const summary = await invoke("health_summary");
    const overall = $("overall");
    overall.className = "pill " + statusClass(summary.overall);
    overall.textContent = summary.overall || "unknown";
    renderMonitors(summary.monitors);
    const proposals = await invoke("list_proposals");
    renderProposals(proposals);
    $("updated").textContent = "Updated " + new Date().toLocaleTimeString();
  } catch (e) {
    $("monitors").innerHTML = '<div class="empty">Error: ' + esc(e) + "</div>";
  }
}

async function decide(act, id, btn) {
  const card = btn.closest(".proposal");
  card.querySelectorAll("button").forEach((b) => (b.disabled = true));
  try {
    await invoke(act === "approve" ? "approve_proposal" : "reject_proposal", { id });
    await refresh();
  } catch (e) {
    card.querySelector(".rationale").textContent = "Failed: " + e;
    card.querySelectorAll("button").forEach((b) => (b.disabled = false));
  }
}

document.addEventListener("click", (ev) => {
  const btn = ev.target.closest("button[data-act]");
  if (btn) decide(btn.dataset.act, btn.dataset.id, btn);
});
$("refresh").addEventListener("click", refresh);

refresh();
// Light polling so the popover stays fresh while open.
setInterval(refresh, 5000);
