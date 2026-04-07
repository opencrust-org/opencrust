import { initIntegrationsView } from '/assets/webchat/integrations/main.js';

const chatEl = document.getElementById("chat");
const inputEl = document.getElementById("input");
const sendBtn = document.getElementById("send");
const reconnectBtn = document.getElementById("reconnect");
const refreshBtn = document.getElementById("refresh");
const clearBtn = document.getElementById("clear");
const connPill = document.getElementById("conn-pill");
const themeToggleBtn = document.getElementById("theme-toggle");
const sessionEl = document.getElementById("session");
const apiStatusEl = document.getElementById("api-status");
const sessionCountEl = document.getElementById("session-count");
const channelListEl = document.getElementById("channel-list");
const providerSelect = document.getElementById("provider-select");
const providerStatus = document.getElementById("provider-status");
const providerModelInput = document.getElementById("provider-model-input");
const providerBaseUrlInput = document.getElementById("provider-base-url-input");
const providerModelOptions = document.getElementById("provider-model-options");
const providerModelStatus = document.getElementById("provider-model-status");
const providerKeySection = document.getElementById("provider-key-section");
const providerApiKey = document.getElementById("provider-api-key");
const providerActivateBtn = document.getElementById("provider-activate");
const authSection = document.getElementById("auth-section");
const keyGatewayEl = document.getElementById("key-gateway");
const authConnectBtn = document.getElementById("auth-connect");
const updateBanner = document.getElementById("update-banner");
const updateBannerText = document.getElementById("update-banner-text");
const updateBannerClose = document.getElementById("update-banner-close");

const uploadBtn = document.getElementById("upload-btn");
const fileInput = document.getElementById("file-input");
const filePendingBar = document.getElementById("file-pending-bar");
const filePendingName = document.getElementById("file-pending-name");
const filePendingClear = document.getElementById("file-pending-clear");

const storageKey = "opencrust.session_id";
const keyStorage = "opencrust.gateway_key";
const providerStorage = "opencrust.provider";
const providerModelStorage = "opencrust.provider_models";
const themeStorageKey = "opencrust.ui.theme";
let sessionId = localStorage.getItem(storageKey) || "";
// Prefer the key injected by the server at page-load time; fall back to a
// previously saved value so existing deployments without server injection
// continue to work.
let gatewayKey =
  window.__OPENCRUST_GATEWAY_KEY__ || localStorage.getItem(keyStorage) || "";
let selectedProvider = localStorage.getItem(providerStorage) || "";
let authRequired = false;
let socket = null;
let reconnectTimer = null;
let providerData = [];
let selectedModelsByProvider = {};
let integrationsView = null;
let nanoTimerInterval = null;
let nanoElapsed = 0;
let thinkingTimeout = null;
let pendingFilename = null;

// Pre-fill saved key
keyGatewayEl.value = gatewayKey;
try {
  const rawModels = localStorage.getItem(providerModelStorage);
  if (rawModels) {
    const parsed = JSON.parse(rawModels);
    if (parsed && typeof parsed === "object") {
      selectedModelsByProvider = parsed;
    }
  }
} catch {
  selectedModelsByProvider = {};
}

function saveProviderModels() {
  localStorage.setItem(providerModelStorage, JSON.stringify(selectedModelsByProvider));
}

function setSavedModelForProvider(providerId, model) {
  if (!providerId) return;
  if (model) {
    selectedModelsByProvider[providerId] = model;
  } else {
    delete selectedModelsByProvider[providerId];
  }
  saveProviderModels();
}

function getSavedModelForProvider(providerId) {
  if (!providerId) return "";
  const value = selectedModelsByProvider[providerId];
  return typeof value === "string" ? value : "";
}

function setTheme(theme, persist = true) {
  const selected = theme === "dark" ? "dark" : "light";
  document.documentElement.setAttribute("data-theme", selected);
  if (themeToggleBtn) {
    themeToggleBtn.textContent = selected === "dark" ? "Light Mode" : "Dark Mode";
  }
  if (persist) {
    localStorage.setItem(themeStorageKey, selected);
  }
}

function initTheme() {
  const stored = localStorage.getItem(themeStorageKey);
  if (stored === "light" || stored === "dark") {
    setTheme(stored, false);
    return;
  }
  const prefersDark = window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
  setTheme(prefersDark ? "dark" : "light");
}

const fontSizeStorageKey = "opencrust.ui.font_size";
// Root font-size values — all rem-based CSS scales with this
const fontSizes = { sm: "14px", md: "18px", lg: "22px" };

function setFontSize(size, persist = true) {
  document.documentElement.style.fontSize = fontSizes[size] || fontSizes.md;
  document.querySelectorAll(".font-size-btn").forEach((btn) => btn.classList.remove("active"));
  const btn = document.getElementById(`font-size-${size}`);
  if (btn) btn.classList.add("active");
  if (persist) localStorage.setItem(fontSizeStorageKey, size);
}

function initFontSize() {
  const stored = localStorage.getItem(fontSizeStorageKey);
  setFontSize(stored && fontSizes[stored] ? stored : "md", false);
}

document.getElementById("font-size-sm").addEventListener("click", () => setFontSize("sm"));
document.getElementById("font-size-md").addEventListener("click", () => setFontSize("md"));
document.getElementById("font-size-lg").addEventListener("click", () => setFontSize("lg"));

// Sidebar drawer (mobile)
const sidebarEl = document.querySelector(".left-sidebar");
const sidebarBackdrop = document.getElementById("sidebar-backdrop");
const sidebarToggleBtn = document.getElementById("sidebar-toggle");

function openSidebar() {
  sidebarEl.classList.add("open");
  sidebarBackdrop.classList.add("open");
}

function closeSidebar() {
  sidebarEl.classList.remove("open");
  sidebarBackdrop.classList.remove("open");
}

sidebarToggleBtn.addEventListener("click", () => {
  sidebarEl.classList.contains("open") ? closeSidebar() : openSidebar();
});
sidebarBackdrop.addEventListener("click", closeSidebar);

// Close sidebar when a nav item is tapped on mobile
document.querySelectorAll(".nav-item").forEach((item) => {
  item.addEventListener("click", () => {
    if (window.innerWidth <= 768) closeSidebar();
  });
});

function wsUrl() {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const base = `${proto}//${location.host}/ws`;
  const key = gatewayKey || keyGatewayEl.value.trim();
  return key ? `${base}?token=${encodeURIComponent(key)}` : base;
}

function setConnectionState(isConnected) {
  connPill.innerHTML = isConnected
    ? '<span class="online">Connected</span>'
    : '<span class="offline">Disconnected</span>';
}
function initIntegrationsViewShell() {
  integrationsView = initIntegrationsView({
    onSystemMessage: (message) => appendMessage("sys", message),
    onErrorMessage: (message) => appendMessage("error", message),
  });
}

function setSession(id) {
  sessionId = id || "";
  if (sessionId) {
    localStorage.setItem(storageKey, sessionId);
    sessionEl.textContent = sessionId.slice(0, 8) + "...";
    sessionEl.title = sessionId;
  } else {
    localStorage.removeItem(storageKey);
    sessionEl.textContent = "none";
    sessionEl.title = "";
  }
}

function escapeHtml(value) {
  return String(value)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function sanitizeUrl(rawUrl) {
  const candidate = String(rawUrl || "").trim();
  if (!candidate) return null;
  if (candidate.startsWith("/") || candidate.startsWith("#")) {
    return candidate;
  }
  try {
    const parsed = new URL(candidate, window.location.origin);
    if (["http:", "https:", "mailto:"].includes(parsed.protocol)) {
      return parsed.href;
    }
  } catch {
    // Ignore invalid URLs.
  }
  return null;
}

function renderInlineMarkdown(input) {
  const replacements = [];
  let text = String(input || "");

  text = text.replace(/`([^`]+)`/g, (_, code) => {
    const idx = replacements.push(`<code>${escapeHtml(code)}</code>`) - 1;
    return `\u0000${idx}\u0000`;
  });

  text = text.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, label, href) => {
    const safeHref = sanitizeUrl(href);
    const safeLabel = escapeHtml(label);
    const html = safeHref
      ? `<a href="${escapeHtml(safeHref)}" target="_blank" rel="noopener noreferrer">${safeLabel}</a>`
      : safeLabel;
    const idx = replacements.push(html) - 1;
    return `\u0000${idx}\u0000`;
  });

  text = escapeHtml(text);
  text = text.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  text = text.replace(/__([^_]+)__/g, "<strong>$1</strong>");
  text = text.replace(/\*([^*]+)\*/g, "<em>$1</em>");
  text = text.replace(/_([^_]+)_/g, "<em>$1</em>");
  text = text.replace(/~~([^~]+)~~/g, "<del>$1</del>");

  return text.replace(/\u0000(\d+)\u0000/g, (_, idx) => replacements[Number(idx)] || "");
}

function renderMarkdown(text) {
  const normalized = String(text || "").replace(/\r\n/g, "\n");
  if (!normalized.trim()) return "";

  const codeBlocks = [];
  const withCodeTokens = normalized.replace(/```([\w-]+)?\n([\s\S]*?)```/g, (_, lang = "", code) => {
    const language = String(lang).trim();
    const classAttr = language ? ` class="language-${escapeHtml(language)}"` : "";
    const codeHtml = `<pre><code${classAttr}>${escapeHtml(code.replace(/\n$/, ""))}</code></pre>`;
    const idx = codeBlocks.push(codeHtml) - 1;
    return `@@CODEBLOCK_${idx}@@`;
  });

  const parseTableCells = (line) => {
    const raw = String(line || "").trim();
    if (!raw.includes("|")) return null;
    let row = raw;
    if (row.startsWith("|")) row = row.slice(1);
    if (row.endsWith("|")) row = row.slice(0, -1);
    const cells = row.split("|").map((cell) => cell.trim());
    return cells.length ? cells : null;
  };

  const isTableDelimiterRow = (line) => {
    const cells = parseTableCells(line);
    if (!cells || cells.length === 0) return false;
    return cells.every((cell) => /^:?-{3,}:?$/.test(cell));
  };

  const parseAlignments = (line, width) => {
    const cells = parseTableCells(line) || [];
    return Array.from({ length: width }, (_, idx) => {
      const cell = cells[idx] || "";
      const left = cell.startsWith(":");
      const right = cell.endsWith(":");
      if (left && right) return "center";
      if (right) return "right";
      return "left";
    });
  };

  const lines = withCodeTokens.split("\n");
  const chunks = [];
  const paragraph = [];
  let inList = false;

  const closeList = () => {
    if (inList) {
      chunks.push("</ul>");
      inList = false;
    }
  };

  const flushParagraph = () => {
    if (!paragraph.length) return;
    const body = renderInlineMarkdown(paragraph.join("\n")).replace(/\n/g, "<br>");
    chunks.push(`<p>${body}</p>`);
    paragraph.length = 0;
  };

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    const trimmed = line.trim();
    const codeMatch = /^@@CODEBLOCK_(\d+)@@$/.exec(trimmed);
    const headingMatch = /^(#{1,6})\s+(.+)$/.exec(trimmed);
    const quoteMatch = /^>\s?(.*)$/.exec(trimmed);
    const listMatch = /^[-*]\s+(.+)$/.exec(trimmed);
    const headerCells = parseTableCells(trimmed);

    if (!trimmed) {
      flushParagraph();
      closeList();
      continue;
    }
    if (codeMatch) {
      flushParagraph();
      closeList();
      chunks.push(codeBlocks[Number(codeMatch[1])] || "");
      continue;
    }
    if (headingMatch) {
      flushParagraph();
      closeList();
      const level = headingMatch[1].length;
      chunks.push(`<h${level}>${renderInlineMarkdown(headingMatch[2])}</h${level}>`);
      continue;
    }
    if (quoteMatch) {
      flushParagraph();
      closeList();
      chunks.push(`<blockquote>${renderInlineMarkdown(quoteMatch[1])}</blockquote>`);
      continue;
    }
    if (listMatch) {
      flushParagraph();
      if (!inList) {
        chunks.push("<ul>");
        inList = true;
      }
      chunks.push(`<li>${renderInlineMarkdown(listMatch[1])}</li>`);
      continue;
    }
    if (headerCells && i + 1 < lines.length && isTableDelimiterRow(lines[i + 1])) {
      flushParagraph();
      closeList();

      const colCount = headerCells.length;
      const alignments = parseAlignments(lines[i + 1], colCount);
      const renderRow = (cells, tag) =>
        Array.from({ length: colCount }, (_, colIdx) => {
          const align = alignments[colIdx] || "left";
          const alignClass = `md-align-${align}`;
          const value = cells[colIdx] || "";
          return `<${tag} class="${alignClass}">${renderInlineMarkdown(value)}</${tag}>`;
        }).join("");

      const bodyRows = [];
      let rowIdx = i + 2;
      while (rowIdx < lines.length) {
        const rowText = lines[rowIdx].trim();
        if (!rowText) break;
        if (isTableDelimiterRow(rowText)) break;
        const rowCells = parseTableCells(rowText);
        if (!rowCells) break;
        bodyRows.push(rowCells);
        rowIdx += 1;
      }

      const thead = `<thead><tr>${renderRow(headerCells, "th")}</tr></thead>`;
      const tbody = bodyRows.length
        ? `<tbody>${bodyRows.map((cells) => `<tr>${renderRow(cells, "td")}</tr>`).join("")}</tbody>`
        : "";
      chunks.push(`<div class="md-table-wrap"><table>${thead}${tbody}</table></div>`);
      i = rowIdx - 1;
      continue;
    }

    closeList();
    paragraph.push(line);
  }

  flushParagraph();
  closeList();

  return chunks
    .join("")
    .replace(/@@CODEBLOCK_(\d+)@@/g, (_, idx) => codeBlocks[Number(idx)] || "");
}

function setMessageContent(div, kind, text) {
  if (kind === "assistant") {
    div.dataset.rawText = text;
    div.innerHTML = renderMarkdown(text);
    return;
  }
  delete div.dataset.rawText;
  div.textContent = text;
}

function appendMessage(kind, text) {
  const div = document.createElement("div");
  div.className = `msg ${kind}`;
  setMessageContent(div, kind, text);
  const thinkingWidget = document.getElementById("nano-agents");
  const widgetVisible = thinkingWidget
    && thinkingWidget.parentElement === chatEl
    && thinkingWidget.style.display !== "none";

  if (widgetVisible) {
    chatEl.insertBefore(div, thinkingWidget);
  } else {
    chatEl.appendChild(div);
  }
  chatEl.scrollTop = chatEl.scrollHeight;
}

function setAgentThinking(thinking) {
  const widget = document.getElementById("nano-agents");
  const timeEl = document.getElementById("nano-time");
  if (!widget) return;

  if (thinking) {
    widget.style.display = "inline-flex";
    chatEl.appendChild(widget);
    chatEl.scrollTop = chatEl.scrollHeight;
    if (!nanoTimerInterval) {
      nanoElapsed = 0;
      if (timeEl) timeEl.textContent = "0s";
      nanoTimerInterval = setInterval(() => {
        nanoElapsed += 1;
        if (timeEl) timeEl.textContent = `${nanoElapsed}s`;
      }, 1000);
    }
  } else {
    widget.style.display = "none";
    if (nanoTimerInterval) {
      clearInterval(nanoTimerInterval);
      nanoTimerInterval = null;
    }
    if (thinkingTimeout) {
      clearTimeout(thinkingTimeout);
      thinkingTimeout = null;
    }
  }
}

function resetThinkingDebounce() {
  setAgentThinking(true);
  if (thinkingTimeout) clearTimeout(thinkingTimeout);
  thinkingTimeout = setTimeout(() => {
    setAgentThinking(false);
  }, 1500);
}

function appendOrUpdateStreamMessage(role, text) {
  let isStreamChunk = false;
  let parsedContent = "";

  try {
    const lines = text.split("\n");
    for (const line of lines) {
      const trimmed = line.trim();
      if (trimmed.startsWith("{") && trimmed.endsWith("}")) {
        const data = JSON.parse(trimmed);
        if (typeof data.content === "string") {
          isStreamChunk = true;
          parsedContent += data.content;
        }
      }
    }
  } catch {
    // Fall back to plain assistant message handling.
  }

  if (!isStreamChunk) {
    appendMessage(role, text);
    setAgentThinking(false);
    return;
  }

  resetThinkingDebounce();

  const messages = chatEl.querySelectorAll(".msg.assistant");
  if (messages.length > 0) {
    const last = messages[messages.length - 1];
    const nextRaw = `${last.dataset.rawText || ""}${parsedContent}`;
    setMessageContent(last, role, nextRaw);
    chatEl.scrollTop = chatEl.scrollHeight;
  } else {
    appendMessage(role, parsedContent);
  }
}

async function refreshStatus() {
  try {
    const r = await fetch("/api/status");
    const j = await r.json();
    apiStatusEl.textContent = "";
    const dot = document.createElement("span");
    dot.className = "status-dot dot-ok";
    apiStatusEl.appendChild(dot);
    apiStatusEl.appendChild(document.createTextNode(j.status));
    sessionCountEl.textContent = j.sessions;

    // Show update banner if a newer version is available
    if (j.latest_version && j.version) {
      const dismissed = sessionStorage.getItem("opencrust.update_dismissed");
      if (dismissed !== j.latest_version) {
        updateBannerText.textContent =
          `Update available: v${j.version} \u2192 v${j.latest_version.replace(/^v/, "")} - run opencrust update`;
        updateBanner.style.display = "";
      }
    } else {
      updateBanner.style.display = "none";
    }

    channelListEl.textContent = "";
    if (j.channels && j.channels.length > 0) {
      for (const ch of j.channels) {
        const tag = document.createElement("span");
        tag.className = "channel-tag";
        const chDot = document.createElement("span");
        chDot.className = "status-dot dot-ok";
        tag.appendChild(chDot);
        tag.appendChild(document.createTextNode(ch));
        channelListEl.appendChild(tag);
      }
    } else {
      const noChannels = document.createElement("span");
      noChannels.className = "no-channels";
      noChannels.textContent = "None configured";
      channelListEl.appendChild(noChannels);
    }
  } catch {
    apiStatusEl.textContent = "";
    const offDot = document.createElement("span");
    offDot.className = "status-dot dot-off";
    apiStatusEl.appendChild(offDot);
    apiStatusEl.appendChild(document.createTextNode("unavailable"));
    sessionCountEl.textContent = "-";
    channelListEl.textContent = "-";
  }

  loadProviders();
  loadUsage();
}

async function loadSessionHistory(sid) {
  try {
    const headers = gatewayKey ? { Authorization: `Bearer ${gatewayKey}` } : {};
    const r = await fetch(`/api/sessions/${encodeURIComponent(sid)}/history`, { headers });
    if (!r.ok) return;
    const j = await r.json();
    const msgs = j.messages || [];
    if (msgs.length === 0) return;
    chatEl.querySelectorAll(".msg").forEach((m) => m.remove());
    for (const msg of msgs) {
      appendMessage(msg.role === "assistant" ? "assistant" : "user", msg.content);
    }
    appendMessage("sys", `Session restored (${msgs.length} messages).`);
  } catch {
    // silently ignore — chat will just be empty
  }
}

async function loadUsage() {
  const periodEl = document.getElementById("usage-period");
  const period = periodEl ? periodEl.value : "";
  const url = period ? `/api/usage?period=${encodeURIComponent(period)}` : "/api/usage";
  try {
    const headers = gatewayKey ? { Authorization: `Bearer ${gatewayKey}` } : {};
    const r = await fetch(url, { cache: "no-store", headers });
    const j = await r.json();
    const fmt = (n) => (typeof n === "number" ? n.toLocaleString() : "—");
    const inputEl = document.getElementById("usage-input");
    const outputEl = document.getElementById("usage-output");
    const totalEl = document.getElementById("usage-total");
    if (inputEl) inputEl.textContent = fmt(j.input_tokens);
    if (outputEl) outputEl.textContent = fmt(j.output_tokens);
    if (totalEl) totalEl.textContent = fmt(j.total_tokens);
  } catch {
    ["usage-input", "usage-output", "usage-total"].forEach((id) => {
      const el = document.getElementById(id);
      if (el) el.textContent = "—";
    });
  }
}

async function loadProviders() {
  try {
    const r = await fetch("/api/providers");
    const j = await r.json();
    providerData = j.providers || [];

    providerSelect.innerHTML = "";
    for (const p of providerData) {
      const opt = document.createElement("option");
      opt.value = p.id;
      opt.textContent = p.active ? p.display_name : `${p.display_name} (not configured)`;
      providerSelect.appendChild(opt);
    }

    // Restore saved selection, or pick the default
    const defaultProvider = providerData.find(p => p.is_default);
    const saved = selectedProvider || (defaultProvider ? defaultProvider.id : "");
    if (saved && [...providerSelect.options].some(o => o.value === saved)) {
      providerSelect.value = saved;
    }
    updateProviderUI();
  } catch {
    providerSelect.innerHTML = '<option value="">unavailable</option>';
    providerStatus.textContent = "";
    providerModelInput.value = "";
    providerModelInput.disabled = true;
    providerModelStatus.textContent = "";
    providerModelOptions.innerHTML = "";
  }
}

function updateModelUI(provider) {
  providerModelOptions.innerHTML = "";

  if (!provider) {
    providerModelInput.value = "";
    providerModelInput.placeholder = "Provider default model";
    providerModelInput.disabled = true;
    providerModelStatus.textContent = "";
    return;
  }

  const models = Array.isArray(provider.models)
    ? provider.models.filter(m => typeof m === "string" && m.trim().length > 0)
    : [];
  for (const modelName of models) {
    const opt = document.createElement("option");
    opt.value = modelName;
    providerModelOptions.appendChild(opt);
  }

  const savedModel = getSavedModelForProvider(provider.id);
  const defaultModel = typeof provider.model === "string" ? provider.model : "";
  const preferredModel = savedModel || defaultModel;
  providerModelInput.value = preferredModel;
  providerModelInput.placeholder = defaultModel
    ? `Provider default: ${defaultModel}`
    : "Provider default model";
  providerModelInput.disabled = !provider.active;

  if (!provider.active) {
    providerModelStatus.textContent = "Activate provider to choose a model.";
  } else if (models.length > 0) {
    providerModelStatus.textContent = `${models.length} model${models.length === 1 ? "" : "s"} available`;
  } else {
    providerModelStatus.textContent = "No model list available; type a model name to override.";
  }
}

function updateProviderUI() {
  const id = providerSelect.value;
  const p = providerData.find(x => x.id === id);
  if (!p) {
    providerStatus.textContent = "";
    providerKeySection.style.display = "none";
    updateModelUI(null);
    return;
  }
  if (p.active) {
    const tag = p.is_default ? "active, default" : "active";
    providerStatus.innerHTML = `<span class="status-dot dot-ok"></span>${tag}`;
    providerKeySection.style.display = "none";
  } else {
    providerStatus.innerHTML = `<span class="status-dot dot-off"></span>not configured`;
    providerKeySection.style.display = p.needs_api_key ? "" : "none";
  }
  selectedProvider = id;
  localStorage.setItem(providerStorage, id);
  updateModelUI(p);
}

providerSelect.addEventListener("change", () => {
  updateProviderUI();
  // If switching to an active provider, tell the backend to use it as default
  const p = providerData.find(x => x.id === providerSelect.value);
  if (p && p.active) {
    fetch("/api/providers", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ provider_type: p.id, set_default: true }),
    }).then(() => loadProviders());
  }
});

providerActivateBtn.addEventListener("click", async () => {
  const id = providerSelect.value;
  const key = providerApiKey.value.trim();
  if (!key) return;
  const model = providerModelInput.value.trim();

  providerActivateBtn.textContent = "Activating...";
  providerActivateBtn.disabled = true;

  try {
    const payload = { provider_type: id, api_key: key, set_default: true };
    if (model) payload.model = model;
    const baseUrl = providerBaseUrlInput.value.trim();
    if (baseUrl) {
      if (!baseUrl.startsWith("http://") && !baseUrl.startsWith("https://")) {
        providerStatus.textContent = "Base URL must start with http:// or https://";
        return;
      }
      payload.base_url = baseUrl;
    }
    const r = await fetch("/api/providers", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    const j = await r.json();
    if (r.ok) {
      providerApiKey.value = "";
      appendMessage("sys", `Provider ${id} activated.`);
      await loadProviders();
    } else {
      appendMessage("error", j.message || "Failed to activate provider.");
    }
  } catch (e) {
    appendMessage("error", `Failed to activate provider: ${e}`);
  } finally {
    providerActivateBtn.textContent = "Save & Activate";
    providerActivateBtn.disabled = false;
  }
});

providerModelInput.addEventListener("input", () => {
  const providerId = providerSelect.value;
  if (!providerId) return;
  setSavedModelForProvider(providerId, providerModelInput.value.trim());
});

function scheduleReconnect() {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, 2000);
}

function handleServerEvent(raw) {
  let evt;
  try {
    evt = JSON.parse(raw);
  } catch {
    appendMessage("sys", `Raw: ${raw}`);
    return;
  }

  if (evt.session_id) setSession(evt.session_id);

  switch (evt.type) {
    case "connected":
      if (evt.note) {
        appendMessage("sys", `Connected (${evt.note}).`);
      }
      refreshStatus();
      break;
    case "resumed":
      refreshStatus();
      if (evt.history_length && evt.history_length > 0) {
        loadSessionHistory(evt.session_id || sessionId);
      }
      break;
    case "message":
      appendOrUpdateStreamMessage("assistant", evt.content || "(empty response)");
      loadUsage();
      break;
    case "error":
      setAgentThinking(false);
      appendMessage("error", `${evt.code || "error"}: ${evt.message || "unknown error"}`);
      break;
    default:
      appendMessage("sys", `Event ${evt.type || "unknown"}: ${JSON.stringify(evt)}`);
  }
}

function connect() {
  if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) {
    return;
  }

  socket = new WebSocket(wsUrl());

  socket.onopen = () => {
    setConnectionState(true);
    if (sessionId) {
      socket.send(JSON.stringify({ type: "resume", session_id: sessionId }));
    } else {
      socket.send(JSON.stringify({ type: "init" }));
    }
  };

  socket.onmessage = (ev) => handleServerEvent(ev.data);

  socket.onclose = () => {
    setAgentThinking(false);
    setConnectionState(false);
    scheduleReconnect();
  };

  socket.onerror = () => {
    setAgentThinking(false);
    setConnectionState(false);
  };
}

function reconnectFresh() {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  if (socket) {
    socket.onclose = null;
    try { socket.close(); } catch { }
    socket = null;
  }
  setConnectionState(false);
  connect();
}

function setPendingFile(filename) {
  pendingFilename = filename;
  if (filename) {
    filePendingName.textContent = filename;
    filePendingBar.style.display = "";
  } else {
    filePendingBar.style.display = "none";
    filePendingName.textContent = "";
  }
}

async function uploadFile(file) {
  if (!sessionId) {
    appendMessage("error", "No active session. Connect first.");
    return;
  }

  const MAX_BYTES = 25 * 1024 * 1024;
  if (file.size > MAX_BYTES) {
    appendMessage("error", `File too large (max 25 MB): ${file.name}`);
    return;
  }

  uploadBtn.disabled = true;
  appendMessage("sys", `Uploading ${escapeHtml(file.name)}…`);

  const formData = new FormData();
  formData.append("file", file, file.name);

  const headers = gatewayKey ? { Authorization: `Bearer ${gatewayKey}` } : {};
  try {
    const r = await fetch(`/api/sessions/${encodeURIComponent(sessionId)}/upload`, {
      method: "POST",
      headers,
      body: formData,
    });
    const j = await r.json();
    if (r.ok) {
      setPendingFile(j.filename || file.name);
      appendMessage("sys", `File attached: ${escapeHtml(j.filename || file.name)}. Send /ingest to add it to memory${pendingFilename ? " (or /ingest replace to overwrite an existing version)" : ""}.`);
    } else {
      appendMessage("error", `Upload failed: ${j.message || r.statusText}`);
    }
  } catch (e) {
    appendMessage("error", `Upload error: ${e}`);
  } finally {
    uploadBtn.disabled = false;
    fileInput.value = "";
  }
}

uploadBtn.addEventListener("click", () => {
  if (!sessionId) {
    appendMessage("error", "No active session. Connect first.");
    return;
  }
  fileInput.click();
});

fileInput.addEventListener("change", () => {
  const file = fileInput.files && fileInput.files[0];
  if (file) uploadFile(file);
});

filePendingClear.addEventListener("click", () => {
  setPendingFile(null);
});

function sendMessage() {
  const content = inputEl.value.trim();
  if (!content) return;

  if (!socket || socket.readyState !== WebSocket.OPEN) {
    appendMessage("error", "Not connected. Click Reconnect to try again.");
    return;
  }

  appendMessage("user", content);
  // If the user sent /ingest, clear the local pending file indicator.
  if (content.trim().split(/\s+/)[0] === "/ingest") setPendingFile(null);
  setAgentThinking(true);
  const msg = { content };
  const pid = providerSelect.value;
  if (pid) msg.provider = pid;
  const model = providerModelInput.value.trim();
  if (model) msg.model = model;
  socket.send(JSON.stringify(msg));
  inputEl.value = "";
  inputEl.focus();
}

sendBtn.addEventListener("click", sendMessage);
inputEl.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    sendMessage();
  }
});

reconnectBtn.addEventListener("click", () => {
  reconnectFresh();
});

clearBtn.addEventListener("click", () => {
  setAgentThinking(false);
  setPendingFile(null);
  chatEl.querySelectorAll(".msg").forEach((msg) => msg.remove());
  setSession("");
  reconnectFresh();
});

refreshBtn.addEventListener("click", refreshStatus);

const usagePeriodEl = document.getElementById("usage-period");
if (usagePeriodEl) usagePeriodEl.addEventListener("change", loadUsage);

updateBannerClose.addEventListener("click", () => {
  updateBanner.style.display = "none";
  const ver = updateBannerText.textContent;
  // Extract version to remember dismissal for this version only
  const match = updateBannerText.innerHTML.match(/v([\d.]+)\s/);
  if (match) sessionStorage.setItem("opencrust.update_dismissed", match[1]);
});

authConnectBtn.addEventListener("click", () => {
  gatewayKey = keyGatewayEl.value.trim();
  if (gatewayKey) {
    localStorage.setItem(keyStorage, gatewayKey);
  }
  reconnectFresh();
});


if (themeToggleBtn) {
  themeToggleBtn.addEventListener("click", () => {
    const current = document.documentElement.getAttribute("data-theme") || "light";
    setTheme(current === "dark" ? "light" : "dark");
  });
}

// Navigation logic
const navItems = {
  chat: document.getElementById("nav-chat"),
  mcps: document.getElementById("nav-mcps"),
  integrations: document.getElementById("nav-integrations")
};

const views = {
  chat: document.getElementById("view-chat"),
  mcps: document.getElementById("view-mcps"),
  integrations: document.getElementById("view-integrations")
};

function switchView(viewId) {
  // Update nav active states
  Object.entries(navItems).forEach(([id, el]) => {
    if (id === viewId) {
      el.classList.add("active");
    } else {
      el.classList.remove("active");
    }
  });

  // Update view visibility
  Object.entries(views).forEach(([id, el]) => {
    if (id === viewId) {
      el.style.display = "";
    } else {
      el.style.display = "none";
    }
  });
}

navItems.chat.addEventListener("click", () => switchView("chat"));
navItems.mcps.addEventListener("click", () => {
  switchView("mcps");
  loadMcpServers();
});

async function loadMcpServers() {
  const list = document.getElementById("mcps-list");
  try {
    const resp = await fetch("/api/mcp");
    const data = await resp.json();
    const servers = data.servers || [];
    if (servers.length === 0) {
      list.innerHTML = '<p style="color:var(--text-secondary)">No MCP servers configured. Add servers to config.yml.</p>';
      return;
    }
    list.innerHTML = servers.map(s => {
      const statusClass = s.connected ? "online" : "offline";
      const statusText = s.connected ? "Connected" : "Disconnected";
      return `<div class="card">
        <div class="card-header">
          <h3 class="card-title">${s.name}</h3>
          <span class="pill ${statusClass}">${statusText}</span>
        </div>
        <div class="card-meta">${s.tools} tool${s.tools !== 1 ? "s" : ""} registered</div>
      </div>`;
    }).join("");
  } catch (e) {
    list.innerHTML = '<p style="color:var(--text-secondary)">Failed to load MCP servers.</p>';
  }
}
navItems.integrations.addEventListener("click", async () => {
  switchView("integrations");
  if (integrationsView) await integrationsView.load();
});

// Boot: check if auth is required, then connect
async function boot() {
  initTheme();
  initFontSize();
  try {
    if (integrationsView) await integrationsView.load();
  } catch (e) {
    console.warn("Integrations failed to load:", e);
  }
  requestAnimationFrame(() => {
    document.body.classList.add("ready");
  });
  setConnectionState(false);
  setSession(sessionId);
  refreshStatus();

  try {
    const r = await fetch("/api/auth-check");
    const j = await r.json();
    authRequired = j.auth_required;
  } catch {
    authRequired = false;
  }

  if (authRequired && !window.__OPENCRUST_GATEWAY_KEY__) {
    // Server requires auth but did not inject a key — ask the user to enter it.
    authSection.style.display = "";
    if (gatewayKey) {
      // Have a saved key — try connecting with it
      connect();
    } else {
      appendMessage("sys", "This gateway requires an API key. Enter it in the sidebar.");
    }
  } else {
    authSection.style.display = "none";
    connect();
  }
}

function initNanoAgents() {
  const bg = document.getElementById("nano-bg");
  const grid = document.getElementById("nano-grid");
  const widget = document.getElementById("nano-agents");
  if (!bg || !grid) return;
  if (widget) widget.style.display = "none";

  const colors = ["var(--brand)", "var(--brand-2)", "var(--online)", "var(--accent-line)"];
  const bits = [];
  for (let i = 0; i < 6; i++) {
    const bit = document.createElement("div");
    bit.className = "nano-bit";
    const size = Math.random() * 2 + 1;
    bit.style.width = `${size}px`;
    bit.style.height = `${size}px`;
    bit.style.left = `${Math.random() * 100}%`;
    bit.style.top = `${Math.random() * 100}%`;
    bit.style.backgroundColor = colors[Math.floor(Math.random() * colors.length)];
    bit.style.opacity = "0";
    bg.appendChild(bit);
    bits.push({ el: bit, id: Math.random() });
  }

  setInterval(() => {
    bits.forEach((b) => {
      if (Math.random() > 0.95) {
        b.el.style.left = `${Math.random() * 100}%`;
        b.el.style.top = `${Math.random() * 100}%`;
        b.el.style.opacity = "0";
      } else {
        b.el.style.opacity = String(Math.sin(Date.now() / 1500 + b.id * 10) * 0.1 + 0.1);
      }
    });
  }, 250);

  const agentColors = [
    ["var(--brand)", "var(--brand-2)"],
    ["var(--online)", "#4ade80"],
    ["var(--warn-text)", "var(--warn-edge)"],
    ["var(--ink-soft)", "var(--ink)"]
  ];

  const agentEls = [];
  let positions = [0, 1, 2, 3];

  for (let i = 0; i < 4; i++) {
    const agent = document.createElement("div");
    agent.className = "nano-agent";
    for (let p = 0; p < 4; p++) {
      const pixel = document.createElement("div");
      pixel.className = "nano-pixel";
      agent.appendChild(pixel);
    }
    grid.appendChild(agent);
    agentEls.push({ el: agent, colors: agentColors[i] });
  }

  setInterval(() => {
    agentEls.forEach((a) => {
      const pixels = a.el.children;
      for (let i = 0; i < pixels.length; i++) {
        pixels[i].style.backgroundColor = a.colors[Math.floor(Math.random() * a.colors.length)];
        pixels[i].style.opacity = String(0.7 + Math.random() * 0.3);
      }
    });
  }, 700);

  function getCoords(index) {
    return { x: (index % 2) * 12, y: Math.floor(index / 2) * 12 };
  }

  function updatePositions() {
    agentEls.forEach((a, i) => {
      const pos = positions[i];
      const coords = getCoords(pos);
      a.el.style.transform = `translate(${coords.x}px, ${coords.y}px)`;
    });
  }

  updatePositions();
  setInterval(() => {
    const next = [...positions];
    next.unshift(next.pop());
    positions = next;
    updatePositions();
  }, 2700);
}

initIntegrationsViewShell();
boot();
initNanoAgents();
