const tauri = window.__TAURI__;
const invoke = tauri?.core?.invoke ?? tauri?.invoke;
const listen = tauri?.event?.listen;

document.querySelectorAll("form,input").forEach((node) => {
  node.autocomplete = "off";
});

const state = {
  session: null,
  selected: null,
  members: [],
  routes: [],
  relays: [],
  networkInterfaces: [],
  pendingJoinGroupName: "",
  groupMessages: [],
  directMessages: new Map(),
  transfers: new Map(),
};

const $ = (id) => document.querySelector(`#${id}`);
const text = (value) => String(value ?? "");
const short = (value) => text(value).slice(0, 8);
const headerOf = (message) => message?.header ?? {};
const payloadOf = (message) => message?.payload ?? {};
const sourceOf = (message) => headerOf(message).source_device_id ?? headerOf(message).sourceDeviceId;
const targetOf = (message) => headerOf(message).target ?? {};
const groupNameOf = (session) => session?.group_name || "群聊";
const isLoopback = (ip) => ip === "127.0.0.1" || ip?.startsWith("127.");

function parseHostPort(value) {
  const raw = text(value).trim();
  const index = raw.lastIndexOf(":");
  if (index <= 0) return {};
  return { host: raw.slice(0, index), port: raw.slice(index + 1) };
}

function encodeShare(value) {
  const bytes = new TextEncoder().encode(JSON.stringify(value));
  return `lanmesh:${btoa(String.fromCharCode(...bytes))}`;
}

function decodeShare(value) {
  const code = text(value).trim().replace(/^lanmesh:/i, "");
  const bytes = Uint8Array.from(atob(code), (char) => char.charCodeAt(0));
  return JSON.parse(new TextDecoder().decode(bytes));
}

const status = $("status");
const sessionList = $("session-list");
const memberList = $("member-list");
const messages = $("messages");
const transfers = $("transfers");
const events = $("events");

function setStatus(value) {
  status.textContent = value;
}

function log(name, payload) {
  events.textContent += `${new Date().toLocaleTimeString()} ${name} ${JSON.stringify(payload)}\n`;
  events.scrollTop = events.scrollHeight;
}

async function call(command, args = {}) {
  if (!invoke) throw new Error("Tauri invoke API is unavailable");
  return invoke(command, args);
}

function showDialog(id) {
  const dialog = $(id);
  if (dialog?.showModal) dialog.showModal();
}

function closeDialog(id) {
  $(id)?.close();
}

function bindAddr() {
  return $("create-bind-preset").value || $("create-bind").value || "0.0.0.0:0";
}

function selectedLocalIp() {
  return $("join-interface").value || $("manual-local-ip").value;
}

function sessionLabel() {
  if (!state.session) return "未连接";
  const role = state.session.role === "relay" ? "Relay" : "Leaf";
  const addr = state.session.bind_addr ? ` · ${state.session.bind_addr}` : "";
  return `${role} · ${short(state.session.device_id)} · ${short(state.session.group_id)}${addr}`;
}

function setSession(session, groupName = "") {
  session.group_name = groupName || session.group_name || state.pendingJoinGroupName || groupNameOf(session);
  state.pendingJoinGroupName = "";
  state.session = session;
  state.selected = { type: "group" };
  state.members = [];
  state.routes = [];
  state.groupMessages = [];
  state.directMessages.clear();
  state.transfers.clear();
  setStatus(sessionLabel());
  $("manual-group-id").value = session.group_id;
  $("manual-group-name").value = session.group_name;
  refreshMembers();
  renderAll();
}

function clearSession() {
  state.session = null;
  state.selected = null;
  state.members = [];
  state.routes = [];
  state.groupMessages = [];
  state.directMessages.clear();
  state.transfers.clear();
  setStatus("未连接");
  renderAll();
}

function forgetRelayGroup(groupId) {
  state.relays = state.relays.filter((relay) => relay.group_id !== groupId);
}

function renderAll() {
  renderSessionList();
  renderMembers();
  renderConversation();
  renderTransfers();
}

function renderSessionList() {
  sessionList.innerHTML = "";

  if (state.session) {
    const node = document.createElement("button");
    node.type = "button";
    node.className = `item clickable ${state.selected?.type === "group" ? "active" : ""}`;
    node.innerHTML = `
      <div class="item-head">
        <span class="title">${groupNameOf(state.session)}</span>
        <span class="badge">${state.session.role === "relay" ? "我创建的" : "已加入"}</span>
      </div>
      <div class="muted">${sessionLabel()}</div>
    `;
    node.addEventListener("click", openGroup);
    sessionList.append(node);
  }

  for (const relay of state.relays) {
    if (relay.group_id === state.session?.group_id) continue;
    const node = document.createElement("button");
    node.type = "button";
    node.className = "item clickable";
    node.innerHTML = `
      <div class="item-head">
        <span class="title">${relay.group_name || "LAN Mesh"}</span>
        <span class="badge">可加入</span>
      </div>
      <div class="muted">Group ${short(relay.group_id)} · ${relay.tcp_addr}</div>
    `;
    node.addEventListener("click", () => openJoinDialog(relay));
    sessionList.append(node);
  }

  if (!sessionList.children.length) {
    sessionList.append(emptyItem("没有群组。新建或加入后会出现在这里。"));
  }
}

function renderMembers() {
  memberList.innerHTML = "";
  if (!state.session) {
    memberList.append(emptyItem("加入群组后显示可单聊对象。"));
    return;
  }

  const peers = state.members.filter((member) => member.device_id !== state.session.device_id);
  for (const member of peers) {
    const node = document.createElement("button");
    node.type = "button";
    node.className = `item clickable ${state.selected?.type === "direct" && state.selected.id === member.device_id ? "active" : ""}`;
    node.innerHTML = `
      <div class="item-head">
        <span class="title">${short(member.device_id)}</span>
        <span class="badge ${member.online ? "online" : ""}">${member.online ? "在线" : "离线"}</span>
      </div>
      <div class="muted">${routeLabel(member)}</div>
    `;
    node.addEventListener("click", () => openDirect(member.device_id));
    memberList.append(node);
  }

  if (!peers.length) memberList.append(emptyItem("暂无其他成员。"));
}

function emptyItem(content) {
  const node = document.createElement("div");
  node.className = "item muted";
  node.textContent = content;
  return node;
}

function routeLabel(member) {
  const route = state.routes.find((item) => item.target_device_id === member.device_id);
  if (!member.online) return "离线";
  if (!route) return "可达状态未知";
  return route.path.length <= 2 ? "直连可达" : `多跳可达(${route.path.length - 1}跳)`;
}

function openGroup() {
  if (!state.session) return;
  state.selected = { type: "group" };
  renderAll();
}

function openDirect(deviceId) {
  if (!state.session || !deviceId) return;
  state.selected = { type: "direct", id: deviceId };
  if (!state.directMessages.has(deviceId)) state.directMessages.set(deviceId, []);
  renderAll();
}

function activeMessages() {
  if (state.selected?.type === "group") return state.groupMessages;
  if (state.selected?.type !== "direct") return [];
  if (!state.directMessages.has(state.selected.id)) state.directMessages.set(state.selected.id, []);
  return state.directMessages.get(state.selected.id);
}

function renderConversation() {
  const hasSelection = Boolean(state.session && state.selected);
  $("empty-state").classList.toggle("hidden", hasSelection);
  $("chat-pane").classList.toggle("hidden", !hasSelection);
  if (!hasSelection) return;

  const isGroup = state.selected.type === "group";
  $("peer-title").textContent = isGroup ? groupNameOf(state.session) : `单聊 ${short(state.selected.id)}`;
  $("peer-subtitle").textContent = isGroup ? sessionLabel() : state.selected.id;
  $("leave-button").textContent = state.session.role === "relay" ? "解散群组" : "退出群组";
  $("share-button").classList.toggle("hidden", !(isGroup && state.session.role === "relay"));
  renderMessages();
}

function pushMessage(list, item) {
  item.at ??= Date.now();
  list.push(item);
  list.sort((a, b) => a.at - b.at);
  renderMessages();
}

function renderMessages() {
  messages.innerHTML = "";
  for (const item of activeMessages()) {
    const node = document.createElement("div");
    node.className = `message ${item.mine ? "mine" : ""}`;
    const content = document.createElement("div");
    const meta = document.createElement("div");
    content.textContent = `${item.kind === "file" ? "📎 " : ""}${item.content}`;
    meta.className = "muted";
    meta.textContent = `${item.mine ? "我" : short(item.from)} · ${new Date(item.at).toLocaleTimeString()} · ${item.status}`;
    node.append(content, meta);
    if (item.kind === "file" && item.path && !item.mine) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "secondary mini";
      button.textContent = "另存为";
      button.addEventListener("click", () => saveReceivedFile(item));
      node.append(button);
    }
    messages.append(node);
  }
  messages.scrollTop = messages.scrollHeight;
}

function addIncoming(message) {
  if (!message || !state.session) return;
  if (message.type !== "text") return;
  const source = sourceOf(message);
  const target = targetOf(message);
  const payload = payloadOf(message);
  const item = {
    from: source,
    mine: source === state.session.device_id,
    status: "已送达",
    content: message.type === "text" ? payload.content : `文件分片 ${payload.file_id || ""}`,
    kind: message.type === "file_chunk" ? "file" : "text",
    at: headerOf(message).timestamp_ms || Date.now(),
  };

  if (target.kind === "device" || target.device_id || target.deviceId) {
    const peer = source === state.session.device_id ? target.device_id ?? target.deviceId : source;
    if (!state.directMessages.has(peer)) state.directMessages.set(peer, []);
    pushMessage(state.directMessages.get(peer), item);
    return;
  }
  pushMessage(state.groupMessages, item);
}

async function refreshMembers() {
  if (!state.session) return;
  const [memberList, statusSnapshot] = await Promise.all([
    call("get_members"),
    call("get_connection_status"),
  ]);
  state.members = memberList.sort((a, b) => text(a.device_id).localeCompare(text(b.device_id)));
  state.routes = statusSnapshot.routes;
  renderAll();
}

function renderRelays(items) {
  const relays = $("relays");
  relays.innerHTML = "";
  const visibleItems = items.filter((relay) => relayState(relay) !== "own");
  if (!visibleItems.length) {
    relays.append(emptyItem("未发现 Relay，可手动填写。"));
    return;
  }
  for (const relay of visibleItems) {
    const state = relayState(relay);
    const node = document.createElement("button");
    node.type = "button";
    node.disabled = state === "joined";
    node.className = `item ${state === "joined" ? "disabled" : "clickable"}`;
    node.innerHTML = `
      <div class="item-head">
        <span class="title">${relay.group_name || "LAN Mesh"}</span>
        <span class="badge">${state === "joined" ? "已加入" : "填入"}</span>
      </div>
      <div class="muted">${relay.group_id}<br />${relay.tcp_addr}</div>
    `;
    if (state !== "joined") node.addEventListener("click", () => fillJoinForm(relay));
    relays.append(node);
  }
}

function relayState(relay) {
  if (!state.session) return "available";
  if (relay.device_id === state.session.device_id || (relay.group_id === state.session.group_id && state.session.role === "relay")) {
    return "own";
  }
  return relay.group_id === state.session.group_id ? "joined" : "available";
}

function fillJoinForm(relay) {
  $("manual-group-id").value = relay.group_id;
  $("manual-relay-addr").value = relay.tcp_addr;
  $("manual-group-name").value = relay.group_name || "群聊";
}

function openJoinDialog(relay = null) {
  if (relay) fillJoinForm(relay);
  showDialog("join-dialog");
}

function renderNetworkInterfaces(items) {
  state.networkInterfaces = items;
  const networkInterfaces = $("network-interfaces");
  const create = $("create-bind-preset");
  const discover = $("discover-bind");
  const joinSelect = $("join-interface");
  networkInterfaces.innerHTML = "";
  create.length = 3;
  discover.length = 2;
  joinSelect.length = 1;

  for (const item of items) {
    const node = document.createElement("div");
    node.className = "item muted";
    node.textContent = `${item.name} · ${item.ip_addr}`;
    networkInterfaces.append(node);
    create.add(new Option(`${item.name} (${item.ip_addr})`, item.bind_addr));
    discover.add(new Option(`${item.name} (${item.ip_addr})`, item.discovery_bind_addr));
    joinSelect.add(new Option(`${item.name} (${item.ip_addr})`, item.ip_addr));
  }
}

async function loadNetworkInterfaces() {
  try {
    renderNetworkInterfaces(await call("list_network_interfaces"));
  } catch (err) {
    log("list_network_interfaces failed", text(err));
  }
}

function relayAddressCandidates() {
  if (!state.session?.bind_addr) return [];
  const { host, port } = parseHostPort(state.session.bind_addr);
  if (!port) return [];
  if (host && host !== "0.0.0.0" && host !== "::") {
    return [{ name: "监听地址", ip: host, addr: state.session.bind_addr }];
  }
  const items = state.networkInterfaces.filter((item) => item.ip_addr);
  const usableItems = items.filter((item) => !isLoopback(item.ip_addr));
  return (usableItems.length ? usableItems : items).map((item) => ({
    name: item.name,
    ip: item.ip_addr,
    addr: `${item.ip_addr}:${port}`,
  }));
}

function sharePayload() {
  const candidates = relayAddressCandidates();
  return {
    version: 1,
    group_id: state.session.group_id,
    group_name: groupNameOf(state.session),
    relay_port: Number(parseHostPort(state.session.bind_addr).port),
    relay_addrs: candidates.map((item) => item.addr),
    relay_interfaces: candidates,
  };
}

function shareText(payload) {
  return [
    `群组: ${payload.group_name}`,
    `Group ID: ${payload.group_id}`,
    "Relay 地址:",
    ...payload.relay_addrs.map((addr) => `- ${addr}`),
    `分享码: ${encodeShare(payload)}`,
  ].join("\n");
}

async function copyText(value) {
  try {
    await navigator.clipboard.writeText(value);
    setStatus("已复制到剪贴板");
  } catch (err) {
    setStatus(`复制失败：${err}`);
  }
}

function openShareDialog() {
  if (!state.session) return;
  const payload = sharePayload();
  $("share-summary").textContent = `${payload.group_name} · Group ${short(payload.group_id)} · ${payload.relay_addrs.length} 个可连接地址`;
  $("share-code-output").value = encodeShare(payload);
  const list = $("share-addresses");
  list.innerHTML = "";
  for (const item of payload.relay_interfaces) {
    const node = document.createElement("div");
    node.className = "item";
    node.innerHTML = `
      <div class="item-head">
        <span class="title">${item.name}</span>
        <button type="button" class="secondary mini">复制</button>
      </div>
      <div class="muted">${item.addr}</div>
    `;
    node.querySelector("button").addEventListener("click", () => copyText(item.addr));
    list.append(node);
  }
  if (!list.children.length) list.append(emptyItem("没有可分享的网卡地址。"));
  showDialog("share-dialog");
}

function fillLocalIp(localIp = "") {
  $("join-interface").value = localIp;
  $("manual-local-ip").value = localIp;
}

async function parseShareIntoJoinForm() {
  try {
    const payload = decodeShare($("share-code-input").value);
    const relayAddrs = payload.relay_addrs || payload.relayAddrs || [];
    if (!payload.group_id || !relayAddrs.length) throw new Error("分享码缺少 Group ID 或 Relay 地址");
    $("manual-group-id").value = payload.group_id;
    $("manual-group-name").value = payload.group_name || "群聊";
    $("manual-relay-addr").value = relayAddrs[0];
    fillLocalIp("");
    const localIps = state.networkInterfaces.map((item) => item.ip_addr).filter((ip) => ip && !isLoopback(ip));
    try {
      const probe = await call("probe_relay_addr", { relayAddrs, localIps, timeoutMs: 250 });
      $("manual-relay-addr").value = probe.relay_addr;
      fillLocalIp(probe.local_ip || "");
      setStatus(`已解析分享码：${probe.relay_addr}`);
    } catch (err) {
      setStatus(`已解析分享码，但未探测到可用地址，先填入第一个候选：${err}`);
    }
  } catch (err) {
    setStatus(`分享码解析失败：${err}`);
  }
}

async function join(groupId, relayAddr, localIp = "") {
  const session = await call("join_group", {
    deviceId: null,
    groupId,
    relayAddr,
    localIp: localIp || null,
  });
  setSession(session, $("manual-group-name").value.trim());
}

async function closeCurrentSession() {
  if (!state.session) return;
  const action = state.session.role === "relay" ? "解散群组" : "退出群组";
  const groupId = state.session.group_id;
  if (!confirm(`${action}后会断开当前会话，继续？`)) return;
  await call("close_session");
  forgetRelayGroup(groupId);
  clearSession();
}

function formatBytes(value) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}

function missingChunks(item) {
  if (item.chunks) {
    return Array.from({ length: item.chunk_count || 0 }, (_, index) => index).filter((index) => !item.chunks.has(index));
  }
  const start = Math.min(item.done_chunks || 0, item.chunk_count || 0);
  return Array.from({ length: (item.chunk_count || 0) - start }, (_, index) => start + index);
}

function transferredBytes(item) {
  if (!item.chunk_count) return 0;
  return Math.min(item.total_size, Math.ceil(item.total_size / item.chunk_count) * (item.done_chunks || 0));
}

function transferSpeed(item) {
  const seconds = ((item.updatedAt || Date.now()) - item.firstSeen) / 1000;
  return seconds > 0 ? transferredBytes(item) / seconds : 0;
}

function rememberTransfer(payload) {
  const now = Date.now();
  const old = state.transfers.get(payload.file_id) || { firstSeen: now };
  const chunks = old.chunks || new Set();
  chunks.add(payload.chunk_index);
  const done_chunks = chunks.size;
  const status = payload.status || (done_chunks >= payload.chunk_count ? "done" : "running");
  state.transfers.set(payload.file_id, {
    ...old,
    ...payload,
    chunks,
    done_chunks,
    updatedAt: now,
    status,
    announced: old.announced || (payload.direction === "incoming" && status === "done" && payload.path),
  });
  if (payload.direction === "incoming" && status === "done" && payload.path && !old.announced) {
    addReceivedFile(payload);
  }
  renderTransfers();
}

function addReceivedFile(payload) {
  const fileName = payload.file_name || payload.path;
  const item = {
    from: payload.from,
    mine: false,
    status: "已接收",
    content: `文件已接收：${fileName}`,
    kind: "file",
    file_name: fileName,
    path: payload.path,
    at: Date.now(),
  };
  if (payload.target_device_id) {
    const peer = payload.from === state.session?.device_id ? payload.target_device_id : payload.from;
    if (!state.directMessages.has(peer)) state.directMessages.set(peer, []);
    pushMessage(state.directMessages.get(peer), item);
    return;
  }
  pushMessage(state.groupMessages, item);
}

async function saveReceivedFile(item) {
  try {
    const saved = await call("save_file_as", { path: item.path, fileName: item.file_name });
    setStatus(`已另存为：${saved}`);
  } catch (err) {
    setStatus(`另存失败：${err}`);
  }
}

function markInterruptedTransfers(reason) {
  for (const item of state.transfers.values()) {
    if (item.done_chunks < item.chunk_count) {
      item.status = "failed";
      item.error = reason;
    }
  }
  renderTransfers();
}

function markLastOutgoingFailed(path, err) {
  const item = Array.from(state.transfers.values())
    .filter((transfer) => transfer.direction === "outgoing" && transfer.done_chunks < transfer.chunk_count)
    .at(-1);
  if (!item) return;
  item.path = path;
  item.status = "failed";
  item.error = text(err);
  renderTransfers();
}

async function retryTransfer(item) {
  const missing = missingChunks(item);
  if (!missing.length) return;
  item.status = "running";
  item.error = "";
  renderTransfers();
  if (item.direction === "outgoing") {
    await call("resume_file_transfer", { fileId: item.file_id, missingChunks: missing });
  } else {
    await call("request_file_resume", { fileId: item.file_id, missingChunks: missing, targetDeviceId: null });
  }
}

async function checkForUpdate({ silent = false } = {}) {
  try {
    if (!silent) setStatus("正在检查更新...");
    const update = await call("check_update");
    if (!update) {
      if (!silent) setStatus("已是最新版本");
      return;
    }

    const notes = text(update.body).trim();
    const message = [
      `发现新版本 ${update.version}`,
      update.date ? `发布时间：${update.date}` : "",
      notes ? `\n更新说明：\n${notes.slice(0, 600)}` : "",
      "\n是否现在下载并安装？安装时程序会退出。",
    ].filter(Boolean).join("\n");

    if (!confirm(message)) {
      setStatus(`发现新版本 ${update.version}，已暂不安装`);
      return;
    }

    setStatus(`正在下载更新 ${update.version}...`);
    await call("install_update");
  } catch (err) {
    if (!silent) setStatus(`检查更新失败：${err}`);
  }
}

function renderTransfers() {
  transfers.innerHTML = "";
  for (const item of state.transfers.values()) {
    const done = item.chunk_count ? Math.round((item.done_chunks / item.chunk_count) * 100) : 0;
    const node = document.createElement("div");
    node.className = "item";
    const title = document.createElement("div");
    const detail = document.createElement("div");
    const progress = document.createElement("progress");
    title.className = "title";
    title.textContent = `${item.direction === "incoming" ? "接收" : "发送"} ${short(item.file_id)} · ${done}%`;
    detail.className = "muted";
    detail.textContent = `${item.done_chunks}/${item.chunk_count} 分片 · ${formatBytes(transferredBytes(item))}/${formatBytes(
      item.total_size,
    )} · ${formatBytes(Math.round(transferSpeed(item)))}/s${item.path ? ` · ${item.path}` : ""}${item.error ? ` · ${item.error}` : ""}`;
    progress.max = item.chunk_count || 1;
    progress.value = item.done_chunks || 0;
    node.append(title, detail, progress);
    if (item.direction === "incoming" && item.status === "done" && item.path) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "secondary";
      button.textContent = "另存为";
      button.addEventListener("click", () => saveReceivedFile(item));
      node.append(button);
    }
    if (item.status === "failed") {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "secondary";
      button.textContent = item.direction === "incoming" ? "请求续传" : "重试续传";
      button.addEventListener("click", async () => {
        try {
          await retryTransfer(item);
        } catch (err) {
          item.status = "failed";
          item.error = text(err);
          renderTransfers();
        }
      });
      node.append(button);
    }
    transfers.append(node);
  }
  if (!transfers.children.length) transfers.append(emptyItem("暂无传输。"));
}

$("open-create").addEventListener("click", () => showDialog("create-dialog"));
$("open-join").addEventListener("click", () => openJoinDialog());
$("close-create").addEventListener("click", () => closeDialog("create-dialog"));
$("close-join").addEventListener("click", () => closeDialog("join-dialog"));
$("close-share").addEventListener("click", () => closeDialog("share-dialog"));
$("leave-button").addEventListener("click", closeCurrentSession);
$("share-button").addEventListener("click", openShareDialog);
$("check-update").addEventListener("click", () => checkForUpdate());
$("parse-share-code").addEventListener("click", parseShareIntoJoinForm);
$("copy-share-code").addEventListener("click", () => copyText($("share-code-output").value));
$("copy-share-text").addEventListener("click", () => copyText(shareText(sharePayload())));

$("create-group").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    const groupName = $("group-name").value.trim() || "LAN Mesh";
    const session = await call("create_group", {
      deviceId: null,
      groupId: null,
      groupName,
      bindAddr: bindAddr(),
    });
    closeDialog("create-dialog");
    setSession(session, groupName);
  } catch (err) {
    setStatus(`创建失败：${err}`);
  }
});

$("discover-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    state.relays = await call("discover_relays", { bindAddr: $("discover-bind").value, durationMs: 1000 });
    renderRelays(state.relays);
    renderSessionList();
  } catch (err) {
    setStatus(`发现失败：${err}`);
  }
});

$("manual-join").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await join($("manual-group-id").value, $("manual-relay-addr").value, selectedLocalIp());
    closeDialog("join-dialog");
  } catch (err) {
    setStatus(`加入失败：${err}`);
  }
});

$("send-text").addEventListener("submit", async (event) => {
  event.preventDefault();
  const content = $("message-input").value.trim();
  if (!content || !state.selected) return;
  const item = { mine: true, from: state.session?.device_id, content, status: "发送中", kind: "text" };
  pushMessage(activeMessages(), item);
  $("message-input").value = "";
  try {
    item.messageId =
      state.selected.type === "group"
        ? await call("send_group_text", { content })
        : await call("send_direct_text", { targetDeviceId: state.selected.id, content });
    item.status = "已送达";
  } catch (err) {
    item.status = `失败：${err}`;
  }
  renderMessages();
});

$("pick-file").addEventListener("click", async () => {
  try {
    $("file-path").value = await call("pick_file");
  } catch (err) {
    setStatus(`选择文件失败：${err}`);
  }
});

$("send-file").addEventListener("submit", async (event) => {
  event.preventDefault();
  const path = $("file-path").value.trim();
  if (!path || !state.selected) {
    setStatus("请先选择会话和文件。");
    return;
  }
  const item = { mine: true, from: state.session?.device_id, content: path, status: "发送中", kind: "file" };
  pushMessage(activeMessages(), item);
  try {
    const sent = await call("send_file", {
      path,
      targetDeviceId: state.selected.type === "direct" ? state.selected.id : null,
    });
    item.status = `已送达 ${sent.chunk_count} 分片`;
  } catch (err) {
    item.status = `失败：${err}`;
    markLastOutgoingFailed(path, err);
  }
  renderMessages();
});

$("join-interface").addEventListener("change", () => {
  $("manual-local-ip").value = $("join-interface").value;
});

if (listen) {
  for (const name of [
    "mesh://neighbor-online",
    "mesh://neighbor-offline",
    "mesh://message-received",
    "mesh://member-changed",
    "mesh://transfer-progress",
    "mesh://update-progress",
  ]) {
    listen(name, ({ payload }) => {
      log(name, payload);
      if (name === "mesh://message-received") addIncoming(payload.message);
      if (name === "mesh://neighbor-offline") {
        markInterruptedTransfers("连接中断");
        if (state.session?.role === "leaf") {
          const groupId = state.session.group_id;
          forgetRelayGroup(groupId);
          clearSession();
          setStatus("群组已断开");
          return;
        }
      }
      if (name === "mesh://member-changed" || name.includes("neighbor")) refreshMembers();
      if (name === "mesh://transfer-progress") rememberTransfer(payload);
      if (name === "mesh://update-progress") {
        const total = payload.contentLength ? `/${formatBytes(payload.contentLength)}` : "";
        setStatus(payload.finished ? "更新已下载，正在安装..." : `更新下载中：${formatBytes(payload.downloaded)}${total}`);
      }
    });
  }
}

renderAll();
loadNetworkInterfaces();
checkForUpdate({ silent: true });
