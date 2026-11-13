// SPDX-License-Identifier: GPL-3.0-or-later
// OpenFarm's interface. No framework: the page asks the local app (/local/*) about this machine and the
// farm server (/api/v1/*, relayed by the local app) about the farm, with a long poll that returns as soon
// as anything changes.
"use strict";

const T = {
  en: {
    tab_jobs: "Jobs", tab_machines: "Machines", tab_submit: "Submit", tab_me: "This machine",
    no_jobs: "No jobs yet. Send one from 3ds Max or Blender, or from Submit.",
    job: "Job", engine: "Engine", state: "State", progress: "Progress", frames: "Frames", eta: "Time left",
    by: "Sent by", priority: "Priority", actions: "",
    st_queued: "Queued", st_rendering: "Rendering", st_paused: "Paused", st_done: "Done", st_failed: "Failed", st_canceled: "Canceled",
    pause: "Pause", resume: "Resume", cancel: "Cancel", retry: "Retry failed", archive: "Remove", back: "← Jobs",
    confirm_cancel: "Cancel this job? Frames already rendered are kept.", confirm_archive: "Remove this job from the list? Its frames stay on disk.",
    done_of: "{d} of {t} frames", failed_n: "{n} failed", warnings_n: "{n} with warnings", avg: "{s} per frame",
    scene: "Scene", output: "Output", tasks: "Tasks", machine: "Machine", started: "Started", time: "Time", log: "Log", error: "Error",
    excluded: "Not given to (failed too often): {m}",
    no_machines: "No machines yet. Start OpenFarm on other computers and join this farm.",
    approve: "Approve", suspend: "Suspend", activate: "Activate", forget: "Forget",
    waiting_approval: "Waiting for approval", offline: "Offline", online: "Online", suspended: "Suspended", busy: "Rendering", idle: "Idle",
    node: "Render node", workstation: "Workstation", server_machine: "Farm server",
    software: "Software", none_found: "no render software found",
    submit_title: "Submit a scene without opening it",
    submit_help: "The scene and the output folder must be on a path every machine can reach (a share such as \\\\server\\projects), not a local disk.",
    f_scene: "Scene file", f_engine: "Engine", f_name: "Job name", f_frames: "Frames", f_camera: "Camera", f_output: "Output folder",
    f_file: "File name", f_format: "Format", f_priority: "Priority", f_chunk: "Frames per task", f_order: "Order", f_device: "Cycles device",
    f_res: "Resolution", f_paused: "Send paused",
    h_frames: "1-100, 1-240:4 (every 4th) or 1,7,22", h_camera: "Empty: the scene's camera", h_file: "#### becomes the frame number",
    h_chunk: "Empty: automatic (single frames first, then about 5 minutes per task)", h_res: "Empty: the scene's own",
    o_smart: "Smart: first, last and middle frames first, then filling the gaps", o_linear: "In order",
    send: "Send to the farm", sent: "Job {id} sent", sending: "Sending…",
    me_title: "This machine", role: "Role", role_server: "Farm server", role_agent: "Member of a farm", farm: "Farm",
    conn: "Connection", c_connected: "Connected", c_waiting_approval: "Waiting for approval on the farm server", c_connecting: "Connecting…", c_error: "Not connected",
    render_here: "Render on this machine", render_on: "This machine takes work from the farm.", render_off: "Suspended: this machine takes no work.",
    farm_suspended: "The farm has suspended this machine.",
    integrations: "Integrations", install_max: "Install the 3ds Max button", install_blender: "Install the Blender add-on",
    installed_max: "Installed. Restart 3ds Max, then add “Send to OpenFarm” (category OpenFarm) to a toolbar or menu.",
    installed_blender: "Installed and turned on. Restart Blender: Properties › Output › OpenFarm.",
    change_role: "Change role", quit: "Quit OpenFarm", confirm_quit: "Quit OpenFarm on this computer? Renders running here stop and go back to the queue.",
    setup_title: "Set up OpenFarm on this computer",
    setup_server: "This computer runs the farm", setup_server_p: "It keeps the queue and hands out work. Other computers join it. It can render too.",
    setup_join: "Join a farm", setup_join_p: "This computer renders for a farm and can send jobs to it.",
    farm_name: "Farm name", render_too: "Render on this computer too", this_name: "This computer's name",
    find: "Search the network", searching: "Searching…", none_found_net: "No farm found. Type the server's address.", address: "Server address",
    kind_node: "Dedicated render node", kind_ws: "Workstation (someone works on it)",
    start: "Start", restarting: "Starting OpenFarm…",
    not_ready: "Waiting for the farm server…", unauthorized: "This browser is not part of the farm. Open OpenFarm on a machine of the farm.",
    gpu: "GPU", cpu: "CPU", ram: "RAM", minutes: "{m} min", seconds: "{s} s", hours: "{h} h {m} min",
  },
  es: {
    tab_jobs: "Trabajos", tab_machines: "Máquinas", tab_submit: "Enviar", tab_me: "Esta máquina",
    no_jobs: "Aún no hay trabajos. Envía uno desde 3ds Max o Blender, o desde Enviar.",
    job: "Trabajo", engine: "Motor", state: "Estado", progress: "Progreso", frames: "Fotogramas", eta: "Queda",
    by: "Enviado por", priority: "Prioridad", actions: "",
    st_queued: "En cola", st_rendering: "Renderizando", st_paused: "En pausa", st_done: "Terminado", st_failed: "Con fallos", st_canceled: "Cancelado",
    pause: "Pausar", resume: "Reanudar", cancel: "Cancelar", retry: "Repetir fallidos", archive: "Quitar", back: "← Trabajos",
    confirm_cancel: "¿Cancelar este trabajo? Los fotogramas ya hechos se conservan.", confirm_archive: "¿Quitar este trabajo de la lista? Sus fotogramas siguen en disco.",
    done_of: "{d} de {t} fotogramas", failed_n: "{n} con fallo", warnings_n: "{n} con avisos", avg: "{s} por fotograma",
    scene: "Escena", output: "Salida", tasks: "Tareas", machine: "Máquina", started: "Inicio", time: "Tiempo", log: "Log", error: "Error",
    excluded: "No se le da a (falló demasiado): {m}",
    no_machines: "Aún no hay máquinas. Abre OpenFarm en otros equipos y únelos a esta granja.",
    approve: "Aprobar", suspend: "Suspender", activate: "Activar", forget: "Olvidar",
    waiting_approval: "Esperando aprobación", offline: "Desconectada", online: "Conectada", suspended: "Suspendida", busy: "Renderizando", idle: "Libre",
    node: "Nodo de render", workstation: "Estación de trabajo", server_machine: "Servidor de la granja",
    software: "Software", none_found: "no se encontró software de render",
    submit_title: "Enviar una escena sin abrirla",
    submit_help: "La escena y la carpeta de salida tienen que estar en una ruta que vean todas las máquinas (una carpeta compartida como \\\\servidor\\proyectos), no en un disco local.",
    f_scene: "Archivo de escena", f_engine: "Motor", f_name: "Nombre del trabajo", f_frames: "Fotogramas", f_camera: "Cámara", f_output: "Carpeta de salida",
    f_file: "Nombre de archivo", f_format: "Formato", f_priority: "Prioridad", f_chunk: "Fotogramas por tarea", f_order: "Orden", f_device: "Dispositivo de Cycles",
    f_res: "Resolución", f_paused: "Enviar en pausa",
    h_frames: "1-100, 1-240:4 (uno de cada 4) o 1,7,22", h_camera: "Vacío: la cámara de la escena", h_file: "#### se cambia por el número de fotograma",
    h_chunk: "Vacío: automático (primero de uno en uno, después unos 5 minutos por tarea)", h_res: "Vacío: la de la escena",
    o_smart: "Inteligente: primero el primero, el último y el central, y después rellenando huecos", o_linear: "En orden",
    send: "Enviar a la granja", sent: "Trabajo {id} enviado", sending: "Enviando…",
    me_title: "Esta máquina", role: "Papel", role_server: "Servidor de la granja", role_agent: "Miembro de una granja", farm: "Granja",
    conn: "Conexión", c_connected: "Conectada", c_waiting_approval: "Esperando aprobación en el servidor de la granja", c_connecting: "Conectando…", c_error: "Sin conexión",
    render_here: "Renderizar en esta máquina", render_on: "Esta máquina recibe trabajo de la granja.", render_off: "Suspendida: esta máquina no recibe trabajo.",
    farm_suspended: "La granja ha suspendido esta máquina.",
    integrations: "Integraciones", install_max: "Instalar el botón de 3ds Max", install_blender: "Instalar el complemento de Blender",
    installed_max: "Instalado. Reinicia 3ds Max y añade «Send to OpenFarm» (categoría OpenFarm) a una barra o un menú.",
    installed_blender: "Instalado y activado. Reinicia Blender: Propiedades › Salida › OpenFarm.",
    change_role: "Cambiar papel", quit: "Salir de OpenFarm", confirm_quit: "¿Salir de OpenFarm en este equipo? Los renders que hay aquí se paran y vuelven a la cola.",
    setup_title: "Configurar OpenFarm en este equipo",
    setup_server: "Este equipo lleva la granja", setup_server_p: "Guarda la cola y reparte el trabajo. Los demás equipos se unen a él. También puede renderizar.",
    setup_join: "Unirse a una granja", setup_join_p: "Este equipo renderiza para una granja y puede enviarle trabajos.",
    farm_name: "Nombre de la granja", render_too: "Renderizar también en este equipo", this_name: "Nombre de este equipo",
    find: "Buscar en la red", searching: "Buscando…", none_found_net: "No se encontró ninguna granja. Escribe la dirección del servidor.", address: "Dirección del servidor",
    kind_node: "Nodo de render dedicado", kind_ws: "Estación de trabajo (alguien trabaja en ella)",
    start: "Empezar", restarting: "Arrancando OpenFarm…",
    not_ready: "Esperando al servidor de la granja…", unauthorized: "Este navegador no forma parte de la granja. Abre OpenFarm en una máquina de la granja.",
    gpu: "GPU", cpu: "CPU", ram: "RAM", minutes: "{m} min", seconds: "{s} s", hours: "{h} h {m} min",
  },
};
const lang = (navigator.language || "en").toLowerCase().startsWith("es") ? "es" : "en";
const t = (k, vars) => {
  let s = (T[lang] && T[lang][k]) || T.en[k] || k;
  if (vars) for (const [a, b] of Object.entries(vars)) s = s.replaceAll("{" + a + "}", b);
  return s;
};
const ENGINES = { vray: "V-Ray", vray_gpu: "V-Ray GPU", corona: "Corona", fstorm: "FStorm", cycles: "Cycles", eevee: "Eevee", dummy: "Test" };

const $ = (sel, root = document) => root.querySelector(sel);
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const h = (html) => { const d = document.createElement("div"); d.innerHTML = html.trim(); return d.firstElementChild; };

let farm = null;       // last /api/v1/state
let me = null;         // last /local/info
let page = location.hash.slice(1) || "jobs";
let version = 0;
let detail = null;     // job detail being shown

function dur(secs) {
  if (secs == null || !isFinite(secs)) return "–";
  secs = Math.round(secs);
  if (secs < 90) return t("seconds", { s: secs });
  const m = Math.round(secs / 60);
  if (m < 90) return t("minutes", { m });
  return t("hours", { h: Math.floor(m / 60), m: m % 60 });
}
function when(ms) { return ms ? new Date(ms).toLocaleString() : "–"; }
function toast(text, bad) {
  const el = $("#toast"); el.textContent = text; el.className = "toast" + (bad ? " bad" : "");
  clearTimeout(toast.timer); toast.timer = setTimeout(() => el.classList.add("hidden"), 4000);
}
function modal(html) { $("#modalBody").innerHTML = html; $("#modal").classList.remove("hidden"); }
$("#modal").addEventListener("click", (e) => { if (e.target.id === "modal" || e.target.classList.contains("modal-close")) $("#modal").classList.add("hidden"); });
document.addEventListener("keydown", (e) => { if (e.key === "Escape") $("#modal").classList.add("hidden"); });

async function api(path, body) {
  const opts = body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) };
  const r = await fetch(path, opts);
  if (!r.ok) throw new Error((await r.text()) || r.statusText);
  const type = r.headers.get("content-type") || "";
  return type.includes("json") ? r.json() : r.text();
}

// ---- data loops -------------------------------------------------------------------------------------

async function loadMe() {
  try { me = await api("/local/info"); } catch { me = null; }
}

async function farmLoop() {
  for (;;) {
    if (!me || me.role === "unset") { await sleep(1500); continue; }
    try {
      const s = await api("/api/v1/state?since=" + version);
      farm = s; version = s.v;
      render();
      if (page === "job" && detail) refreshDetail();
    } catch (e) {
      farm = null; render(String(e.message || e));
      await sleep(2000);
    }
  }
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function meLoop() {
  for (;;) {
    const before = JSON.stringify(me);
    await loadMe();
    if (JSON.stringify(me) !== before) render();
    await sleep(3000);
  }
}

// ---- navigation ------------------------------------------------------------------------------------

for (const b of document.querySelectorAll("#tabs button")) {
  b.textContent = t(b.dataset.t);
  b.addEventListener("click", () => go(b.dataset.page));
}
function go(p, arg) {
  page = p;
  if (p === "job") { detail = { id: arg, data: null }; location.hash = "job/" + arg; refreshDetail(); }
  else { detail = null; location.hash = p; }
  render();
}
window.addEventListener("hashchange", () => {
  const [p, id] = location.hash.slice(1).split("/");
  if (p === "job" && id && (!detail || detail.id != id)) go("job", Number(id));
  else if (p && p !== page && p !== "job") go(p);
});

// ---- rendering --------------------------------------------------------------------------------------

function render(error) {
  for (const b of document.querySelectorAll("#tabs button")) b.classList.toggle("on", b.dataset.page === page || (page === "job" && b.dataset.page === "jobs"));
  $("#farmName").textContent = farm ? farm.farm : (me && me.farm_name) || "";
  renderStatus();
  const main = $("#main");
  if (!me) { main.innerHTML = `<div class="empty">${esc(t("restarting"))}</div>`; return; }
  if (me.role === "unset") { if (!$("#setup")) renderSetup(main); return; }
  if (page === "me") return renderMe(main);
  if (page === "submit") { if (!$("#submitForm")) renderSubmit(main); return; }
  if (!farm) { main.innerHTML = `<div class="empty">${esc(error && error.includes("not part") ? t("unauthorized") : t("not_ready"))}<div class="small">${esc(error || "")}</div></div>`; return; }
  if (page === "machines") return renderMachines(main);
  if (page === "job") return renderDetail(main);
  return renderJobs(main);
}

function renderStatus() {
  const el = $("#connStatus");
  if (!me) { el.innerHTML = `<span class="dot bad"></span>OpenFarm`; return; }
  const a = me.agent || {};
  let cls = "ok", text = me.name;
  if (me.renders) {
    if (a.state === "connected") { cls = a.tasks && a.tasks.length ? "run" : (a.accepting && a.enabled ? "ok" : "warn"); }
    else if (a.state === "waiting_approval") { cls = "warn"; text += " · " + t("waiting_approval"); }
    else { cls = "bad"; text += " · " + t("c_" + (a.state || "connecting")); }
  }
  el.innerHTML = `<span class="dot ${cls}"></span>${esc(text)}`;
}

function progressBar(j) {
  const pct = (n) => (j.total ? (100 * n) / j.total : 0);
  return `<div class="prog"><i class="d" style="width:${pct(j.done)}%"></i><i class="r" style="width:${pct(j.rendering)}%"></i><i class="f" style="width:${pct(j.failed)}%"></i></div>`;
}

function renderJobs(main) {
  if (!farm.jobs.length) { main.innerHTML = `<div class="empty">${esc(t("no_jobs"))}</div>`; return; }
  const rows = farm.jobs.map((j) => `
    <tr class="click" data-id="${j.id}">
      <td><b>${esc(j.name)}</b><div class="small muted">#${j.id} · ${esc(j.scene)}</div></td>
      <td>${esc(ENGINES[j.engine] || j.engine)}</td>
      <td><span class="badge ${j.state}">${esc(t("st_" + j.state))}</span></td>
      <td style="min-width:180px">${progressBar(j)}<div class="small muted">${esc(t("done_of", { d: j.done, t: j.total }))}${j.failed ? " · " + esc(t("failed_n", { n: j.failed })) : ""}${j.warnings ? " · " + esc(t("warnings_n", { n: j.warnings })) : ""}</div></td>
      <td class="hide-sm">${j.eta_secs != null ? esc(dur(j.eta_secs)) : "–"}<div class="small muted">${j.avg_secs != null ? esc(t("avg", { s: dur(j.avg_secs) })) : ""}</div></td>
      <td class="hide-sm"><input type="number" min="0" max="100" value="${j.priority}" class="prio" data-id="${j.id}" style="width:64px"></td>
      <td class="hide-sm small muted">${esc(j.submitted_by)}</td>
      <td style="text-align:right">${jobButtons(j)}</td>
    </tr>`).join("");
  main.innerHTML = `<table><thead><tr><th>${t("job")}</th><th>${t("engine")}</th><th>${t("state")}</th><th>${t("progress")}</th><th class="hide-sm">${t("eta")}</th><th class="hide-sm">${t("priority")}</th><th class="hide-sm">${t("by")}</th><th></th></tr></thead><tbody>${rows}</tbody></table>`;
  wireJobButtons(main);
  for (const tr of main.querySelectorAll("tr.click")) tr.addEventListener("click", (e) => { if (!e.target.closest("button,input")) go("job", Number(tr.dataset.id)); });
  for (const inp of main.querySelectorAll("input.prio")) inp.addEventListener("change", () => act(inp.dataset.id, "priority", Number(inp.value)));
}

function jobButtons(j) {
  const b = [];
  if (j.state === "paused") b.push(["resume", t("resume")]);
  else if (j.state === "queued" || j.state === "rendering") b.push(["pause", t("pause")]);
  if (j.failed) b.push(["retry_failed", t("retry")]);
  if (!["done", "failed", "canceled"].includes(j.state)) b.push(["cancel", t("cancel")]);
  else b.push(["archive", t("archive")]);
  return b.map(([a, label]) => `<button class="btn small ${a === "cancel" || a === "archive" ? "danger" : ""}" data-act="${a}" data-id="${j.id}">${esc(label)}</button>`).join(" ");
}
function wireJobButtons(root) {
  for (const btn of root.querySelectorAll("button[data-act]")) btn.addEventListener("click", (e) => {
    e.stopPropagation();
    const a = btn.dataset.act;
    if (a === "cancel" && !confirm(t("confirm_cancel"))) return;
    if (a === "archive" && !confirm(t("confirm_archive"))) return;
    act(btn.dataset.id, a);
    if (a === "archive" && page === "job") go("jobs");
  });
}
async function act(id, action, priority) {
  try { await api(`/api/v1/jobs/${id}/action`, { action, priority }); } catch (e) { toast(e.message, true); }
}

async function refreshDetail() {
  if (!detail) return;
  try { detail.data = await api(`/api/v1/jobs/${detail.id}`); } catch (e) { detail.data = { error: e.message }; }
  if (page === "job") renderDetail($("#main"));
}

function renderDetail(main) {
  const d = detail && detail.data;
  if (!d) { main.innerHTML = `<div class="empty">…</div>`; return; }
  if (d.error) { main.innerHTML = `<p><a href="#jobs">${esc(t("back"))}</a></p><div class="empty">${esc(d.error)}</div>`; return; }
  const j = d.summary, s = d.spec;
  const frames = d.frames.map((f) => {
    const cls = f.state + (f.warning && f.state === "done" ? " warn" : "");
    const img = f.thumb ? `<img loading="lazy" src="/api/v1/jobs/${j.id}/thumbs/${f.frame}?v=${f.secs || 0}" alt="">` : "";
    const title = [f.machine, f.secs != null ? dur(f.secs) : "", f.warning || ""].filter(Boolean).join(" · ");
    return `<div class="frame ${cls} ${img ? "" : "empty"}" data-frame="${f.frame}" title="${esc(title)}">${img || esc(t("st_" + (f.state === "pending" ? "queued" : f.state)))}<span class="n">${f.frame}</span>${f.secs != null ? `<span class="s">${esc(dur(f.secs))}</span>` : ""}</div>`;
  }).join("");
  const tasks = d.tasks.slice().reverse().slice(0, 300).map((tk) => `
    <tr><td>#${tk.id}</td><td>${esc(machineName(tk.machine))}</td><td>${esc(fmtFrames(tk.frames))}</td>
    <td><span class="badge ${tk.state === "done" ? "done" : tk.state === "failed" ? "failed" : tk.state === "canceled" ? "" : "rendering"}">${esc(tk.state)}</span></td>
    <td class="small">${esc(when(tk.started))}</td><td>${tk.finished ? esc(dur((tk.finished - tk.started) / 1000)) : "…"}</td>
    <td class="small" style="max-width:420px">${esc(tk.error || "")}</td>
    <td><button class="btn small" data-log="${tk.id}">${esc(t("log"))}</button></td></tr>`).join("");
  main.innerHTML = `
    <p class="row"><a href="#jobs">${esc(t("back"))}</a></p>
    <div class="card">
      <div class="row"><h1 class="grow" style="margin:0">${esc(j.name)} <span class="badge ${j.state}">${esc(t("st_" + j.state))}</span></h1>${jobButtons(j)}</div>
      <div style="margin:14px 0">${progressBar(j)}</div>
      <div class="kv">
        <div>${t("progress")}</div><div>${esc(t("done_of", { d: j.done, t: j.total }))}${j.failed ? " · " + esc(t("failed_n", { n: j.failed })) : ""}${j.avg_secs != null ? " · " + esc(t("avg", { s: dur(j.avg_secs) })) : ""}${j.eta_secs != null ? " · " + esc(t("eta")) + " " + esc(dur(j.eta_secs)) : ""}</div>
        <div>${t("engine")}</div><div>${esc(ENGINES[s.engine] || s.engine)}${s.device ? " · " + esc(s.device) : ""}${s.camera ? " · " + esc(s.camera) : ""}</div>
        <div>${t("frames")}</div><div>${esc(s.frames)}</div>
        <div>${t("scene")}</div><div>${esc(s.scene.original_path || s.scene.file_name)}</div>
        <div>${t("output")}</div><div>${esc(s.output.folder)}\\${esc(s.output.name)}.${esc(s.output.format)}</div>
        <div>${t("by")}</div><div>${esc(s.submitted_by)} · ${esc(when(j.created))}</div>
        ${j.error ? `<div>${t("error")}</div><div style="color:var(--bad)">${esc(j.error)}</div>` : ""}
        ${j.excluded.length ? `<div></div><div class="muted">${esc(t("excluded", { m: j.excluded.join(", ") }))}</div>` : ""}
      </div>
    </div>
    <h2>${t("frames")}</h2><div class="frames">${frames}</div>
    <h2>${t("tasks")}</h2>
    <table><thead><tr><th></th><th>${t("machine")}</th><th>${t("frames")}</th><th>${t("state")}</th><th>${t("started")}</th><th>${t("time")}</th><th>${t("error")}</th><th></th></tr></thead><tbody>${tasks}</tbody></table>`;
  wireJobButtons(main);
  for (const el of main.querySelectorAll(".frame")) el.addEventListener("click", () => {
    const f = d.frames.find((x) => x.frame == el.dataset.frame);
    const img = f.thumb ? `<img src="/api/v1/jobs/${j.id}/thumbs/${f.frame}?v=${f.secs || 0}">` : "";
    modal(`<h2 style="margin-top:0">${esc(t("frames"))} ${f.frame}</h2>${img}<p class="muted">${esc([f.machine, f.secs != null ? dur(f.secs) : "", f.warning || "", f.attempts ? "×" + f.attempts : ""].filter(Boolean).join(" · "))}</p>${f.task ? `<button class="btn" data-log="${f.task}">${esc(t("log"))}</button>` : ""}`);
    wireLogs($("#modalBody"));
  });
  wireLogs(main);
}
function wireLogs(root) {
  for (const b of root.querySelectorAll("button[data-log]")) b.addEventListener("click", async () => {
    const text = await api(`/api/v1/tasks/${b.dataset.log}/log`).catch((e) => e.message);
    modal(`<h2 style="margin-top:0">${esc(t("log"))} #${esc(b.dataset.log)}</h2><pre class="log">${esc(text || "—")}</pre>`);
    const pre = $("#modalBody pre"); pre.scrollTop = pre.scrollHeight;
  });
}
function machineName(id) { const m = farm && farm.machines.find((x) => x.id === id); return m ? m.name : id; }
function fmtFrames(list) {
  if (!list.length) return "";
  const out = []; let a = list[0], b = list[0];
  for (const f of list.slice(1).concat([null])) { if (f === b + 1) { b = f; continue; } out.push(a === b ? a : a + "-" + b); a = b = f; }
  return out.join(",");
}

function renderMachines(main) {
  if (!farm.machines.length) { main.innerHTML = `<div class="empty">${esc(t("no_machines"))}</div>`; return; }
  const cards = farm.machines.map((m) => {
    const tel = m.telemetry || {};
    let state = !m.online ? ["", t("offline")] : !m.approved ? ["waiting", t("waiting_approval")] : !m.enabled || !m.accepting ? ["paused", t("suspended")] : m.tasks.length ? ["rendering", t("busy")] : ["done", t("idle")];
    const kind = m.rank === 0 ? t("node") : m.rank === 2 ? t("server_machine") : t("workstation");
    const ramPct = m.caps.ram_mb ? (100 * tel.ram_used_mb) / m.caps.ram_mb : 0;
    const gpus = (m.caps.gpus || []).map((g) => {
      const l = (tel.gpus || []).find((x) => x.index === g.index) || {};
      return `<div class="meter"><span>${t("gpu")} ${g.index}</span><div class="bar"><i style="width:${l.util_pct || 0}%"></i></div><span>${l.util_pct || 0}% · ${l.temp_c || 0}°C</span></div>
              <div class="small muted" style="margin:-2px 0 4px 60px">${esc(g.name)} · ${Math.round((l.vram_used_mb || 0) / 1024)}/${Math.round(g.vram_mb / 1024)} GB${l.watts ? " · " + Math.round(l.watts) + " W" : ""}</div>`;
    }).join("");
    const sw = (m.caps.software || []).map((s) => `<span class="chip">${esc(s.name)} ${esc(s.version)}</span>`).join("") || `<span class="small muted">${esc(t("none_found"))}</span>`;
    const tasks = m.tasks.map((tk) => `<div class="task"><b>${esc(tk.job_name)}</b> <span class="muted">· ${esc(tk.slot)} · ${esc(tk.frames)}</span>${tk.frame != null ? `<div class="bar"><i style="width:${tk.pct}%"></i></div><div class="small muted">${t("frames")} ${tk.frame} · ${Math.round(tk.pct)}%</div>` : ""}</div>`).join("");
    const btns = [];
    if (!m.approved && m.online) btns.push(`<button class="btn small primary" data-m="${esc(m.id)}" data-a="approve">${esc(t("approve"))}</button>`);
    if (m.approved) btns.push(m.enabled ? `<button class="btn small" data-m="${esc(m.id)}" data-a="disable">${esc(t("suspend"))}</button>` : `<button class="btn small" data-m="${esc(m.id)}" data-a="enable">${esc(t("activate"))}</button>`);
    if (!m.online) btns.push(`<button class="btn small danger" data-m="${esc(m.id)}" data-a="forget">${esc(t("forget"))}</button>`);
    return `<div class="card machine">
      <div class="row"><h3 class="grow">${esc(m.name)}</h3><span class="badge ${state[0]}">${esc(state[1])}</span></div>
      <div class="small muted">${esc(kind)}${m.address ? " · " + esc(m.address) : ""}${m.version ? " · v" + esc(m.version) : ""}</div>
      ${m.online ? `<div style="margin-top:10px">
        <div class="meter"><span>${t("cpu")}</span><div class="bar"><i style="width:${tel.cpu_pct || 0}%"></i></div><span>${Math.round(tel.cpu_pct || 0)}%</span></div>
        <div class="small muted" style="margin:-2px 0 4px 60px">${esc(m.caps.cpu)} · ${m.caps.threads}</div>
        <div class="meter"><span>${t("ram")}</span><div class="bar"><i style="width:${ramPct}%"></i></div><span>${Math.round((tel.ram_used_mb || 0) / 1024)}/${Math.round(m.caps.ram_mb / 1024)} GB</span></div>
        ${gpus}</div>` : ""}
      <div class="chips">${sw}</div>
      ${tasks}
      <div class="row" style="margin-top:12px">${btns.join("")}</div>
    </div>`;
  }).join("");
  main.innerHTML = `<div class="cards">${cards}</div>`;
  for (const b of main.querySelectorAll("button[data-m]")) b.addEventListener("click", async () => {
    try { await api(`/api/v1/machines/${encodeURIComponent(b.dataset.m)}/action`, { action: b.dataset.a }); } catch (e) { toast(e.message, true); }
  });
}

function renderSubmit(main) {
  const opt = (v, l, sel) => `<option value="${v}" ${sel ? "selected" : ""}>${esc(l)}</option>`;
  main.innerHTML = `
    <h1>${esc(t("submit_title"))}</h1><p class="muted" style="max-width:900px">${esc(t("submit_help"))}</p>
    <form class="grid" id="submitForm" autocomplete="off">
      <label>${t("f_scene")}</label><input type="text" name="scene" placeholder="\\\\server\\projects\\shot\\scene.max" required>
      <label>${t("f_engine")}</label><select name="engine">${Object.entries(ENGINES).map(([k, v]) => opt(k, v, k === "corona")).join("")}</select>
      <label class="cycles">${t("f_device")}</label><select name="device" class="cycles">${["OPTIX", "CUDA", "HIP", "ONEAPI", "CPU"].map((d) => opt(d, d)).join("")}</select>
      <label>${t("f_name")}</label><input type="text" name="jobname">
      <label>${t("f_frames")}</label><input type="text" name="frames" value="1" required><div class="hint">${esc(t("h_frames"))}</div>
      <label>${t("f_camera")}</label><input type="text" name="camera"><div class="hint">${esc(t("h_camera"))}</div>
      <label>${t("f_output")}</label><input type="text" name="folder" placeholder="\\\\server\\projects\\shot\\renders" required>
      <label>${t("f_file")}</label><input type="text" name="file" value="frame_####"><div class="hint">${esc(t("h_file"))}</div>
      <label>${t("f_format")}</label><select name="format">${["exr", "png", "jpg"].map((f) => opt(f, f.toUpperCase())).join("")}</select>
      <label>${t("f_res")}</label><div class="row"><input type="number" name="w" placeholder="1920" style="width:110px"> × <input type="number" name="h" placeholder="1080" style="width:110px"></div><div class="hint">${esc(t("h_res"))}</div>
      <label>${t("f_priority")}</label><input type="number" name="priority" value="50" min="0" max="100">
      <label>${t("f_chunk")}</label><input type="number" name="chunk" min="1"><div class="hint">${esc(t("h_chunk"))}</div>
      <label>${t("f_order")}</label><select name="order">${opt("smart", t("o_smart"))}${opt("linear", t("o_linear"))}</select>
      <label>${t("f_paused")}</label><div><input type="checkbox" name="paused"></div>
      <div></div><div><button class="btn primary" type="submit">${esc(t("send"))}</button></div>
    </form>`;
  const form = $("#submitForm");
  const syncEngine = () => { for (const el of form.querySelectorAll(".cycles")) el.style.display = form.engine.value === "cycles" ? "" : "none"; };
  form.engine.addEventListener("change", syncEngine); syncEngine();
  form.scene.addEventListener("change", () => {
    if (!form.jobname.value) form.jobname.value = form.scene.value.split(/[\\/]/).pop().replace(/\.[^.]+$/, "");
    const ext = form.scene.value.toLowerCase().split(".").pop();
    if (ext === "blend" && !["cycles", "eevee"].includes(form.engine.value)) { form.engine.value = "cycles"; syncEngine(); }
    if (ext === "max" && ["cycles", "eevee"].includes(form.engine.value)) { form.engine.value = "corona"; syncEngine(); }
  });
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const btn = form.querySelector("button[type=submit]"); btn.disabled = true; btn.textContent = t("sending");
    try {
      const engine = form.engine.value;
      const scene = engine === "dummy" ? { hash: "0".repeat(64), file_name: "test", original_path: form.scene.value, size: 0 } : await api("/api/v1/scenes/from-path", { path: form.scene.value });
      const spec = {
        name: form.jobname.value || scene.file_name, engine, scene, frames: form.frames.value,
        camera: form.camera.value || null,
        output: { folder: form.folder.value, name: form.file.value || "frame_####", format: form.format.value },
        priority: Number(form.priority.value || 50), chunk: form.chunk.value ? Number(form.chunk.value) : null,
        order: form.order.value, device: engine === "cycles" ? form.device.value : null,
        resolution: form.w.value && form.h.value ? [Number(form.w.value), Number(form.h.value)] : null,
        settings: {}, submitted_by: me ? me.name : "", paused: form.paused.checked,
      };
      const r = await api("/api/v1/jobs", spec);
      toast(t("sent", { id: r.id }));
      go("job", r.id);
    } catch (err) { toast(err.message, true); }
    finally { btn.disabled = false; btn.textContent = t("send"); }
  });
}

function renderMe(main) {
  const a = me.agent || {};
  const sw = ((a.caps && a.caps.software) || []).map((s) => `<span class="chip" title="${esc(s.path)}">${esc(s.name)} ${esc(s.version)}</span>`).join("") || `<span class="muted">${esc(t("none_found"))}</span>`;
  const gpus = ((a.caps && a.caps.gpus) || []).map((g) => esc(g.name)).join(", ");
  main.innerHTML = `
    <h1>${esc(t("me_title"))}</h1>
    <div class="card" style="max-width:820px">
      <div class="kv">
        <div>${t("this_name")}</div><div><b>${esc(me.name)}</b> <span class="muted small">v${esc(me.version)}</span></div>
        <div>${t("role")}</div><div>${esc(me.role === "server" ? t("role_server") : t("role_agent"))}${me.role === "agent" ? " · " + esc(me.kind === "node" ? t("kind_node") : t("kind_ws")) : ""}</div>
        <div>${t("farm")}</div><div>${esc(a.farm || me.farm_name || "")} <span class="muted small">${esc(me.server_url)}</span></div>
        ${me.renders ? `<div>${t("conn")}</div><div><span class="dot ${a.state === "connected" ? "ok" : a.state === "waiting_approval" ? "warn" : "bad"}"></span>${esc(t("c_" + (a.state || "connecting")))} <span class="small muted">${esc(a.message || "")}</span></div>
        <div>${t("render_here")}</div><div><label><input type="checkbox" id="accepting" ${a.accepting ? "checked" : ""}> ${esc(a.accepting ? t("render_on") : t("render_off"))}</label>${a.enabled === false ? `<div class="small" style="color:var(--warn)">${esc(t("farm_suspended"))}</div>` : ""}</div>` : ""}
        <div>${t("cpu")}</div><div>${esc((a.caps && a.caps.cpu) || "")} ${a.caps ? "· " + a.caps.threads + " · " + Math.round(a.caps.ram_mb / 1024) + " GB" : ""}</div>
        <div>${t("gpu")}</div><div>${gpus || "–"}</div>
        <div>${t("software")}</div><div class="chips" style="margin:0">${sw}</div>
      </div>
      <h2>${esc(t("integrations"))}</h2>
      <div class="row"><button class="btn" data-install="max">${esc(t("install_max"))}</button><button class="btn" data-install="blender">${esc(t("install_blender"))}</button></div>
      <div class="row" style="margin-top:18px"><button class="btn" id="changeRole">${esc(t("change_role"))}</button><span class="grow"></span><button class="btn danger" id="quit">${esc(t("quit"))}</button></div>
    </div>`;
  const acc = $("#accepting");
  if (acc) acc.addEventListener("change", async () => { await api("/local/accepting", { accepting: acc.checked }); await loadMe(); render(); });
  for (const b of main.querySelectorAll("button[data-install]")) b.addEventListener("click", async () => {
    b.disabled = true;
    try { await api("/local/install/" + b.dataset.install, {}); toast(t("installed_" + b.dataset.install)); }
    catch (e) { toast(e.message, true); } finally { b.disabled = false; }
  });
  $("#changeRole").addEventListener("click", () => { renderSetup(main, true); });
  $("#quit").addEventListener("click", async () => { if (confirm(t("confirm_quit"))) await api("/local/quit", {}).catch(() => {}); });
}

function renderSetup(main, changing) {
  main.innerHTML = `
    <div id="setup"><h1 style="text-align:center;margin-top:20px">${esc(t("setup_title"))}</h1>
    <div class="setup">
      <div class="card"><h2>${esc(t("setup_server"))}</h2><p>${esc(t("setup_server_p"))}</p>
        <form class="grid" style="grid-template-columns:1fr" id="fServer">
          <label>${t("farm_name")}<input type="text" name="farm" value="${esc((me && me.farm_name) || "")}"></label>
          <label>${t("this_name")}<input type="text" name="pcname" value="${esc((me && me.name) || "")}"></label>
          <label><input type="checkbox" name="render" checked> ${esc(t("render_too"))}</label>
          <button class="btn primary" type="submit">${esc(t("start"))}</button>
        </form></div>
      <div class="card"><h2>${esc(t("setup_join"))}</h2><p>${esc(t("setup_join_p"))}</p>
        <form class="grid" style="grid-template-columns:1fr" id="fJoin">
          <button class="btn" type="button" id="find">${esc(t("find"))}</button>
          <div class="servers" id="servers"></div>
          <label>${t("address")}<input type="text" name="server" placeholder="192.168.1.20" value="${esc((me && me.server) || "")}"></label>
          <label>${t("this_name")}<input type="text" name="pcname" value="${esc((me && me.name) || "")}"></label>
          <select name="kind">${`<option value="workstation">${esc(t("kind_ws"))}</option><option value="node">${esc(t("kind_node"))}</option>`}</select>
          <button class="btn primary" type="submit">${esc(t("start"))}</button>
        </form></div>
    </div>${changing ? `<p style="text-align:center"><a href="#me" onclick="location.reload()">${esc(t("back").replace("Jobs", "").replace("Trabajos", ""))}</a></p>` : ""}</div>`;
  const start = async (body) => {
    try { await api("/local/setup", body); } catch (e) { toast(e.message, true); return; }
    main.innerHTML = `<div class="empty">${esc(t("restarting"))}</div>`;
    for (let i = 0; i < 60; i++) { await sleep(1000); try { const info = await api("/local/info"); if (info.role !== "unset") { location.hash = "jobs"; location.reload(); return; } } catch {} }
  };
  $("#fServer").addEventListener("submit", (e) => { e.preventDefault(); const f = e.target; start({ role: "server", farm_name: f.farm.value, name: f.pcname.value, render: f.render.checked }); });
  $("#fJoin").addEventListener("submit", (e) => { e.preventDefault(); const f = e.target; start({ role: "agent", server: f.server.value, name: f.pcname.value, kind: f.kind.value }); });
  $("#find").addEventListener("click", async () => {
    const box = $("#servers"); box.innerHTML = `<span class="muted">${esc(t("searching"))}</span>`;
    const found = await api("/local/discover").catch(() => []);
    if (!found.length) { box.innerHTML = `<span class="muted small">${esc(t("none_found_net"))}</span>`; return; }
    box.innerHTML = found.map((s) => `<div class="server" data-a="${esc(s.address)}"><b>${esc(s.name)}</b><span class="muted small">${esc(s.address)} · v${esc(s.version)}</span></div>`).join("");
    for (const el of box.querySelectorAll(".server")) el.addEventListener("click", () => {
      for (const o of box.querySelectorAll(".server")) o.classList.remove("on");
      el.classList.add("on"); $("#fJoin").server.value = el.dataset.a;
    });
  });
}

// ---- start --------------------------------------------------------------------------------------------
(async () => {
  await loadMe();
  const [p, id] = location.hash.slice(1).split("/");
  if (p === "job" && id) go("job", Number(id)); else render();
  meLoop();
  farmLoop();
})();
