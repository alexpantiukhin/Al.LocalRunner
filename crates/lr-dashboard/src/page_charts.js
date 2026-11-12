var LRCharts = (function () {
  "use strict";
  var NS = "http://www.w3.org/2000/svg";
  var WAITS = {
    runner_busy: "runner busy", memory: "memory", cpu: "CPU", tag: "exclusive tag",
    no_runner: "no runner", offline: "runners offline", rule: "policy rule", reserved: "start reserved"
  };
  function svg(tag, attrs) {
    var n = document.createElementNS(NS, tag);
    Object.keys(attrs).forEach(function (k) { if (k === "text") { n.textContent = attrs[k]; } else { n.setAttribute(k, attrs[k]); } });
    return n;
  }
  function clip(text, n) { text = text == null ? "" : String(text); return text.length > n ? text.slice(0, n - 1) + "…" : text; }
  function label(x, y, text, n) {
    var t = svg("text", { x: x, y: y, text: clip(text, n) });
    t.appendChild(svg("title", { text: String(text == null ? "" : text) }));
    return t;
  }
  function bars(title, items) {
    var lw = 150, bw = 200, w = lw + bw + 110, h = 18 * items.length + 20;
    var max = Math.max.apply(null, items.map(function (i) { return i.v || 0; }).concat([1]));
    var g = svg("svg", { "class": "chart", width: w, height: h, viewBox: "0 0 " + w + " " + h, role: "img", "aria-label": title });
    g.appendChild(svg("text", { x: 0, y: 12, text: title }));
    items.forEach(function (it, i) {
      var y = 18 + i * 18;
      g.appendChild(svg("rect", { "class": "bg", x: lw, y: y, width: bw, height: 12 }));
      g.appendChild(svg("rect", { "class": it.cls, x: lw, y: y, width: Math.round(bw * (it.v || 0) / max), height: 12 }));
      g.appendChild(label(0, y + 10, it.k, 24));
      g.appendChild(label(lw + bw + 6, y + 10, it.t, 18));
    });
    return g;
  }
  function meter(used, total, unit) {
    var text = used + "/" + total + unit;
    var g = svg("svg", { "class": "chart meter", width: 90 + 7 * text.length, height: 14, role: "img", "aria-label": used + " of " + total + " " + unit });
    g.appendChild(svg("rect", { "class": "bg", x: 0, y: 1, width: 80, height: 12 }));
    g.appendChild(svg("rect", { "class": "m", x: 0, y: 1, width: total ? Math.round(80 * Math.min(used, total) / total) : 0, height: 12 }));
    g.appendChild(svg("text", { x: 86, y: 11, text: text }));
    return g;
  }
  function cls(state) { return state === "passed" ? "ok" : state === "running" || state === "cancelling" ? "m" : state === "cancelled" ? "p" : "bad"; }
  function lanes(jobs, now, span) {
    var from = now - span, lw = 120, bw = 480, w = lw + bw + 10, rows = {}, names = [], waits = [];
    jobs.forEach(function (j) {
      var s = j.detail.summary;
      if (s.queued_at < now && (s.started_at || now) > from) waits.push([s.queued_at, s.started_at || now]);
      if (!s.runner || !s.started_at || (s.finished_at || now) < from) return;
      if (!rows[s.runner]) { rows[s.runner] = []; names.push(s.runner); }
      rows[s.runner].push(s);
    });
    names.sort();
    var h = 20 * (names.length + 1) + 34;
    var g = svg("svg", { "class": "chart lanes", width: w, height: h, viewBox: "0 0 " + w + " " + h, role: "img", "aria-label": "Runner lanes" });
    g.appendChild(svg("text", { x: 0, y: 12, text: "Runners over the last " + Math.round(span / 3600000) + "h (gaps are idle)" }));
    function x(t) { return lw + Math.round(bw * (Math.max(from, Math.min(now, t)) - from) / span); }
    function lane(i, name, spans) {
      var y = 20 + i * 20;
      g.appendChild(svg("rect", { "class": "bg", x: lw, y: y, width: bw, height: 14 }));
      g.appendChild(label(0, y + 11, name, 18));
      spans.forEach(function (p) {
        var r = svg("rect", { "class": p.cls, x: x(p.a), y: y, width: Math.max(1, x(p.b) - x(p.a)), height: 14 });
        r.appendChild(svg("title", { text: p.title }));
        g.appendChild(r);
      });
    }
    names.forEach(function (n, i) {
      lane(i, n, rows[n].map(function (s) {
        return { a: s.started_at, b: s.finished_at || now, cls: cls(s.state), title: s.job + " · " + s.client_label + " · " + s.state };
      }));
    });
    lane(names.length, "waiting", waits.map(function (p) { return { a: p[0], b: p[1], cls: "p", title: "orders waiting" }; }));
    var axis = 20 + (names.length + 1) * 20 + 10;
    for (var k = 0; k <= 4; k += 1) {
      g.appendChild(svg("text", { x: lw + Math.round(bw * k / 4) - (k === 4 ? 20 : 0), y: axis, text: k === 4 ? "now" : "-" + Math.round(span * (4 - k) / 4 / 3600000) + "h" }));
    }
    return g;
  }
  function saturation(jobs, now) {
    var by = {};
    jobs.forEach(function (j) {
      if (j.list !== "queued" || !j.wait_code) return;
      var b = by[j.wait_code] || (by[j.wait_code] = { jobs: 0, ms: 0 });
      b.jobs += 1;
      b.ms += Math.max(0, now - j.detail.summary.queued_at);
    });
    return Object.keys(by).sort().map(function (k) { return { code: k, name: WAITS[k] || k, jobs: by[k].jobs, ms: by[k].ms }; });
  }
  return { bars: bars, meter: meter, lanes: lanes, saturation: saturation, clip: clip };
}());
