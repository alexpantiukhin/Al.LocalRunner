var handlers = {}, sources = [], logText = "", nextData = null, ids = {};
function El(tag, id) {
  var self = this;
  this.tagName = tag.toUpperCase(); this.id = id || ""; this.children = []; this.attrs = {}; this.listeners = {};
  this.style = {}; this.dataset = {}; this.value = ""; this.textContent = ""; this.hidden = false; this.className = "";
  this.checked = false;
  function has(c) { return self.className.split(" ").indexOf(c) >= 0; }
  this.classList = {
    add: function (c) { if (!has(c)) self.className = (self.className + " " + c).trim(); },
    remove: function (c) { self.className = self.className.split(" ").filter(function (x) { return x !== c; }).join(" "); },
    toggle: function (c, on) { if (on) { this.add(c); } else { this.remove(c); } },
    contains: has
  };
}
El.prototype.setAttribute = function (k, v) { this.attrs[k] = String(v); };
El.prototype.getAttribute = function (k) { return this.attrs[k] == null ? "" : this.attrs[k]; };
El.prototype.appendChild = function (c) { this.children.push(c); return c; };
El.prototype.replaceChildren = function () { this.children = Array.prototype.slice.call(arguments); };
El.prototype.addEventListener = function (t, f) { (this.listeners[t] = this.listeners[t] || []).push(f); };
El.prototype.fire = function (t) { var self = this; (this.listeners[t] || []).forEach(function (f) { f({ target: self, preventDefault: function () {} }); }); };
El.prototype.text = function () { return this.textContent + this.children.map(function (c) { return c.text ? c.text() : c.textContent; }).join(""); };
["focus", "showModal", "close", "remove", "click", "blur"].forEach(function (m) { El.prototype[m] = function () {}; });
var TABS = ["queue", "history", "stats", "runners", "brokers"];
globalThis.document = {
  getElementById: function (id) { return ids[id] || (ids[id] = new El("div", id)); },
  querySelector: function () { return new El("meta"); },
  querySelectorAll: function (sel) { return sel === ".tab" ? TABS.map(function (t) { return document.getElementById("tab-" + t); }) : []; },
  createElement: function (t) { return new El(t); },
  createElementNS: function (ns, t) { return new El(t); },
  createTextNode: function (s) { return { textContent: s }; },
  addEventListener: function (t, f) { handlers["doc:" + t] = f; },
  body: new El("body")
};
globalThis.window = { addEventListener: function (t, f) { handlers[t] = f; } };
globalThis.location = { search: "", hash: "" };
globalThis.history = { replaceState: function (a, b, h) { location.hash = h; } };
globalThis.localStorage = { getItem: function () { return null; }, setItem: function () {} };
globalThis.setInterval = function () { return 0; };
globalThis.EventSource = function () { this.addEventListener = function () {}; sources.push(this); };
globalThis.fetch = function (url) {
  return Promise.resolve({
    ok: true,
    json: function () { return Promise.resolve(nextData); },
    text: function () { return Promise.resolve(logText); }
  });
};
function tick() { return new Promise(function (r) { setTimeout(r, 5); }); }
function start(config, data) {
  document.getElementById("lr-config").textContent = JSON.stringify(config);
  document.getElementById("lr-data").textContent = JSON.stringify(data);
  nextData = data;
}
