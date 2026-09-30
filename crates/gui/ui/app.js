/* G-Link GUI（白色玻璃 · FlashBoost 布局） */
"use strict";

const $ = (id) => document.getElementById(id);
const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const appWin = window.__TAURI__.window.getCurrentWindow();

/* ---------- 自绘窗口控制 ---------- */
$("btnMin").addEventListener("click", () => appWin.minimize());
$("btnMax").addEventListener("click", () => appWin.toggleMaximize());
$("btnClose").addEventListener("click", () => appWin.close());

/* ---------- Toast ---------- */
const toastEl = $("toast");
let toastTimer;
function showToast(msg, err) {
  toastEl.textContent = msg;
  toastEl.classList.toggle("err", !!err);
  toastEl.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toastEl.classList.remove("show"), 2200);
}

/* ---------- 智能选区：切到延迟最低的节点 ---------- */
async function smartPick() {
  const ok = nodes.filter((n) => n.lat != null);
  if (!ok.length) { showToast("节点延迟探测中，请稍候再试", true); return; }
  ok.sort((a, b) => a.lat - b.lat);
  const best = ok[0];
  if (best.id === selectedId) { showToast(`当前节点已是优选（${Math.round(best.lat)}ms）`); return; }
  await selectNode(best.id);
  showToast(`已智能选择「${best.name || best.addr}」· ${Math.round(best.lat)}ms`);
}

/* ---------- 侧边导航视图切换 ---------- */
document.querySelectorAll(".nav-item").forEach((item) => {
  item.addEventListener("click", () => {
    const v = item.dataset.view;
    if (v === "region") { smartPick(); return; }   // 智能选区是动作，不切视图
    document.querySelectorAll(".nav-item").forEach((x) => x.classList.remove("active"));
    item.classList.add("active");
    $("viewHome").style.display = v === "home" ? "flex" : "none";
    $("viewLogs").style.display = v === "logs" ? "flex" : "none";
    $("viewSettings").style.display = v === "setup" ? "flex" : "none";
  });
});

/* ---------- 配置：多节点 ---------- */
const DEFAULT_PROC = "TslGame.exe";
const DEFAULT_NODES = [
  { id: 1, name: "中国香港 CN2", addr: "64.90.1.52:41000", token: "xawvnpyxj4t6bc67a3jysvdc" },
];
const REGION_NAME = { kr: "韩国", jp: "日本", us: "美国", hk: "中国香港" };

let nodes = [];          // [{id,name,addr,token,country,lat}]
let selectedId = null;
let nextId = 100;

function loadCfg() {
  $("cfgProc").value = localStorage.getItem("pubg_accel_proc") || DEFAULT_PROC;
  try { nodes = JSON.parse(localStorage.getItem("pubg_accel_nodes") || "null"); } catch (e) { nodes = null; }
  if (!Array.isArray(nodes) || !nodes.length) {
    const old = JSON.parse(localStorage.getItem("pubg_accel_cfg") || "null");
    nodes = (old && old.relay)
      ? [{ id: 1, name: "节点 1", addr: old.relay, token: old.token || "" }]
      : DEFAULT_NODES.map((n) => ({ ...n }));
  }
  nextId = Math.max(100, ...nodes.map((n) => +n.id || 0)) + 1;
  // 迁移：旧版默认节点（韩国 210.x）整体替换为新默认节点；缺名称/令牌时补齐
  const oldIdx = nodes.findIndex((n) => n.addr === "210.126.235.176:41000");
  if (oldIdx >= 0) nodes[oldIdx] = { ...nodes[oldIdx], ...DEFAULT_NODES[0] };
  nodes.forEach((n) => {
    const d = DEFAULT_NODES.find((x) => x.addr === n.addr);
    if (d) {
      if (!n.token) n.token = d.token;
      if (!n.name || n.name === "默认节点" || /^节点/.test(n.name)) n.name = d.name;
    }
  });
  const savedSel = +localStorage.getItem("pubg_accel_sel");
  selectedId = nodes.some((n) => n.id === savedSel) ? savedSel : nodes[0].id;
  nodes.forEach((n) => (n.lat = null));
}
function saveCfg() {
  localStorage.setItem("pubg_accel_proc", $("cfgProc").value.trim() || DEFAULT_PROC);
  localStorage.setItem("pubg_accel_nodes", JSON.stringify(nodes.map(({ id, name, addr, token, country }) => ({ id, name, addr, token, country }))));
  localStorage.setItem("pubg_accel_sel", String(selectedId));
}
function selNode() { return nodes.find((n) => n.id === selectedId) || nodes[0]; }

/* ---- 国家识别 ---- */
function countryOf(n) {
  if (n.country && n.country !== "auto") return n.country;
  const t = (n.name || "") + " " + n.addr;
  if (/韩|kr|seoul|首尔/i.test(t)) return "kr";
  if (/日|jp|japan|东京|tokyo/i.test(t)) return "jp";
  if (/美|us|usa|america|洛杉矶|los angeles|圣何塞/i.test(t)) return "us";
  if (/香港|hk|hong ?kong/i.test(t)) return "hk";
  return null;
}

const GLOBE_SVG = '<svg viewBox="0 0 24 24" fill="none" stroke="#9aa8b0" stroke-width="1.6"><circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c2.5 2.6 3.8 5.7 3.8 9s-1.3 6.4-3.8 9c-2.5-2.6-3.8-5.7-3.8-9S9.5 5.6 12 3z"/></svg>';

function flagHTML(cc) {
  if (!cc) return GLOBE_SVG;
  const NS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("viewBox", "0 0 36 24");
  const use = document.createElementNS(NS, "use");
  use.setAttribute("href", "#flag-" + cc);
  svg.appendChild(use);
  return svg.outerHTML;
}

/* ---------- 当前节点显示（加速面板） ---------- */
function renderSelect() {
  const s = selNode();
  if (!s) return;
  $("nodeRegion").textContent = s.name || s.addr;
  const nf = $("nodeFlag");
  const cc = countryOf(s);
  nf.classList.toggle("none", !cc);
  nf.title = cc ? REGION_NAME[cc] : "未知地区";
  nf.innerHTML = flagHTML(cc);
}

/* ---------- 节点下拉面板 ---------- */
const nodePop = $("nodePop");
function renderPop() {
  let html = "";
  let lastRegion = null;
  nodes.forEach((n) => {
    const cc = countryOf(n);
    const region = cc ? REGION_NAME[cc] : "其他节点";
    if (region !== lastRegion) { html += `<div class="np-group">${region}</div>`; lastRegion = region; }
    const latCls = n.lat == null ? "" : n.lat < 100 ? " g" : n.lat < 150 ? " y" : " r";
    html += `
      <div class="np-item${n.id === selectedId ? " sel" : ""}" data-id="${n.id}">
        <div class="flag${cc ? "" : " none"}">${flagHTML(cc)}</div>
        <div class="np-main">
          <div class="np-region">${n.name || n.addr}</div>
          <div class="np-name">${n.addr}</div>
        </div>
        <div class="np-ping${latCls}">${n.lat == null ? "--" : Math.round(n.lat) + "ms"}</div>
        <svg class="np-check" viewBox="0 0 16 16"><path d="M3 8.5l3 3L13 4.5" fill="none" stroke="#10b981" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>
        <span class="np-ops">
          <button class="np-op" data-act="edit" title="编辑">✎</button>
          <button class="np-op del" data-act="del" title="删除">×</button>
        </span>
      </div>`;
  });
  html += `<div class="np-add" id="npAdd">＋ 添加节点</div>`;
  nodePop.innerHTML = html;
}
function openPop() {
  const sel = $("nodeSelect");
  const r = sel.getBoundingClientRect();
  nodePop.style.left = Math.max(8, Math.min(r.left, window.innerWidth - 352)) + "px";
  nodePop.style.top = r.bottom + 8 + "px";
  nodePop.style.width = Math.max(340, r.width) + "px";
  nodePop.classList.add("show");
  $("nodeArrow").style.transform = "rotate(180deg)";
}
function closePop() {
  nodePop.classList.remove("show");
  $("nodeArrow").style.transform = "";
}
$("nodeSelect").addEventListener("click", (e) => {
  e.stopPropagation();
  nodePop.classList.contains("show") ? closePop() : openPop();
});
nodePop.addEventListener("click", async (e) => {
  e.stopPropagation();
  if (e.target.closest("#npAdd")) { closePop(); openNodeDialog(null); return; }
  const op = e.target.closest(".np-op");
  if (op) {
    const id = +op.closest(".np-item").dataset.id;
    if (op.dataset.act === "edit") { closePop(); openNodeDialog(nodes.find((n) => n.id === id)); }
    else removeNode(id);
    return;
  }
  const item = e.target.closest(".np-item");
  if (!item) return;
  await selectNode(+item.dataset.id);
  closePop();
  const n = selNode();
  showToast(`已切换至「${n.name || n.addr}」节点`);
});
document.addEventListener("click", closePop);
window.addEventListener("resize", closePop);

/* ---------- 设置弹窗里的节点列表 ---------- */
function renderNodeList() {
  const el = $("nodeList");
  el.innerHTML = "";
  nodes.forEach((n) => {
    const chip = document.createElement("div");
    chip.className = "node-chip" + (n.id === selectedId ? " sel" : "");
    chip.title = n.addr + (n.token ? "" : "（未设置令牌）");
    const cc = countryOf(n);
    const flagWrap = document.createElement("span");
    flagWrap.className = "flag" + (cc ? "" : " none");
    flagWrap.innerHTML = flagHTML(cc);
    chip.appendChild(flagWrap);
    const txt = document.createElement("div");
    txt.className = "nc-txt";
    const name = document.createElement("span"); name.className = "nc-name"; name.textContent = n.name || n.addr;
    const addr = document.createElement("span"); addr.className = "nc-addr"; addr.textContent = n.addr;
    txt.append(name, addr);
    const lat = document.createElement("span");
    lat.className = "nc-lat" + (n.lat == null ? "" : n.lat < 100 ? " g" : n.lat < 150 ? " y" : " r");
    lat.textContent = n.lat == null ? "--" : String(Math.round(n.lat));
    const del = document.createElement("button");
    del.className = "nc-del"; del.textContent = "×"; del.title = "删除节点";
    del.addEventListener("click", (e) => { e.stopPropagation(); removeNode(n.id); });
    chip.append(txt, lat, del);
    chip.addEventListener("click", () => selectNode(n.id));
    chip.addEventListener("dblclick", () => openNodeDialog(n));
    el.appendChild(chip);
  });
}

function renderNodes() { renderSelect(); renderPop(); renderNodeList(); renderGames(); }

/* ---------- 节点选择 / 删除 ---------- */
async function selectNode(id) {
  if (id === selectedId) return;
  selectedId = id;
  saveCfg(); renderNodes();
  const s = selNode();
  pushLog(`已切换节点 → ${s.name || s.addr}`, "sys");
  setLatency(null);
  sparkHist = [];       // 新节点重置曲线
  probeFlags = [];      // 新节点重置丢包统计
  failStreak = 0;
  lastProbeOk = null;
  renderSpark(); renderLoss();
  if (running) {
    pushLog("正在热切换到新节点…", "sys");
    try {
      await invoke("stop_engine");
      await invoke("start_engine", { relay: s.addr, token: s.token, process: $("cfgProc").value.trim() || DEFAULT_PROC });
      pushLog("热切换完成，已接入新节点", "session");
      setRunning(true);
      showToast("热切换完成，已接入新节点");
    } catch (e) { pushLog("切换失败: " + e, "err"); showToast("切换失败", "err"); }
  }
}
function removeNode(id) {
  nodes = nodes.filter((n) => n.id !== id);
  if (!nodes.length) nodes = DEFAULT_NODES.map((n) => ({ ...n }));
  if (selectedId === id) selectedId = nodes[0].id;
  saveCfg(); renderNodes();
  pushLog("节点已删除", "sys");
}

/* ---------- 添加/编辑节点对话框 ---------- */
let editingId = null;
function openNodeDialog(n) {
  editingId = n ? n.id : null;
  $("dlgTitle").textContent = n ? "编辑节点" : "添加节点";
  $("dlgName").value = n ? n.name : "";
  $("dlgAddr").value = n ? n.addr : "";
  $("dlgToken").value = n ? n.token : "";
  $("dlgCountry").value = n && n.country ? n.country : "auto";
  $("nodeOverlay").classList.add("on");
  $("dlgAddr").focus();
}
function closeNodeDialog() { $("nodeOverlay").classList.remove("on"); }
$("btnAddNode").addEventListener("click", () => openNodeDialog(null));
$("dlgCancel").addEventListener("click", closeNodeDialog);
$("nodeOverlay").addEventListener("click", (e) => { if (e.target === $("nodeOverlay")) closeNodeDialog(); });
$("dlgOk").addEventListener("click", () => {
  const addr = $("dlgAddr").value.trim();
  if (!addr) { $("dlgAddr").focus(); return; }
  const item = {
    name: $("dlgName").value.trim() || addr,
    addr,
    token: $("dlgToken").value.trim(),
    country: $("dlgCountry").value,
  };
  if (editingId != null) {
    const i = nodes.findIndex((n) => n.id === editingId);
    if (i >= 0) nodes[i] = { ...nodes[i], ...item };
    pushLog("节点已更新", "sys");
  } else {
    nodes.push({ id: nextId++, ...item, lat: null });
    pushLog(`节点已添加: ${item.name}`, "sys");
  }
  saveCfg(); renderNodes(); closeNodeDialog();
});

/* ---------- 节点延迟探测（与引擎无关，常开） ---------- */
let sparkHist = [];   // 选中节点延迟历史（曲线）
let probeFlags = [];  // 最近探测成败（丢包率）：1=通 0=超时
let lastLat = null;
let failStreak = 0;   // 选中节点连续探测失败次数
let lastProbeOk = null; // 上一轮选中节点探测结果：true/false/null
let autoSwitching = false; // 自动切换防重入

async function probeLoop() {
  if (!nodes.length) return;
  const results = await Promise.all(nodes.map(async (n) => {
    try { return [n.id, await invoke("probe_node", { addr: n.addr, token: n.token || "x" })]; }
    catch (e) { return [n.id, null]; }
  }));
  results.forEach(([id, ms]) => {
    const n = nodes.find((x) => x.id === id);
    if (!n) return;
    n.lat = ms == null ? null : Math.round(ms);
  });
  const s = selNode();
  if (s) {
    setLatency(s.lat);
    if (s.lat != null) {
      sparkHist.push(s.lat);
      if (sparkHist.length > 24) sparkHist.shift();
    }
    probeFlags.push(s.lat == null ? 0 : 1);
    if (probeFlags.length > 20) probeFlags.shift();
    renderSpark(); renderLoss();
    // 探测状态变化时打日志（避免每轮刷屏）
    const ok = s.lat != null;
    if (ok) failStreak = 0; else failStreak++;
    if (ok !== lastProbeOk) {
      if (ok) pushLog(`节点探测正常 → ${s.addr}（${Math.round(s.lat)}ms）`, "sys");
      else pushLog(`节点探测超时 → ${s.addr}（服务器无响应或链路丢包，丢包率统计中）`, "err");
      lastProbeOk = ok;
    }
    // 节点故障自动切换：加速中当前节点连续 4 轮（约 10s）探测失败 → 切到最优备选
    if (running && !ok && failStreak >= 4 && !autoSwitching) {
      const okNodes = nodes.filter((n) => n.id !== s.id && n.lat != null);
      if (okNodes.length) {
        okNodes.sort((a, b) => a.lat - b.lat);
        const best = okNodes[0];
        autoSwitching = true;
        pushLog(`节点 ${s.addr} 持续失联，自动切换至「${best.name || best.addr}」（${Math.round(best.lat)}ms）`, "err");
        showToast("节点失联，已自动切换至最优节点", true);
        await selectNode(best.id);
        autoSwitching = false;
      }
    }
  }
  renderNodes();   // 统一在轮末刷新（延迟数字/下拉/游戏卡）
}

/* ---------- 延迟显示：<100 绿 / <150 黄 / >=150 红 ---------- */
function setLatency(ms) {
  const pingVal = $("pingVal"), nodePing = $("nodePing"), trend = $("pingTrend");
  if (ms == null) {
    pingVal.innerHTML = "— <small>ms</small>";
    nodePing.textContent = "--"; nodePing.className = "ns-ping";
    trend.textContent = "—"; trend.className = "trend";
    lastLat = null;
    updateNetPill(null);
    return;
  }
  const v = Math.round(ms);
  pingVal.innerHTML = v + " <small>ms</small>";
  nodePing.textContent = v + "ms";
  nodePing.className = "ns-ping " + (v < 100 ? "g" : v < 150 ? "y" : "r");
  updateNetPill(v);
  if (lastLat != null && lastLat > 0) {
    const d = ((lastLat - ms) / lastLat) * 100;
    if (d >= 1) { trend.textContent = "▼ " + Math.round(d) + "%"; trend.className = "trend good"; }
    else if (d <= -1) { trend.textContent = "▲ " + Math.round(-d) + "%"; trend.className = "trend bad"; }
    else { trend.textContent = "—"; trend.className = "trend"; }
  }
  lastLat = ms;
  renderSaved();
}

/* ---------- 顶栏网络状态 pill ---------- */
function updateNetPill(lat) {
  const netTxt = $("netTxt"), netPill = $("netPill");
  if (lat == null) {
    if (failStreak >= 2) {
      netTxt.textContent = "节点不可达";
      netPill.className = "net-pill bad";
    } else {
      netTxt.textContent = "检测网络中…";
      netPill.className = "net-pill";
    }
    return;
  }
  netTxt.textContent = lat < 100 ? "网络状态良好" : lat < 150 ? "网络状态一般" : "网络状态较差";
  netPill.className = "net-pill " + (lat < 100 ? "" : lat < 150 ? "warn" : "bad");
}

function renderSpark() {
  const line = $("sparkLine"), area = $("sparkArea");
  if (sparkHist.length < 2) { line.setAttribute("points", ""); area.setAttribute("points", ""); return; }
  const min = Math.min(...sparkHist), max = Math.max(...sparkHist), rng = (max - min) || 1;
  const pts = sparkHist.map((v, i) => {
    const x = i * (100 / (sparkHist.length - 1));
    const y = 26 - ((v - min) / rng) * 22;
    return x.toFixed(1) + "," + y.toFixed(1);
  });
  line.setAttribute("points", pts.join(" "));
  area.setAttribute("points", "0,30 " + pts.join(" ") + " 100,30");
}
function renderLoss() {
  const el = $("lossVal");
  if (!probeFlags.length) { el.innerHTML = "0.0<small>%</small>"; el.className = "s-value v-good"; return; }
  const loss = (1 - probeFlags.reduce((a, b) => a + b, 0) / probeFlags.length) * 100;
  el.innerHTML = loss.toFixed(1) + "<small>%</small>";
  el.className = "s-value " + (loss < 1 ? "v-good" : loss < 5 ? "v-mid" : "v-bad");
}

/* ---------- 游戏库 ---------- */
/* 选择游戏 → 自动绑定对应加速进程；加速中且进程变更时热重启 */
async function selectGame(name, proc) {
  const prev = $("cfgProc").value.trim() || DEFAULT_PROC;
  localStorage.setItem("pubg_accel_game", name);
  applyHeroCover(name);
  $("cfgProc").value = proc;
  saveCfg();
  $("detectTxt").textContent = "目标进程 · " + proc + " · UDP 智能分流";
  renderGames();
  showToast(`已选择「${name}」· 加速进程绑定 ${proc}`);
  pushLog(`已选择游戏「${name}」，加速进程绑定 ${proc}`, "sys");
  if (running && prev !== proc) {
    pushLog("加速进程已变更，正在热重启引擎…", "sys");
    try {
      const s = selNode();
      await invoke("stop_engine");
      await invoke("start_engine", { relay: s.addr, token: s.token, process: proc });
      pushLog("热重启完成，新进程已接入隧道", "session");
      showToast("已按新进程热重启加速");
    } catch (e) { pushLog("热重启失败: " + e, "err"); }
  }
}

function renderGames() {
  const grid = $("gameGrid");
  grid.innerHTML = "";
  const s = selNode();
  const ping = s && s.lat != null ? Math.round(s.lat) : null;
  const curGame = localStorage.getItem("pubg_accel_game") || "绝地求生";
  // 绝地求生（内置）
  const card = document.createElement("div");
  card.className = "game-card" + (curGame === "绝地求生" ? " selected" : "");
  const icon = document.createElement("img");
  icon.className = "g-icon-img"; icon.draggable = false; icon.alt = "PUBG"; icon.src = "assets/logo.png";
  const meta = document.createElement("div");
  meta.className = "g-meta";
  const lvl = ping == null ? "检测中" : ping < 100 ? "优" : ping < 150 ? "良" : "波动";
  meta.innerHTML = `
    <div class="g-title">绝地求生</div>
    <div class="g-ping"><span class="ping-dot ${ping == null ? "" : ping < 100 ? "p-good" : ping < 150 ? "p-mid" : "p-bad"}"></span><span class="g-ping-txt">${ping == null ? "延迟检测中" : ping + "ms · " + lvl}</span></div>`;
  card.append(icon, meta);
  card.addEventListener("click", () => selectGame("绝地求生", "TslGame.exe"));
  grid.appendChild(card);
  // 自定义游戏卡
  for (const g of loadCustomGames()) {
    const c = document.createElement("div");
    c.className = "game-card" + (curGame === g.name ? " selected" : "");
    const ic = document.createElement("img");
    ic.className = "g-icon-img"; ic.draggable = false; ic.alt = g.name;
    ic.src = g.img || "assets/pubg_bg.svg";
    ic.onerror = () => { ic.onerror = null; ic.src = "assets/logo.png"; };
    const mt = document.createElement("div");
    mt.className = "g-meta";
    const t = document.createElement("div"); t.className = "g-title"; t.textContent = g.name;
    const p = document.createElement("div"); p.className = "g-ping";
    const pt = document.createElement("span"); pt.className = "g-ping-txt"; pt.textContent = g.proc;
    p.append(pt); mt.append(t, p);
    const del = document.createElement("div");
    del.className = "g-del"; del.textContent = "×"; del.title = "删除该游戏";
    del.addEventListener("click", (e) => { e.stopPropagation(); removeCustomGame(g.name); });
    c.append(ic, mt, del);
    c.addEventListener("click", () => selectGame(g.name, g.proc));
    grid.appendChild(c);
  }
  // 自定义加速（添加卡）
  const add = document.createElement("div");
  add.className = "game-card add";
  add.innerHTML = `<div class="g-icon soon">+</div><div class="g-meta"><div class="g-title">自定义加速</div><div class="g-ping"><span class="g-ping-txt">添加自定义游戏</span></div></div>`;
  add.addEventListener("click", openCustomDialog);
  grid.appendChild(add);
}

/* ---------- 自定义加速游戏 ---------- */
const CUSTOM_KEY = "pubg_accel_custom_games";
function loadCustomGames() {
  try { return JSON.parse(localStorage.getItem(CUSTOM_KEY)) || []; } catch { return []; }
}
function saveCustomGames(list) { localStorage.setItem(CUSTOM_KEY, JSON.stringify(list)); }

/* 主页封面/标题联动：自定义游戏显示其封面，PUBG 恢复默认 */
function applyHeroCover(name) {
  const g = loadCustomGames().find((x) => x.name === name);
  const img = $("coverImg");
  if (g) {
    img.onerror = null;
    img.src = g.img || "assets/pubg_bg.jpg";
    $("coverName").textContent = g.name;
    $("coverSub").textContent = "自定义游戏";
    $("gameTitle").textContent = g.name;
  } else {
    img.onerror = function () { this.onerror = null; this.src = "assets/pubg_bg.svg"; };
    img.src = "assets/pubg_bg.jpg";
    $("coverName").textContent = "绝地求生";
    $("coverSub").textContent = "PUBG: BATTLEGROUNDS";
    $("gameTitle").textContent = "绝地求生 · PUBG";
  }
}

function removeCustomGame(name) {
  saveCustomGames(loadCustomGames().filter((g) => g.name !== name));
  if (localStorage.getItem("pubg_accel_game") === name) {
    localStorage.setItem("pubg_accel_game", "绝地求生");
    applyHeroCover("绝地求生");
  }
  renderGames();
  showToast(`已删除「${name}」`);
  pushLog(`已删除自定义游戏「${name}」`, "sys");
}

let dzData = null; // 当前选择的封面 dataURL
function openCustomDialog() {
  dzData = null;
  $("cgName").value = "";
  $("cgProc").value = "";
  $("dzPreview").hidden = true;
  $("dzPreview").removeAttribute("src");
  $("dropzone").classList.remove("has-img");
  $("customOverlay").classList.add("on");
}
function closeCustomDialog() { $("customOverlay").classList.remove("on"); }
$("cgCancel").addEventListener("click", closeCustomDialog);
$("customOverlay").addEventListener("click", (e) => { if (e.target === $("customOverlay")) closeCustomDialog(); });

const dz = $("dropzone");
dz.addEventListener("click", () => $("dzFile").click());
$("dzFile").addEventListener("change", (e) => {
  const f = e.target.files && e.target.files[0];
  if (f) handleImage(f);
  e.target.value = "";
});
dz.addEventListener("dragover", (e) => { e.preventDefault(); dz.classList.add("dragover"); });
dz.addEventListener("dragleave", () => dz.classList.remove("dragover"));
dz.addEventListener("drop", (e) => {
  e.preventDefault();
  dz.classList.remove("dragover");
  const f = e.dataTransfer.files && e.dataTransfer.files[0];
  if (f && f.type.startsWith("image/")) handleImage(f);
  else if (f) showToast("请拖入图片文件", true);
});
// 阻止把图拖进窗口其他位置触发浏览器默认打开
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => e.preventDefault());

function handleImage(file) {
  const reader = new FileReader();
  reader.onload = () => {
    const img = new Image();
    img.onload = () => {
      // 居中裁剪为 264×344 封面比例并压缩，控制 localStorage 占用
      const W = 264, H = 344;
      const cv = document.createElement("canvas");
      cv.width = W; cv.height = H;
      const ctx = cv.getContext("2d");
      const r = Math.max(W / img.width, H / img.height);
      const w = img.width * r, h = img.height * r;
      ctx.drawImage(img, (W - w) / 2, (H - h) / 2, w, h);
      dzData = cv.toDataURL("image/jpeg", 0.85);
      $("dzPreview").src = dzData;
      $("dzPreview").hidden = false;
      dz.classList.add("has-img");
    };
    img.src = reader.result;
  };
  reader.readAsDataURL(file);
}

$("cgSave").addEventListener("click", () => {
  const proc = $("cgProc").value.trim();
  if (!proc) { showToast("请填写游戏进程名（如 xxx.exe）", true); return; }
  let name = $("cgName").value.trim();
  if (!name) name = proc.replace(/\.exe$/i, "");
  const list = loadCustomGames();
  if (list.some((g) => g.name === name)) { showToast("已存在同名游戏", true); return; }
  list.push({ name, proc, img: dzData });
  if (JSON.stringify(list).length > 4000000) { showToast("图片总体积过大，请更换更小的图片", true); return; }
  saveCustomGames(list);
  closeCustomDialog();
  renderGames();
  selectGame(name, proc);
  pushLog(`已添加自定义游戏「${name}」并绑定进程 ${proc}`, "sys");
});

/* 搜索过滤游戏卡 */
$("searchInput").addEventListener("input", (e) => {
  const q = e.target.value.trim().toLowerCase();
  document.querySelectorAll("#gameGrid .game-card").forEach((c) => {
    c.style.display = c.textContent.toLowerCase().includes(q) ? "" : "none";
  });
});

/* ---------- 设置页 ---------- */
$("btnSave").addEventListener("click", () => {
  saveCfg();
  $("detectTxt").textContent = "目标进程 · " + ($("cfgProc").value.trim() || DEFAULT_PROC) + " · UDP 智能分流";
  showToast("配置已保存");
});

/* ---------- 装饰性交互 ---------- */
$("vipBadge").addEventListener("click", () => showToast("G-Link · 内测阶段免费使用"));
$("mbBtn").addEventListener("click", () => showToast("内测版本免费使用，无需付费"));
$("avatar").addEventListener("click", () => showToast("账户功能开发中"));

/* ---------- 日志 ---------- */
const logEl = $("log");
const logLines = [];
let logFilter = "all";
function classify(line) {
  if (/^\[latency\] \d+$/.test(line)) return "latency";
  if (/error|失败|Error|ERROR|拒绝|无法|denied|auth failed/i.test(line)) return "err";
  if (/\[加速成功\]|\[检测\]|session.*opened|replaced/i.test(line)) return "session";
  if (/^\[stats\]/.test(line)) return "stat";
  return "sys";
}
function pushLog(line, kind) {
  const t = new Date().toTimeString().slice(0, 8);
  const o = { t, line, kind };
  logLines.push(o);
  if (logLines.length > 400) logLines.shift();
  if (logFilter === "all" || logFilter === kind) appendLine(o);
}
function appendLine(o) {
  const div = document.createElement("div");
  div.className = "log-line " + o.kind;
  div.textContent = `[${o.t}] ${o.line}`;
  logEl.appendChild(div);
  while (logEl.childElementCount > 400) logEl.removeChild(logEl.firstChild);
  logEl.scrollTop = logEl.scrollHeight;
}
function renderLog() {
  logEl.innerHTML = "";
  logLines.filter((o) => logFilter === "all" || o.kind === logFilter).forEach(appendLine);
}
$("logFilters").addEventListener("click", (e) => {
  const b = e.target.closest(".log-filter");
  if (!b) return;
  document.querySelectorAll(".log-filter").forEach((x) => x.classList.remove("on"));
  b.classList.add("on");
  logFilter = b.dataset.k;
  renderLog();
});

/* ---------- 统计 + 实时速率 ---------- */
const fmt = (n) => (n >= 1e6 ? (n / 1e6).toFixed(1) + "M" : n >= 1e3 ? (n / 1e3).toFixed(1) + "k" : String(Math.round(n)));
let lastTun = { v: 0, t: 0 };
function handleLine(line) {
  pushLog(line, classify(line));
  const s = line.match(/\[stats\].*tunneled (\d+) \/ reinjected (\d+) \/ passthrough (\d+) \/ dropped (\d+) \/ sessions (\d+)/);
  if (s) {
    $("stTun").textContent = fmt(+s[1]);
    $("stRe").textContent = fmt(+s[2]);
    $("stPass").textContent = fmt(+s[3]);
    $("stSess").textContent = s[5];
    const now = Date.now(), v = +s[1];
    if (lastTun.t && v >= lastTun.v) {
      const pps = (v - lastTun.v) / ((now - lastTun.t) / 1000);
      $("speedVal").innerHTML = fmt(pps) + " <small>pps</small>";
    }
    lastTun = { v, t: now };
  }
  const m = line.match(/^\[latency\] (\d+)$/);
  if (m) setLatency(+m[1]);
  // 从引擎会话日志提取游戏服务器 IP，用于直连延迟基准
  const t = line.match(/tunnel session [0-9a-fx]+ -> ([0-9.]+):\d+/);
  if (t) gameServerIp = t[1];
}

/* ---------- 加速前后延迟对比（状态栏「累计节省延迟」） ---------- */
let gameServerIp = null;   // 当前隧道会话的游戏服务器 IP（引擎日志解析）
let directLat = null;      // 直连游戏服务器的 ICMP 延迟
function renderSaved() {
  const el = $("savedMs");
  if (!running || directLat == null || lastLat == null) { el.textContent = "—"; el.style.color = ""; return; }
  const diff = Math.round(directLat - lastLat);
  if (diff >= 0) { el.textContent = `↓ ${diff} ms`; el.style.color = "#10b981"; }
  else { el.textContent = `↑ ${-diff} ms`; el.style.color = "#f59e0b"; }
}
async function directPingLoop() {
  while (true) {
    if (gameServerIp && running) {
      try { directLat = await invoke("ping_direct", { ip: gameServerIp }); } catch { directLat = null; }
      renderSaved();
    }
    await new Promise((r) => setTimeout(r, 3000));
  }
}

/* ---------- 引擎管理 ---------- */
let running = false;

function setRunning(on) {
  running = on;
  $("boostRing").classList.toggle("on", on);
  $("btnLabel").textContent = on ? "加速中" : "一键加速";
  const st = $("boostState");
  st.textContent = on ? "已连接 · 专线加密传输" : "未加速";
  st.classList.toggle("on", on);
  if (!on) {
    lastTun = { v: 0, t: 0 };
    $("speedVal").innerHTML = "— <small>pps</small>";
    ["stTun", "stRe", "stPass", "stSess"].forEach((id) => ($(id).textContent = "0"));
  }
}

async function toggleBoost() {
  if (running) {
    try { await invoke("stop_engine"); showToast("已停止加速"); }
    catch (e) { pushLog("停止失败: " + e, "err"); showToast("停止失败", "err"); }
    return;
  }
  saveCfg();
  const s = selNode();
  if (!s) { showToast("没有可用节点，请先添加", "err"); return; }
  try {
    await invoke("start_engine", {
      relay: s.addr,
      token: s.token,
      process: $("cfgProc").value.trim() || DEFAULT_PROC,
    });
    setRunning(true);
    showToast("加速成功，延迟已大幅降低");
    pushLog(`加速引擎已拉起 → ${s.name || s.addr}`, "sys");
  } catch (e) {
    const msg = String(e);
    if (msg.includes("已在运行")) {
      setRunning(true);   // 后端在跑但界面失同步：纠正状态
      pushLog("引擎已在运行，界面状态已同步", "sys");
    } else {
      pushLog("启动引擎失败: " + msg, "err");
      showToast("启动失败: " + msg.slice(0, 60), "err");
    }
  }
  startPolling();
}
$("boostRing").addEventListener("click", toggleBoost);

/* 引擎日志轮询（主通道，事件为辅） */
let pollTimer = null;
function startPolling() {
  if (pollTimer) return;
  pollTimer = setInterval(async () => {
    try {
      const lines = await invoke("engine_logs");
      lines.forEach(handleLine);
      const on = await invoke("engine_running");
      if (!on && running) setRunning(false);
      if (on && !running) setRunning(true);
    } catch (e) {}
  }, 700);
}
listen("engine-log", (ev) => handleLine(String(ev.payload)));
listen("engine-exited", (ev) => {
  pushLog(`引擎已退出（code ${ev.payload}）`, "sys");
  setRunning(false);
  showToast("加速已停止");
});
window.addEventListener("beforeunload", () => {
  if (running) invoke("stop_engine").catch(() => {});
});

/* ---------- 今日加速时长 ---------- */
const todayKey = () => new Date().toISOString().slice(0, 10);
let todayMs = 0;
try {
  const t = JSON.parse(localStorage.getItem("pubg_accel_today") || "null");
  if (t && t.d === todayKey()) todayMs = t.ms || 0;
} catch (e) {}
function saveToday() { localStorage.setItem("pubg_accel_today", JSON.stringify({ d: todayKey(), ms: todayMs })); }
function renderToday() {
  const m = Math.floor(todayMs / 60000);
  $("todayBoost").textContent = m >= 60 ? `${Math.floor(m / 60)} 小时 ${m % 60} 分` : `${m} 分钟`;
}
setInterval(() => {
  if (running) { todayMs += 1000; saveToday(); }
  renderToday();
}, 1000);

/* ---------- 模式切换（底部状态栏） ---------- */
const modeSwitch = $("modeSwitch");
try {
  const m = localStorage.getItem("pubg_accel_mode");
  if (m) [...modeSwitch.children].forEach((s) => s.classList.toggle("active", s.textContent === m));
} catch (e) {}
modeSwitch.addEventListener("click", (e) => {
  if (e.target.tagName !== "SPAN") return;
  [...modeSwitch.children].forEach((s) => s.classList.remove("active"));
  e.target.classList.add("active");
  localStorage.setItem("pubg_accel_mode", e.target.textContent);
});

/* ---------- 启动 ---------- */
loadCfg();
renderNodes();
applyHeroCover(localStorage.getItem("pubg_accel_game") || "绝地求生");
renderToday();
setLatency(null);
$("detectTxt").textContent = "目标进程 · " + ($("cfgProc").value.trim() || DEFAULT_PROC) + " · UDP 智能分流";
pushLog("就绪。启动游戏后点击「一键加速」。", "sys");
invoke("engine_running").then((on) => { setRunning(on); if (on) startPolling(); }).catch(() => {});
startPolling();
probeLoop(); setInterval(probeLoop, 2500);   // 全节点延迟轮询
directPingLoop();                            // 直连延迟基准（延迟对比）
