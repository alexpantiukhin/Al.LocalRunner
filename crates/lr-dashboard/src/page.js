(function () {
  "use strict";
  var $ = function (id) { return document.getElementById(id); };
  var config = JSON.parse($("lr-config").textContent);
  var csrf = document.querySelector('meta[name="lr-csrf"]').getAttribute("content");
  var params = new URLSearchParams(location.search);
  var S = {
    data: JSON.parse($("lr-data").textContent), tab: "queue", sort: { col: "", dir: 1 },
    sel: -1, open: null, paused: false, missed: 0, log: "", as: params.get("as") || ""
  };
  var FILTERS = ["text", "project", "source", "label", "runner", "profile", "state", "period"];
  var FINAL = ["passed", "failed", "timeout", "cancelled", "lost"];

  function el(tag, attrs, kids) {
    var n = document.createElement(tag);
    Object.keys(attrs || {}).forEach(function (k) {
      if (k === "text") { n.textContent = attrs[k]; } else if (k === "cls") { n.className = attrs[k]; } else { n.setAttribute(k, attrs[k]); }
    });
    (kids || []).forEach(function (c) { if (c != null) n.appendChild(typeof c === "string" ? document.createTextNode(c) : c); });
    return n;
  }
  function svg(tag, attrs) {
    var n = document.createElementNS("http://www.w3.org/2000/svg", tag);
    Object.keys(attrs).forEach(function (k) { if (k === "text") { n.textContent = attrs[k]; } else { n.setAttribute(k, attrs[k]); } });
    return n;
  }
  function now() { return config.live ? Date.now() : S.data.taken_at; }
  function dur(ms) {
    if (ms == null) return "";
    var s = Math.round(ms / 1000);
    if (s < 60) return s + "s";
    if (s < 3600) return Math.floor(s / 60) + "m" + (s % 60) + "s";
    return Math.floor(s / 3600) + "h" + Math.floor(s % 3600 / 60) + "m";
  }
  function when(ms) { return ms ? new Date(ms).toLocaleString() : ""; }
  function store(key, value) { try { localStorage.setItem("lr." + key, value); } catch (e) { return; } }
  function load(key) { try { return localStorage.getItem("lr." + key); } catch (e) { return null; } }

  function row(j) {
    var d = j.detail, s = d.summary, steps = d.steps || [];
    var budget = steps.reduce(function (a, st) { return a + st.timeout_s * 1000; }, 0);
    var t = now();
    var info = j.list === "running" && s.started_at
      ? dur(t - s.started_at) + (budget ? " / " + dur(budget) : "")
      : (d.wait_reason || "") + (s.started_at ? "" : " (" + dur(t - s.queued_at) + ")");
    return {
      j: j, position: j.position || "", job: s.job, state: s.state, project: s.project || "",
      source: d.source || "", label: s.label || "", profile: s.profile || "", cwd: d.cwd || "",
      runner: s.runner || "", broker: j.broker || "", wait: info, list: j.list,
      queued: s.queued_at, duration: s.finished_at && s.started_at ? s.finished_at - s.started_at : null
    };
  }
  function matches(r) {
    var f = S.filters, t = now();
    function has(v, q) { return !q || String(v).toLowerCase().indexOf(q.toLowerCase()) >= 0; }
    if (f.source && r.source !== f.source) return false;
    if (f.period && r.queued < t - Number(f.period) * 1000) return false;
    if (!has(r.project, f.project) || !has(r.label, f.label) || !has(r.runner, f.runner)) return false;
    if (!has(r.profile, f.profile) || !has(r.state, f.state)) return false;
    if (f.text) {
      var all = [r.job, r.project, r.label, r.profile, r.cwd, r.runner, r.broker, r.wait, r.j.detail.note || "", r.j.detail.branch || ""].join(" ");
      if (!has(all, f.text)) return false;
    }
    return true;
  }
  function sorted(rows) {
    var c = S.sort.col, d = S.sort.dir;
    rows.sort(function (a, b) {
      var pa = S.as && a.label === S.as ? 0 : 1, pb = S.as && b.label === S.as ? 0 : 1;
      if (pa !== pb) return pa - pb;
      if (!c) return 0;
      var x = a[c], y = b[c];
      if (typeof x === "number" && typeof y === "number") return (x - y) * d;
      return String(x).localeCompare(String(y), undefined, { numeric: true }) * d;
    });
    return rows;
  }
  function visible() {
    var lists = S.tab === "history" ? ["history"] : ["running", "queued", "outbox"];
    var rows = S.data.jobs.filter(function (j) { return lists.indexOf(j.list) >= 0; }).map(row).filter(matches);
    if (S.tab === "queue" && !S.sort.col) {
      rows.sort(function (a, b) { return lists.indexOf(a.list) - lists.indexOf(b.list) || (a.position || 0) - (b.position || 0); });
    }
    return sorted(rows);
  }

  function renderTable(body, cols) {
    var rows = visible();
    S.rows = rows;
    body.replaceChildren.apply(body, rows.map(function (r, i) {
      var tr = el("tr", { "data-job": r.job, tabindex: "-1" }, cols.map(function (c) {
        var v = c === "queued" ? when(r.queued) : c === "duration" ? dur(r.duration) : String(r[c]);
        var td = el("td", { "data-col": c, text: v });
        if (c === "state") td.className = "state-" + r.state;
        if (c === "source" && r.j.detail.github_run_url && /^https:\/\//.test(r.j.detail.github_run_url)) {
          td.replaceChildren(el("a", { href: r.j.detail.github_run_url, rel: "noreferrer noopener", target: "_blank", text: v }));
        }
        return td;
      }));
      if (S.as && r.label === S.as) tr.classList.add("pinned");
      if (r.j.stale) tr.classList.add("stale");
      if (i === S.sel) tr.classList.add("sel");
      tr.addEventListener("click", function () { S.sel = i; openCard(r.j); });
      return tr;
    }));
  }
  var QCOLS = ["position", "job", "state", "project", "source", "label", "profile", "cwd", "runner", "broker", "wait"];
  var HCOLS = ["job", "state", "project", "source", "label", "profile", "runner", "queued", "duration"];

  function bars(title, items) {
    var w = 320, h = 18 * items.length + 20, max = Math.max.apply(null, items.map(function (i) { return i.v || 0; }).concat([1]));
    var g = svg("svg", { "class": "chart", width: w, height: h, role: "img", "aria-label": title });
    g.appendChild(svg("text", { x: 0, y: 12, text: title }));
    items.forEach(function (it, i) {
      var y = 18 + i * 18;
      g.appendChild(svg("rect", { "class": "bg", x: 90, y: y, width: 180, height: 12 }));
      g.appendChild(svg("rect", { "class": it.cls, x: 90, y: y, width: Math.round(180 * (it.v || 0) / max), height: 12 }));
      g.appendChild(svg("text", { x: 0, y: y + 10, text: it.k }));
      g.appendChild(svg("text", { x: 275, y: y + 10, text: it.t }));
    });
    return g;
  }
  function renderStats() {
    var root = $("tab-stats"), st = S.data.stats || { profiles: [], clients: [] };
    var want = (S.filters.profile || "").toLowerCase();
    var kids = st.profiles.filter(function (p) { return p.profile.toLowerCase().indexOf(want) >= 0; }).map(function (p) {
      var rate = p.runs ? Math.round(100 * p.passed / p.runs) : 0;
      var steps = el("table", {}, [el("tr", {}, ["Step", "Runs", "Passed", "Median", "p95"].map(function (h) { return el("th", { text: h }); }))].concat(
        p.steps.map(function (s) { return el("tr", {}, [s.name, s.runs, s.passed, dur(s.duration_ms.median), dur(s.duration_ms.p95)].map(function (v) { return el("td", { text: String(v) }); })); })));
      return el("div", { cls: "group" }, [
        el("h2", { text: p.profile + " — " + rate + "% passed of " + p.runs }),
        bars("Results", [{ k: "passed", v: p.passed, cls: "ok", t: String(p.passed) }, { k: "failed", v: p.failed, cls: "bad", t: String(p.failed) },
          { k: "timeout", v: p.timeouts, cls: "bad", t: String(p.timeouts) }, { k: "cancelled", v: p.cancelled, cls: "p", t: String(p.cancelled) }, { k: "lost", v: p.lost, cls: "bad", t: String(p.lost) }]),
        bars("Wait / duration", [{ k: "wait p50", v: p.wait_ms.median, cls: "m", t: dur(p.wait_ms.median) }, { k: "wait p95", v: p.wait_ms.p95, cls: "p", t: dur(p.wait_ms.p95) },
          { k: "run p50", v: p.duration_ms.median, cls: "m", t: dur(p.duration_ms.median) }, { k: "run p95", v: p.duration_ms.p95, cls: "p", t: dur(p.duration_ms.p95) }]),
        el("div", { cls: "scroll" }, [steps])
      ]);
    });
    if (st.clients.length) {
      kids.push(el("div", { cls: "group" }, [bars("Pool time by client", st.clients.map(function (c) { return { k: c.label, v: c.busy_ms, cls: "m", t: dur(c.busy_ms) + " / " + c.runs }; }))]));
    }
    root.replaceChildren.apply(root, kids.length ? kids : [el("p", { text: "No statistics yet." })]);
  }
  function meter(used, total, unit) {
    var g = svg("svg", { "class": "chart meter", width: 140, height: 14, role: "img", "aria-label": used + " of " + total + " " + unit });
    g.appendChild(svg("rect", { "class": "bg", x: 0, y: 1, width: 80, height: 12 }));
    g.appendChild(svg("rect", { "class": "m", x: 0, y: 1, width: total ? Math.round(80 * Math.min(used, total) / total) : 0, height: 12 }));
    g.appendChild(svg("text", { x: 84, y: 11, text: used + "/" + total + unit }));
    return g;
  }
  function renderRunners() {
    var root = $("tab-runners");
    var kids = (S.data.runners || []).map(function (b) {
      var p = b.pool;
      var t = el("table", {}, [el("tr", {}, ["Runner", "State", "Platforms", "Free mem", "Free CPU", "Tags held", "Services", "Unmet"].map(function (h) { return el("th", { text: h }); }))]);
      b.runners.filter(function (r) { return !S.filters.runner || r.name.indexOf(S.filters.runner) >= 0; }).forEach(function (r) {
        t.appendChild(el("tr", { cls: b.stale ? "stale" : "" }, [
          el("td", { text: r.name }),
          el("td", { text: (r.online ? "online" : "offline") + (r.last_seen ? " · " + when(r.last_seen) : "") }),
          el("td", { text: (r.caps.platforms || []).join(", ") }),
          el("td", {}, [meter(r.free_mem_mb, r.caps.mem_mb, "MB")]),
          el("td", {}, [meter(r.free_cpu, r.caps.cpu, "")]),
          el("td", { text: r.held_tags.map(function (h) { return h.tag + (h.exclusive ? "!" : "") + "←" + h.job; }).join(", ") }),
          el("td", { text: r.services.map(function (s) { return s.name + ":" + s.state; }).join(", ") }),
          el("td", { text: r.unmet.join(", ") })
        ]));
      });
      return el("div", { cls: "group" + (b.stale ? " stale" : "") }, [
        el("h2", { text: b.broker + (b.stale ? " (stale)" : "") }),
        el("p", {}, ["Pool memory ", meter(p.used_mem_mb, p.total_mem_mb, "MB"), " CPU ", meter(p.used_cpu, p.total_cpu, "")]),
        el("div", { cls: "scroll" }, [t])
      ]);
    });
    root.replaceChildren.apply(root, kids.length ? kids : [el("p", { text: "No runners known." })]);
  }
  function renderBrokers() {
    var root = $("tab-brokers");
    var t = el("table", {}, [el("tr", {}, ["Broker", "Zone", "Reachable", "Orders in outbox"].map(function (h) { return el("th", { text: h }); }))]);
    (S.data.brokers || []).forEach(function (b) {
      t.appendChild(el("tr", { cls: b.available ? "" : "stale" }, [b.name, b.zone || "", b.available ? "yes" : "no (data stale)", String(b.outbox)].map(function (v) { return el("td", { text: v }); })));
    });
    root.replaceChildren(el("div", { cls: "scroll" }, [t]));
  }

  function render() {
    document.querySelectorAll(".tabs button").forEach(function (b) { b.setAttribute("aria-selected", String(b.dataset.tab === S.tab)); });
    document.querySelectorAll(".tab").forEach(function (t) { t.hidden = t.id !== "tab-" + S.tab; });
    document.querySelectorAll("th[data-sort]").forEach(function (th) {
      th.classList.toggle("asc", th.dataset.sort === S.sort.col && S.sort.dir > 0);
      th.classList.toggle("desc", th.dataset.sort === S.sort.col && S.sort.dir < 0);
    });
    if (S.tab === "queue") renderTable($("queue-body"), QCOLS);
    if (S.tab === "history") renderTable($("history-body"), HCOLS);
    if (S.tab === "stats") renderStats();
    if (S.tab === "runners") renderRunners();
    if (S.tab === "brokers") renderBrokers();
    if (S.open) {
      var fresh = S.data.jobs.filter(function (j) { return j.detail.summary.job === S.open.detail.summary.job; })[0];
      if (fresh) fillCard(fresh);
    }
  }

  function saveState() {
    var h = new URLSearchParams();
    h.set("tab", S.tab);
    FILTERS.forEach(function (k) { if (S.filters[k]) h.set(k, S.filters[k]); });
    if (S.sort.col) h.set("sort", (S.sort.dir < 0 ? "-" : "") + S.sort.col);
    history.replaceState(null, "", "#" + h.toString());
    store("filters", JSON.stringify(S.filters));
  }
  function restoreState() {
    var h = new URLSearchParams(location.hash.slice(1)), saved = {};
    try { saved = JSON.parse(load("filters") || "{}") || {}; } catch (e) { saved = {}; }
    var fromHash = FILTERS.some(function (k) { return h.has(k); });
    S.filters = {};
    FILTERS.forEach(function (k) { S.filters[k] = (fromHash ? h.get(k) : saved[k]) || ""; $("f-" + k).value = S.filters[k]; });
    S.tab = h.get("tab") || "queue";
    var sort = h.get("sort") || "";
    S.sort = { col: sort.replace(/^-/, ""), dir: sort.charAt(0) === "-" ? -1 : 1 };
  }

  function fillCard(j) {
    S.open = j;
    var d = j.detail, s = d.summary, t = now();
    $("card-title").textContent = s.job + " · " + s.state + (j.stale ? " (stale)" : "");
    var f = [["Project", s.project], ["Source", d.source], ["Run", d.github_run_url], ["Label", s.label], ["Client", s.client_label], ["Note", d.note],
      ["Branch", d.branch], ["Commit", d.commit], ["Directory", d.cwd], ["Profile", s.profile], ["Platform", s.platform], ["Runner", s.runner],
      ["Broker", j.broker], ["Waiting because", d.wait_reason], ["Queued", when(s.queued_at)], ["Exit code", s.exit_code], ["Reason", s.reason],
      ["Cancelled by", d.cancelled_by], ["Secrets", (d.secrets || []).join(", ")]];
    var dl = $("card-fields");
    dl.replaceChildren();
    f.forEach(function (p) { if (p[1] != null && p[1] !== "") { dl.appendChild(el("dt", { text: p[0] })); dl.appendChild(el("dd", { text: String(p[1]) })); } });
    var steps = $("card-steps");
    steps.replaceChildren.apply(steps, (d.steps || []).map(function (st) {
      var spent = st.started_at ? (st.finished_at || t) - st.started_at : 0;
      var fill = el("div", { cls: "fill " + st.state });
      fill.style.width = Math.min(100, st.timeout_s ? 100 * spent / (st.timeout_s * 1000) : 0) + "%";
      return el("div", { cls: "step" }, [el("span", { text: st.name + " · " + st.state }), el("div", { cls: "track" }, [fill]), el("span", { text: dur(spent) + " / " + dur(st.timeout_s * 1000) })]);
    }));
    $("card-cancel").hidden = !config.live || FINAL.indexOf(s.state) >= 0;
    $("card").hidden = false;
  }
  function openCard(j) {
    fillCard(j);
    S.log = (j.last_lines || []).join("\n");
    showLog();
    loadLog();
  }
  function showLog() {
    var q = $("log-search").value.toLowerCase();
    var text = q ? S.log.split("\n").filter(function (l) { return l.toLowerCase().indexOf(q) >= 0; }).join("\n") : S.log;
    var pre = $("log");
    pre.textContent = text;
    if ($("log-follow").checked && !q) pre.scrollTop = pre.scrollHeight;
  }
  function loadLog() {
    if (!S.open) return;
    var id = S.open.detail.summary.job;
    if (!config.live) {
      S.log = config.logs[id] != null ? config.logs[id] : config.with_logs ? "(no log)" : "(logs are not included in this snapshot)";
      showLog();
      return;
    }
    fetch("/api/log/" + encodeURIComponent(id), { credentials: "same-origin" }).then(function (r) { return r.ok ? r.text() : ""; })
      .then(function (t) { if (S.open && S.open.detail.summary.job === id && t) { S.log = t; showLog(); } }).catch(function () { return; });
  }
  function closeCard() { $("card").hidden = true; S.open = null; }

  function askCancel(jobs) {
    if (!jobs.length) return;
    S.cancelJobs = jobs;
    var list = $("cancel-list");
    list.replaceChildren.apply(list, jobs.map(function (j) { var s = j.detail.summary; return el("li", { text: s.job + " — " + (s.project || "") + " " + (s.label || "") + " (" + s.state + ")" }); }));
    $("cancel-error").textContent = "";
    $("cancel-reason").value = "";
    $("cancel-dialog").showModal();
  }
  function post(path, body) {
    return fetch(path, { method: "POST", credentials: "same-origin", headers: { "content-type": "application/json", "x-lr-csrf": csrf }, body: JSON.stringify(body) });
  }
  function doCancel() {
    var ids = S.cancelJobs.map(function (j) { return j.detail.summary.job; }), reason = $("cancel-reason").value || null;
    var req = ids.length === 1 ? post("/api/cancel", { job: ids[0], reason: reason }) : post("/api/cancel-many", { jobs: ids, reason: reason });
    req.then(function (r) {
      if (r.ok) { $("cancel-dialog").close(); refresh(); return null; }
      return r.text().then(function (t) { $("cancel-error").textContent = r.status + " " + t; });
    }).catch(function (e) { $("cancel-error").textContent = String(e); });
  }

  function exportRows(kind) {
    var rows = S.tab === "queue" || S.tab === "history" ? visible().map(function (r) { return r.j; }) : S.tab === "stats" ? [S.data.stats] : S.tab === "runners" ? S.data.runners : S.data.brokers;
    var text, type;
    if (kind === "json") { text = JSON.stringify(rows, null, 2); type = "application/json"; } else {
      var cols = S.tab === "history" ? HCOLS : QCOLS;
      var flat = S.tab === "queue" || S.tab === "history" ? visible() : rows.map(function (r) { return { json: JSON.stringify(r) }; });
      if (!(S.tab === "queue" || S.tab === "history")) cols = ["json"];
      var q = function (v) { v = v == null ? "" : String(v); return /[",\n]/.test(v) || /^[=+\-@]/.test(v) ? '"' + v.replace(/"/g, '""') + '"' : v; };
      text = [cols.join(",")].concat(flat.map(function (r) { return cols.map(function (c) { return q(r[c]); }).join(","); })).join("\n");
      type = "text/csv";
    }
    download(S.tab + "." + kind, text, type);
  }
  function download(name, text, type) {
    var a = el("a", { download: name, href: URL.createObjectURL(new Blob([text], { type: type })) });
    document.body.appendChild(a); a.click(); a.remove();
    setTimeout(function () { URL.revokeObjectURL(a.href); }, 1000);
  }

  var timer = null;
  function refresh() {
    if (!config.live) return;
    fetch("/api/snapshot", { credentials: "same-origin" }).then(function (r) { return r.ok ? r.json() : null; })
      .then(function (d) { if (d && !S.paused) { S.data = d; render(); } }).catch(function () { return; });
    if (S.open && $("log-follow").checked) loadLog();
  }
  function schedule() { if (!timer) timer = setTimeout(function () { timer = null; refresh(); }, 300); }
  function connect() {
    var conn = $("conn");
    var es = new EventSource("/api/events");
    es.onopen = function () { conn.textContent = "live"; conn.className = "conn live"; refresh(); };
    es.onerror = function () { conn.textContent = "reconnecting…"; conn.className = "conn down"; };
    es.addEventListener("update", function () {
      if (S.paused) { S.missed += 1; $("pause").textContent = "Resume (" + S.missed + " new)"; } else { schedule(); }
    });
  }

  function move(delta) {
    var n = (S.rows || []).length;
    if (!n) return;
    S.sel = Math.max(0, Math.min(n - 1, S.sel + delta));
    render();
    var tr = document.querySelector("tbody tr.sel");
    if (tr) tr.focus();
  }
  var pendingG = false;
  document.addEventListener("keydown", function (e) {
    var typing = /^(INPUT|SELECT|TEXTAREA)$/.test(e.target.tagName);
    if (e.key === "Escape") { if (typing) e.target.blur(); closeCard(); return; }
    if (typing || e.ctrlKey || e.metaKey || e.altKey) return;
    if (pendingG) {
      pendingG = false;
      var tab = { q: "queue", h: "history", s: "stats", r: "runners", b: "brokers" }[e.key];
      if (tab) { S.tab = tab; S.sel = -1; saveState(); render(); }
      return;
    }
    if (e.key === "/") { e.preventDefault(); $("f-text").focus(); } else if (e.key === "j") { move(1); } else if (e.key === "k") { move(-1); } else if (e.key === "g") { pendingG = true; } else if (e.key === "?") { $("help-dialog").showModal(); } else if (e.key === "Enter" && S.rows && S.rows[S.sel]) { openCard(S.rows[S.sel].j); }
  });

  document.querySelectorAll(".tabs button").forEach(function (b) { b.addEventListener("click", function () { S.tab = b.dataset.tab; S.sel = -1; saveState(); render(); }); });
  document.querySelectorAll("th[data-sort]").forEach(function (th) {
    th.addEventListener("click", function () { var c = th.dataset.sort; S.sort = { col: c, dir: S.sort.col === c ? -S.sort.dir : 1 }; saveState(); render(); });
  });
  FILTERS.forEach(function (k) { $("f-" + k).addEventListener("input", function (e) { S.filters[k] = e.target.value; S.sel = -1; saveState(); render(); }); });
  $("card-close").addEventListener("click", closeCard);
  $("card-cancel").addEventListener("click", function () { if (S.open) askCancel([S.open]); });
  $("cancel-filtered").addEventListener("click", function () {
    var lists = ["running", "queued", "outbox"];
    askCancel(S.data.jobs.filter(function (j) { return lists.indexOf(j.list) >= 0 && FINAL.indexOf(j.detail.summary.state) < 0; }).map(row).filter(matches).map(function (r) { return r.j; }));
  });
  $("cancel-no").addEventListener("click", function () { $("cancel-dialog").close(); });
  $("cancel-yes").addEventListener("click", doCancel);
  $("help").addEventListener("click", function () { $("help-dialog").showModal(); });
  $("help-close").addEventListener("click", function () { $("help-dialog").close(); });
  $("log-search").addEventListener("input", showLog);
  $("log-copy").addEventListener("click", function () { if (navigator.clipboard) navigator.clipboard.writeText($("log").textContent); });
  $("log-download").addEventListener("click", function () { if (S.open) download(S.open.detail.summary.job + ".log", S.log, "text/plain"); });
  $("export-csv").addEventListener("click", function () { exportRows("csv"); });
  $("export-json").addEventListener("click", function () { exportRows("json"); });
  $("pause").addEventListener("click", function () {
    S.paused = !S.paused;
    $("pause").textContent = S.paused ? "Resume" : "Pause";
    if (!S.paused) { S.missed = 0; refresh(); }
  });

  if (!config.live) document.querySelectorAll(".live-only").forEach(function (n) { n.classList.add("hidden-mode"); });
  restoreState();
  render();
  if (config.live) { connect(); setInterval(function () { if (S.open && $("log-follow").checked && !S.paused) loadLog(); }, 2000); }
}());
