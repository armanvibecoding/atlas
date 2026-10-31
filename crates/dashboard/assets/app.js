// Atlas paneli — vanilla JS, harici bağımlılık yok.
// GÜVENLİK: CLI verisi (journal notları, LLM çıktısı…) güvenilmezdir; DOM'a yalnız textContent ile
// yazılır, innerHTML hiç kullanılmaz.
"use strict";

const REFRESH_MS = 15000;
const EVENT_WINDOW = 1000; // olay akışı: son N seq penceresi
let lastSeq = 0;
let busy = false;
let selectedStrategy = null;
let commands = [];
const openEvents = new Set();

// ---------- yardımcılar ----------
const $ = (id) => document.getElementById(id);

function el(tag, props, ...kids) {
  const e = document.createElement(tag);
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v === undefined || v === null) continue;
      if (k === "class") e.className = v;
      else if (k === "text") e.textContent = String(v);
      else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
      else e.setAttribute(k, String(v));
    }
  }
  for (const c of kids.flat()) {
    if (c === undefined || c === null || c === false) continue;
    e.append(c instanceof Node ? c : String(c));
  }
  return e;
}

function put(id, ...nodes) {
  const box = $(id);
  box.replaceChildren(...nodes.flat().filter(Boolean));
}

function badge(text, kind) {
  return el("span", { class: "badge b-" + kind, text });
}

function fmtNum(x, digits) {
  if (typeof x !== "number" || !isFinite(x)) return x === null || x === undefined ? "—" : String(x);
  return x.toLocaleString("tr-TR", { maximumFractionDigits: digits ?? 0, minimumFractionDigits: digits ?? 0 });
}

function fmtUsd(x) {
  if (typeof x !== "number") return "—";
  const s = Math.abs(x).toLocaleString("tr-TR", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  return (x < 0 ? "−$" : "$") + s;
}

function compact(v, max) {
  let s;
  try { s = typeof v === "string" ? v : JSON.stringify(v); } catch (_) { s = String(v); }
  if (s === undefined) s = "";
  return max && s.length > max ? s.slice(0, max) + "…" : s;
}

function scrollTable(head, rows) {
  return el("div", { class: "scroll" },
    el("table", null,
      el("thead", null, el("tr", null, head.map((h) =>
        typeof h === "string" ? el("th", { text: h }) : el("th", { class: h.cls, text: h.t })))),
      el("tbody", null, rows)));
}

function td(v, cls) { return el("td", { class: cls }, v instanceof Node ? v : String(v ?? "—")); }

function msg(text, kind) { return el("div", { class: "msg " + kind, text }); }

function prioBadge(p) {
  const k = p === "high" ? "bad" : p === "medium" ? "warn" : "info";
  return badge(String(p || "?").toUpperCase(), k);
}

// API: {kind: ok|unavailable|error, env, data, ok, message, status}
async function api(path) {
  let r;
  try {
    r = await fetch("/api/" + path, { cache: "no-store", headers: { Accept: "application/json" } });
  } catch (e) {
    return { kind: "error", message: "sunucuya ulaşılamadı: " + e };
  }
  let body = null;
  try { body = await r.json(); } catch (_) { /* gövde JSON değil */ }
  if (r.status === 200 && body && body.schema === "atlas.cli/1") {
    return { kind: "ok", env: body, data: body.data, ok: body.ok === true, status: 200 };
  }
  if (r.status === 501) {
    return { kind: "unavailable", message: (body && body.error) || "henüz yok", status: 501, body };
  }
  return { kind: "error", message: (body && body.error) || "HTTP " + r.status, status: r.status, body };
}

function failNode(res) {
  return res.kind === "unavailable"
    ? msg("Henüz yok — bu komut CLI'da tanımlı değil (" + res.message + ")", "na")
    : msg("Hata: " + res.message, "err");
}

// ---------- tema ----------
function applyTheme(t) {
  if (t === "light" || t === "dark") document.documentElement.setAttribute("data-theme", t);
  else document.documentElement.removeAttribute("data-theme");
}
function initTheme() {
  let t = "auto";
  try { t = localStorage.getItem("atlas-theme") || "auto"; } catch (_) { /* depolama yok */ }
  $("theme").value = t;
  applyTheme(t);
  $("theme").addEventListener("change", (e) => {
    applyTheme(e.target.value);
    try { localStorage.setItem("atlas-theme", e.target.value); } catch (_) { /* yok say */ }
  });
}

// ---------- kartlar ----------
function renderStatus(res) {
  if (res.kind !== "ok") return put("durum", failNode(res));
  const d = res.data || {};
  const j = d.journal || {};
  if (j.last && typeof j.last.seq === "number") lastSeq = j.last.seq;
  const kills = d.kills || {};
  const killKeys = Object.keys(kills);
  const pos = d.positions || {};
  const posKeys = Object.keys(pos);
  const prop = d.prop || {};
  const tiles = el("div", { class: "tiles" },
    tile("Kill switch", killKeys.length ? badge(killKeys.length + " AKTİF", "bad") : badge("yok", "ok")),
    tile("Açık pozisyon", fmtNum(posKeys.length)),
    tile("Gerçekleşmiş net", fmtUsd(d.realized_net_usd)),
    tile("Journal olayı", fmtNum(j.events)),
    tile("Kilometre taşı", d.milestone || "—"));
  const parts = [tiles];
  if (killKeys.length) {
    parts.push(el("h3", { text: "Aktif kill switch'ler" }),
      scrollTable(["Kapsam", "Sebep"], killKeys.map((k) => el("tr", null, td(k), td(kills[k])))));
  }
  if (posKeys.length) {
    parts.push(el("h3", { text: "Açık pozisyonlar" }),
      scrollTable(["Strateji / sembol", "Ayrıntı"],
        posKeys.map((k) => el("tr", null, td(k), td(compact(pos[k], 300), "payload mono")))));
  }
  parts.push(el("h3", { text: "Prop config" }),
    el("dl", { class: "kv" },
      el("dt", { text: "Dosya" }), el("dd", { text: prop.file ?? "—" }),
      el("dt", { text: "Durum" }), el("dd", null, prop.loaded ? badge("yüklendi", "ok") : badge("yüklenemedi", "bad")),
      prop.loaded ? [el("dt", { text: "Firma" }), el("dd", { text: prop.firm ?? "—" }),
        el("dt", { text: "Hesap" }), el("dd", { text: prop.account ?? "—" })]
        : [el("dt", { text: "Hata" }), el("dd", { text: prop.error ?? "—" })],
      el("dt", { text: "Son olay" }),
      el("dd", { text: j.last ? "#" + j.last.seq + " " + j.last.kind + " · " + j.last.time : "—" }),
      el("dt", { text: "CLI saati" }), el("dd", { text: d.time ?? "—" })));
  put("durum", parts);
}

function tile(k, v) {
  return el("div", { class: "tile" }, el("div", { class: "k", text: k }),
    el("div", { class: "v" }, v instanceof Node ? v : String(v)));
}

function renderHealth(res) {
  if (res.kind !== "ok") return put("saglik", failNode(res));
  const d = res.data || {};
  const checks = Array.isArray(d.checks) ? d.checks : [];
  put("saglik",
    el("p", null, d.healthy ? badge("SAĞLIKLI", "ok") : badge("SORUNLU", "bad"),
      " ", el("span", { class: "muted", text: checks.filter((c) => c.ok).length + "/" + checks.length + " kontrol" })),
    el("ul", { class: "checks" }, checks.map((c) =>
      el("li", null, c.ok ? badge("OK", "ok") : badge("FAIL", "bad"),
        el("span", { class: "n", text: c.name }), el("span", { class: "d", text: c.detail })))));
}

function renderReplay(res) {
  if (res.kind !== "ok") return put("journal", failNode(res));
  const d = res.data || {};
  const mm = Array.isArray(d.mismatches) ? d.mismatches : [];
  const parts = [
    el("p", null, res.ok ? badge("TUTARLI", "ok") : badge("TUTARSIZ", "bad")),
    el("div", { class: "tiles" },
      tile("Olay", fmtNum(d.events)), tile("Checkpoint", fmtNum(d.checkpoints)), tile("Uyuşmazlık", fmtNum(mm.length))),
    el("dl", { class: "kv" }, el("dt", { text: "Son hash" }), el("dd", { class: "mono", text: d.final_hash ?? "—" })),
  ];
  if (mm.length) {
    parts.push(el("h3", { text: "Uyuşmazlıklar" }),
      scrollTable(["seq", "Kayıtlı", "Yeniden hesaplanan"],
        mm.map((m) => el("tr", null, td(m.seq), td(m.recorded, "mono"), td(m.recomputed, "mono")))));
  }
  put("journal", parts);
}

function renderData(res) {
  if (res.kind !== "ok") return put("veri", failNode(res));
  const d = res.data;
  if (!Array.isArray(d)) return put("veri", msg("Veri deposu okunamadı: " + compact(d && d.error ? d.error : d, 400), "err"));
  if (!d.length) return put("veri", msg("Depoda veri seti yok.", "na"));
  put("veri", scrollTable(["Veri seti", { t: "Bar", cls: "num" }, { t: "Kalite bulgusu", cls: "num" }, "Başlangıç", "Bitiş"],
    d.map((r) => el("tr", null, td(r.dataset, "mono"), td(fmtNum(r.bars), "num"),
      td(r.quality_issues ? badge(fmtNum(r.quality_issues), "warn") : badge("0", "ok"), "num"), td(r.from), td(r.to)))));
}

function gateText(last) {
  if (!last) return badge("henüz gauntlet yok", "info");
  if (last.passed) return badge("GEÇTİ", "ok");
  return badge("kapı " + (last.failed_gate ?? "?") + " düştü", "bad");
}

function renderStrategies(res) {
  if (res.kind !== "ok") return put("stratejiler", failNode(res));
  const rows = Array.isArray(res.data) ? res.data : [];
  if (!rows.length) return put("stratejiler", msg("strategies/ altında genom yok.", "na"));
  put("stratejiler",
    el("p", { class: "muted small", text: "Ayrıntı ve gauntlet raporu için satıra tıklayın." }),
    scrollTable(["Ad", "Enstrüman", "Aşama", "Son gauntlet", { t: "İşlem", cls: "num" }, { t: "Sharpe", cls: "num" }, "Deney", "Etiketler"],
      rows.map((r) => {
        if (r.error) return el("tr", null, td(compact(r.file, 120), "mono"), el("td", { colspan: "7" }, msg(r.error, "err")));
        const last = r.last_experiment;
        return el("tr", { class: "click", onclick: () => showStrategy(r.name) },
          td(r.name), td(r.instrument), td(r.stage === "gauntlet_geçti" ? badge("gauntlet geçti", "ok") : badge(r.stage || "aday", "info")),
          td(gateText(last)), td(last ? fmtNum(last.trades) : "—", "num"),
          td(last && typeof last.sharpe === "number" ? fmtNum(last.sharpe, 2) : "—", "num"),
          td(last ? "#" + last.id : "—"), td(Array.isArray(r.tags) ? r.tags.join(", ") : ""));
      })));
}

async function showStrategy(name) {
  selectedStrategy = name;
  const box = $("strateji-detay");
  box.hidden = false;
  put("strateji-detay", el("p", { class: "muted", text: name + " yükleniyor…" }));
  const res = await api("strategies/show/" + encodeURIComponent(name));
  if (selectedStrategy !== name) return;
  if (res.kind !== "ok") return put("strateji-detay", failNode(res));
  const d = res.data || {};
  put("strateji-detay",
    el("h3", { text: "Strateji: " + name }),
    el("dl", { class: "kv" },
      el("dt", { text: "Dosya" }), el("dd", { class: "mono", text: d.file ?? "—" }),
      el("dt", { text: "Hash" }), el("dd", { class: "mono", text: d.hash ?? "—" }),
      el("dt", { text: "Son deney" }), el("dd", null, gateText(d.last_experiment))),
    el("h3", { text: "Son gauntlet raporu" }),
    el("pre", { class: "md", text: d.report_md || "(henüz gauntlet raporu yok)" }),
    el("details", null, el("summary", { text: "Genom (JSON)" }),
      el("pre", { class: "raw", text: JSON.stringify(d.genome, null, 2) })),
    el("p", null, el("button", { type: "button", text: "Kapat", onclick: () => { selectedStrategy = null; box.hidden = true; } })));
}

// CSP satır içi style özniteliğine izin vermez; genişlik CSSOM ile verilir.
function barFill(pct) {
  const f = el("div", { class: "bar-fill" });
  f.style.width = Math.max(0, Math.min(100, pct)).toFixed(1) + "%";
  return f;
}

function gateSortKey(k) {
  const n = Number(k);
  return isFinite(n) ? n : 1e9;
}

function renderRegistry(res) {
  if (res.kind !== "ok") return put("sicil", failNode(res));
  const s = (res.data && res.data.summary) || {};
  const recent = (res.data && res.data.recent) || [];
  const hist = s.failed_gate_histogram || {};
  const keys = Object.keys(hist).sort((a, b) => gateSortKey(a) - gateSortKey(b));
  const max = Math.max(1, ...keys.map((k) => hist[k]));
  const src = s.by_source || {};
  put("sicil",
    el("div", { class: "tiles" },
      tile("Toplam deneme", fmtNum(s.trials)), tile("Geçen", fmtNum(s.passed)),
      tile("Ders", fmtNum(s.lessons)), tile("Holdout erişimi", fmtNum(s.holdout_accesses))),
    el("h3", { text: "Düşülen kapı dağılımı" }),
    keys.length ? el("div", { class: "bars" }, keys.map((k) =>
      el("div", { class: "bar-row" },
        el("span", { text: isFinite(Number(k)) ? "kapı " + k : k }),
        el("div", { class: "bar-track" }, barFill(100 * hist[k] / max)),
        el("span", { class: "num", text: fmtNum(hist[k]) }))))
      : msg("Henüz deney yok.", "na"),
    el("h3", { text: "Kaynak performansı" }),
    scrollTable(["Kaynak", { t: "Deneme", cls: "num" }, { t: "Geçen", cls: "num" }],
      Object.keys(src).map((k) => el("tr", null, td(k), td(fmtNum((src[k] || [])[0]), "num"), td(fmtNum((src[k] || [])[1]), "num")))),
    el("h3", { text: "Son denemeler" }),
    scrollTable(["#", "Ad", "Kaynak", "Sonuç", { t: "İşlem", cls: "num" }],
      recent.map((e) => el("tr", null, td(e.id), td(e.name), td(e.source), td(gateText(e)), td(fmtNum(e.trades), "num")))));
}

async function loadEvents() {
  const prio = $("ev-prio").value;
  const kind = $("ev-kind").value.trim();
  const show = Number($("ev-limit").value) || 50;
  if (kind && !/^[a-z_]{1,40}$/.test(kind)) return put("olaylar", msg("Tür yalnız küçük harf ve _ içerebilir.", "err"));
  const p = new URLSearchParams();
  p.set("since", "seq:" + Math.max(0, lastSeq - EVENT_WINDOW));
  p.set("limit", String(EVENT_WINDOW));
  if (prio) p.set("prio", prio);
  if (kind) p.set("kind", kind);
  const res = await api("events?" + p.toString());
  if (res.kind !== "ok") return put("olaylar", failNode(res));
  const evs = (Array.isArray(res.data) ? res.data : []).slice().reverse().slice(0, show);
  if (!evs.length) return put("olaylar", msg("Bu filtreyle olay yok (son " + EVENT_WINDOW + " olay içinde).", "na"));
  put("olaylar",
    el("p", { class: "muted small", text: "En yeniler üstte · son " + EVENT_WINDOW + " olay penceresi · gösterilen " + evs.length }),
    scrollTable(["seq", "Zaman (UTC)", "Öncelik", "Tür", "Anahtar", "İçerik"],
      evs.map((e) => el("tr", null, td(e.seq), td(e.time, "mono nowrap"), td(prioBadge(e.priority)), td(e.kind, "mono nowrap"),
        td(e.key ?? "—", "mono"), eventPayload(e.payload, e.seq)))));
}

// Kısa önizleme; tıklayınca tam içerik (biçimli JSON) açılır/kapanır.
// Açık tutulan satırlar otomatik yenilemede kapanmasın.
function eventPayload(p, seq) {
  const short = compact(p, 90);
  const full = JSON.stringify(p, null, 2);
  const cell = el("td", { class: "ev-payload mono", title: "tamamı için tıklayın" });
  const show = (open) => {
    cell.classList.toggle("open", open);
    cell.textContent = open ? full : short;
  };
  show(openEvents.has(seq));
  cell.addEventListener("click", () => {
    const open = !openEvents.has(seq);
    if (open) openEvents.add(seq); else openEvents.delete(seq);
    show(open);
  });
  return cell;
}

function renderGeneric(v) {
  if (v === null || v === undefined) return el("p", { class: "muted", text: "boş" });
  if (Array.isArray(v)) {
    if (!v.length) return el("p", { class: "muted", text: "boş liste" });
    if (v.every((x) => x && typeof x === "object" && !Array.isArray(x))) {
      const cols = [...new Set(v.flatMap((x) => Object.keys(x)))].slice(0, 10);
      return scrollTable(cols, v.map((x) => el("tr", null, cols.map((c) => td(compact(x[c], 200), "payload")))));
    }
    return el("pre", { class: "raw", text: JSON.stringify(v, null, 2) });
  }
  if (typeof v === "object") {
    return el("dl", { class: "kv" }, Object.keys(v).map((k) => [el("dt", { text: k }),
      el("dd", null, typeof v[k] === "object" && v[k] !== null ? el("pre", { class: "raw", text: JSON.stringify(v[k], null, 2) }) : String(v[k]))]));
  }
  return el("p", { text: String(v) });
}

async function loadM3() {
  const items = [["report/daily", "Günlük rapor"], ["report/weekly", "Haftalık rapor"], ["budget", "Bütçe"], ["approvals/list", "Bekleyen onaylar"]];
  const results = await Promise.all(items.map(([p]) => api(p)));
  put("m3", items.map(([, title], i) => {
    const r = results[i];
    return el("div", null, el("h3", { text: title }), r.kind === "ok" ? renderGeneric(r.data) : failNode(r));
  }));
}

// ---------- ham komut gezgini ----------
async function initRaw() {
  const sel = $("raw-cmd");
  try {
    const r = await fetch("/api", { cache: "no-store" });
    const b = await r.json();
    commands = (b && b.data && b.data.commands) || [];
  } catch (_) { commands = []; }
  sel.replaceChildren(...commands.map((c, i) =>
    el("option", { value: String(i), text: c.milestone + " · " + c.title + " — /api/" + c.route + (c.param ? "/<ad>" : "") })));
  const sync = () => {
    const c = commands[Number(sel.value)];
    $("raw-param-l").hidden = !(c && c.param);
    $("raw-query-l").hidden = !(c && c.query && c.query.length);
  };
  sel.addEventListener("change", sync);
  sync();
  $("raw-go").addEventListener("click", async () => {
    const c = commands[Number(sel.value)];
    if (!c) return;
    let path = c.route;
    if (c.param) {
      const v = $("raw-param").value.trim();
      if (!v) { $("raw-out").textContent = "Ad/kimlik gerekli."; return; }
      path += "/" + encodeURIComponent(v);
    }
    if (c.query && c.query.length) {
      const q = $("raw-query").value.trim().replace(/^\?/, "");
      if (q) path += "?" + q;
    }
    const out = $("raw-out");
    out.textContent = "yükleniyor…";
    let r;
    try { r = await fetch("/api/" + path, { cache: "no-store" }); } catch (e) { out.textContent = "Hata: " + e; return; }
    let text;
    try { text = JSON.stringify(await r.json(), null, 2); } catch (_) { text = "(JSON değil)"; }
    out.classList.remove("muted");
    out.textContent = "GET /api/" + path + " → HTTP " + r.status + "\n\n" + text;
  });
}

// ---------- yenileme ----------
async function refreshAll() {
  if (busy) return;
  busy = true;
  try {
    const [st, he, sl, ex, dl, rp] = await Promise.all([
      api("status"), api("health"), api("strategies/list"), api("experiments/summary"), api("data/list"), api("journal/replay"),
    ]);
    renderStatus(st);
    renderHealth(he);
    renderStrategies(sl);
    renderRegistry(ex);
    renderData(dl);
    renderReplay(rp);
    await Promise.all([loadEvents(), loadM3()]);
    if (selectedStrategy) await showStrategy(selectedStrategy);
    $("updated").textContent = "son güncelleme " + new Date().toLocaleTimeString("tr-TR");
  } finally {
    busy = false;
  }
}

document.addEventListener("DOMContentLoaded", () => {
  initTheme();
  initRaw();
  $("refresh").addEventListener("click", refreshAll);
  for (const id of ["ev-prio", "ev-limit"]) $(id).addEventListener("change", loadEvents);
  $("ev-kind").addEventListener("change", loadEvents);
  setInterval(() => { if ($("auto").checked && !document.hidden) refreshAll(); }, REFRESH_MS);
  refreshAll();
});
