import {
  cleanNickname,
  conversationPresentation,
  displayName as formatDisplayName,
  rememberNickname as cacheNickname,
  routeLabel as formatRouteLabel,
  syncMemberNicknames,
  visibleMembers,
} from "./ui-model.mjs";

// This no-bundler entry point remains intentionally centralized while the UI
// stays small; split it into native ES modules when another independent view
// or a second notification workflow is added.
const tauri = window.__TAURI__;
const invoke = tauri?.core?.invoke ?? tauri?.invoke;
const listen = tauri?.event?.listen;

document.querySelectorAll("form,input").forEach((node) => {
  node.autocomplete = "off";
});

const state = {
  session: null,
  selected: null,
  view: "chat",
  settings: loadSettings(),
  nicknames: new Map(),
  members: [],
  savedGroups: [],
  routes: [],
  relays: [],
  networkInterfaces: [],
  pendingJoinGroupName: "",
  groupMessages: [],
  directMessages: new Map(),
  transfers: new Map(),
  deferredUpdateMessages: new Map(),
  deferredUpdateReady: new Map(),
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
const nicknameOf = (payload) => payload?.sender_nickname ?? payload?.senderNickname ?? "";

function loadSettings() {
  try {
    return {
      sendKey: "ctrl_enter",
      nickname: "",
      closeToTray: true,
      notificationsEnabled: true,
      retainInstaller: true,
      trayNoticeAcknowledged: false,
      ...JSON.parse(localStorage.getItem("lanMeshSettings") || "{}"),
    };
  } catch {
    return { sendKey: "ctrl_enter", nickname: "", closeToTray: true, notificationsEnabled: true, retainInstaller: true, trayNoticeAcknowledged: false };
  }
}

function saveSettings() {
  localStorage.setItem("lanMeshSettings", JSON.stringify(state.settings));
  $("nickname").value = state.settings.nickname;
  $("send-key").value = state.settings.sendKey;
  $("close-to-tray").checked = state.settings.closeToTray;
  $("notifications-enabled").checked = state.settings.notificationsEnabled;
  $("retain-installer").checked = state.settings.retainInstaller;
  void syncDesktopSettings();
  renderSendHint();
  renderAll();
}

async function syncDesktopSettings() {
  try {
    await Promise.all([
      call("set_close_to_tray", { enabled: state.settings.closeToTray }),
      call("set_notifications_enabled", { enabled: state.settings.notificationsEnabled }),
      call("set_retain_installer", { enabled: state.settings.retainInstaller }),
    ]);
  } catch (err) {
    log("sync desktop settings failed", text(err));
  }
}

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

function rememberNickname(deviceId, nickname) {
  return cacheNickname(state.nicknames, deviceId, nickname);
}

function displayName(deviceId) {
  return formatDisplayName(deviceId, {
    sessionDeviceId: state.session?.device_id,
    settingsNickname: state.settings.nickname,
    nicknames: state.nicknames,
  });
}

function senderNickname() {
  return cleanNickname(state.settings.nickname) || null;
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
  restoreDeferredUpdates(session.group_id);
  rememberNickname(session.device_id, state.settings.nickname);
  setStatus(sessionLabel());
  $("manual-group-id").value = session.group_id;
  $("manual-group-name").value = session.group_name;
  refreshMembers();
  void announceNickname();
  loadSavedGroups();
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

async function loadSavedGroups() {
  try {
    state.savedGroups = await call("list_saved_groups");
    renderSessionList();
  } catch (err) {
    log("list_saved_groups failed", text(err));
  }
}

function forgetRelayGroup(groupId) {
  state.relays = state.relays.filter((relay) => relay.group_id !== groupId);
}

function renderAll() {
  renderView();
  renderSessionList();
  renderMembers();
  renderConversation();
  renderTransfers();
}

function renderView() {
  $("chat-view").classList.toggle("hidden", state.view !== "chat");
  $("settings-view").classList.toggle("hidden", state.view !== "settings");
  $("open-chat-view").classList.toggle("active", state.view === "chat");
  $("open-chat-view").classList.toggle("secondary", state.view !== "chat");
  $("open-settings-view").classList.toggle("active", state.view === "settings");
  $("open-settings-view").classList.toggle("secondary", state.view !== "settings");
}

function renderSendHint() {
  $("send-hint").textContent =
    state.settings.sendKey === "enter"
      ? "Enter 发送；Shift+Enter / Ctrl+Enter 换行。"
      : "Ctrl+Enter 发送；Enter / Shift+Enter 换行。";
}

function renderSessionList() {
  sessionList.innerHTML = "";

  for (const group of state.savedGroups) {
    const node = document.createElement("div");
    node.className = `item ${group.group_id === state.session?.group_id ? "active" : ""}`;
    const head = document.createElement("div");
    head.className = "item-head";
    const title = document.createElement("span");
    title.className = "title";
    title.textContent = group.group_name || "LAN Mesh";
    const badge = document.createElement("span");
    badge.className = `badge ${group.status === "connected" ? "online" : ""}`;
    badge.textContent = group.status === "connected" ? (group.role === "relay" ? "我创建的" : "已连接") : "无法联通";
    head.append(title, badge);
    const detail = document.createElement("div");
    detail.className = "muted";
    detail.textContent = `Group ${short(group.group_id)} · ${group.role === "relay" ? group.bind_addr || "Relay" : group.relay_addr || "Leaf"}`;
    node.append(head, detail);
    if (group.last_error && group.status === "unreachable") {
      const error = document.createElement("div");
      error.className = "muted";
      error.textContent = group.last_error;
      node.append(error);
    }
    const actions = document.createElement("div");
    actions.className = "item-actions";
    if (group.status === "connected") {
      const open = document.createElement("button");
      open.type = "button";
      open.className = "secondary mini";
      open.textContent = group.group_id === state.session?.group_id ? "当前群组" : "打开";
      open.disabled = group.group_id === state.session?.group_id;
      open.addEventListener("click", () => activateSavedGroup(group));
      actions.append(open);
    } else {
      const retry = document.createElement("button");
      retry.type = "button";
      retry.className = "secondary mini";
      retry.textContent = "尝试重连";
      retry.addEventListener("click", () => retrySavedGroup(group));
      actions.append(retry);
    }
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger mini";
    remove.textContent = "删除";
    remove.addEventListener("click", () => deleteSavedGroup(group));
    actions.append(remove);
    node.append(actions);
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

  for (const member of visibleMembers(state.members)) {
    const isSelf = member.device_id === state.session.device_id;
    const node = document.createElement(isSelf ? "div" : "button");
    if (!isSelf) node.type = "button";
    node.className = `item ${isSelf ? "" : "clickable"} ${state.selected?.type === "direct" && state.selected.id === member.device_id ? "active" : ""}`;
    node.innerHTML = `
      <div class="item-head">
        <span class="title">${isSelf ? `我 · ${displayName(member.device_id)}` : displayName(member.device_id)}</span>
        <span class="badge ${member.online ? "online" : ""}">${member.online ? "在线" : "离线"}</span>
      </div>
      <div class="muted">${routeLabel(member)}</div>
    `;
    if (!isSelf) node.addEventListener("click", () => openDirect(member.device_id));
    memberList.append(node);
  }

  if (!state.members.length) memberList.append(emptyItem("暂无成员。"));
}

function emptyItem(content) {
  const node = document.createElement("div");
  node.className = "item muted";
  node.textContent = content;
  return node;
}

function routeLabel(member) {
  return formatRouteLabel(member, state.routes);
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
  const presentation = conversationPresentation(state.session, state.selected, displayName);
  $("peer-title").textContent = presentation.title;
  $("peer-subtitle").textContent = isGroup ? sessionLabel() : state.selected.id;
  $("leave-button").textContent = presentation.leaveLabel;
  $("share-button").classList.toggle("hidden", !isGroup);
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
    content.textContent = `${item.kind === "file" ? "📎 " : item.kind === "update" ? "⬆️ " : ""}${item.content}`;
    meta.className = "muted";
    meta.textContent = `${item.mine ? `我(${displayName(item.from)})` : displayName(item.from)} · ${new Date(item.at).toLocaleTimeString()} · ${item.status}`;
    node.append(content, meta);
    if (item.kind === "file" && item.path && !item.mine) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "secondary mini";
      button.textContent = "另存为";
      button.addEventListener("click", () => saveReceivedFile(item));
      node.append(button);
    }
    if (item.kind === "update" && item.ready && !item.mine) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "secondary mini";
      button.textContent = "安装更新";
      button.addEventListener("click", () => installReceivedUpdate(item));
      node.append(button);
    }
    messages.append(node);
  }
  messages.scrollTop = messages.scrollHeight;
}

function addIncoming(message) {
  if (!message || !state.session) return;
  if (message.type !== "text" && message.type !== "update_package") return;
  const source = sourceOf(message);
  const target = targetOf(message);
  const payload = payloadOf(message);
  const learnedNickname = rememberNickname(source, nicknameOf(payload));
  const item = {
    from: source,
    mine: source === state.session.device_id,
    status: "已送达",
    content: message.type === "text" ? payload.content : `更新包 v${payload.version} · ${payload.file_name}`,
    kind: message.type === "update_package" ? "update" : "text",
    file_id: payload.file_id,
    version: payload.version,
    ready: false,
    at: headerOf(message).timestamp_ms || Date.now(),
  };

  if (target.kind === "device" || target.device_id || target.deviceId) {
    const peer = source === state.session.device_id ? target.device_id ?? target.deviceId : source;
    if (!state.directMessages.has(peer)) state.directMessages.set(peer, []);
    pushMessage(state.directMessages.get(peer), item);
    if (learnedNickname) renderAll();
    return;
  }
  pushMessage(state.groupMessages, item);
  if (learnedNickname) renderAll();
}

async function installReceivedUpdate(item) {
  const source = item.from ? short(item.from) : "未知设备";
  const message = [
    `确认安装来自局域网设备 ${source} 的更新包 v${item.version}？`,
    "程序将退出并启动本地安装程序。",
    "该包仅完成完整性校验，未验证发布签名；请仅在信任来源时继续。",
  ].join("\n\n");
  if (!confirm(message)) return;
  try {
    await call("install_received_update_package", { fileId: item.file_id });
  } catch (err) {
    setStatus(`无法安装更新包：${err}`);
  }
}

function markUpdatePackageReady(payload) {
  const messagesByConversation = [state.groupMessages, ...state.directMessages.values()];
  for (const list of messagesByConversation) {
    const item = list.find((message) => message.kind === "update" && message.file_id === payload.file_id);
    if (item) {
      item.ready = true;
      item.status = "已校验，可安装";
      renderMessages();
      return;
    }
  }
}

function deferUpdateMessage(groupId, message) {
  const pending = state.deferredUpdateMessages.get(groupId) || [];
  const fileId = payloadOf(message).file_id;
  if (!pending.some((item) => payloadOf(item).file_id === fileId)) pending.push(message);
  state.deferredUpdateMessages.set(groupId, pending);
}

function deferUpdateReady(payload) {
  const pending = state.deferredUpdateReady.get(payload.group_id) || new Map();
  pending.set(payload.file_id, payload);
  state.deferredUpdateReady.set(payload.group_id, pending);
}

function restoreDeferredUpdates(groupId) {
  const messages = state.deferredUpdateMessages.get(groupId) || [];
  state.deferredUpdateMessages.delete(groupId);
  for (const message of messages) addIncoming(message);

  const ready = state.deferredUpdateReady.get(groupId);
  state.deferredUpdateReady.delete(groupId);
  if (ready) {
    for (const payload of ready.values()) markUpdatePackageReady(payload);
  }
}

async function refreshMembers() {
  if (!state.session) return;
  const [memberList, statusSnapshot] = await Promise.all([
    call("get_members"),
    call("get_connection_status"),
  ]);
  syncMemberNicknames(state.nicknames, memberList);
  state.members = memberList.sort((a, b) => text(a.device_id).localeCompare(text(b.device_id)));
  state.routes = statusSnapshot.routes;
  renderAll();
}

async function announceNickname() {
  if (!state.session) return;
  try {
    await call("announce_nickname", { nickname: senderNickname() });
    await refreshMembers();
  } catch (err) {
    log("announce_nickname failed", text(err));
  }
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
  if (state.session?.role !== "relay") {
    return state.session?.relay_addr ? [{ name: "群主 Relay", ip: parseHostPort(state.session.relay_addr).host, addr: state.session.relay_addr }] : [];
  }
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
    groupName: $("manual-group-name").value.trim() || null,
    relayAddr,
    localIp: localIp || null,
  });
  session.relay_addr = relayAddr;
  setSession(session, $("manual-group-name").value.trim());
}

async function activateSavedGroup(group) {
  try {
    const active = await call("activate_saved_group", { groupId: group.group_id });
    setSession(active, active.group_name);
  } catch (err) {
    setStatus(`打开群组失败：${err}`);
    loadSavedGroups();
  }
}

async function retrySavedGroup(group) {
  try {
    const active = await call("retry_saved_group", { groupId: group.group_id });
    setStatus(`已重新连接 ${active.group_name}`);
    setSession(active, active.group_name);
  } catch (err) {
    setStatus(`重连失败：${err}`);
    loadSavedGroups();
  }
}

async function deleteSavedGroup(group) {
  if (!confirm(`删除“${group.group_name}”的本地记录？`)) return;
  try {
    await call("delete_saved_group", { groupId: group.group_id });
    if (group.group_id === state.session?.group_id) clearSession();
    await loadSavedGroups();
    setStatus("已删除群组记录");
  } catch (err) {
    setStatus(`删除失败：${err}`);
  }
}

async function closeCurrentSession() {
  if (!state.session) return;
  const action = state.session.role === "relay" ? "解散群组" : "退出群组";
  const groupId = state.session.group_id;
  if (!confirm(`${action}后会断开当前会话，继续？`)) return;
  await call("close_session");
  forgetRelayGroup(groupId);
  clearSession();
  loadSavedGroups();
}

function leaveConversation() {
  const action = conversationPresentation(state.session, state.selected, displayName).leaveAction;
  if (action === "open-group") {
    openGroup();
    return;
  }
  void closeCurrentSession();
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
  rememberNickname(payload.from, nicknameOf(payload));
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

async function openShareUpdateDialog() {
  try {
    const update = await call("get_retained_update_package");
    if (!update) {
      setStatus("请先检查更新获取安装包");
      return;
    }
    $("share-update-summary").textContent = `将分享更新包 v${update.version} · ${update.fileName} · ${formatBytes(update.totalSize)}`;
    const groups = $("share-update-groups");
    groups.innerHTML = "";
    for (const group of state.savedGroups) {
      const label = document.createElement("label");
      label.className = "check-row item";
      const input = document.createElement("input");
      input.type = "checkbox";
      input.name = "share-update-group";
      input.value = group.group_id;
      input.disabled = group.status !== "connected";
      const name = document.createElement("span");
      name.textContent = `${group.group_name} · ${group.status === "connected" ? "可发送" : "当前不可连接"}`;
      label.append(input, name);
      groups.append(label);
    }
    if (!groups.children.length) groups.append(emptyItem("没有可分享的群组。"));
    showDialog("share-update-dialog");
  } catch (err) {
    setStatus(`无法读取保留安装包：${err}`);
  }
}

async function shareRetainedUpdatePackage(event) {
  event.preventDefault();
  const groupIds = [...document.querySelectorAll('input[name="share-update-group"]:checked')].map((input) => input.value);
  try {
    const result = await call("share_retained_update_package", { groupIds });
    closeDialog("share-update-dialog");
    const failed = result.failed_groups?.length || result.failedGroups?.length || 0;
    setStatus(`已向 ${result.shared.length} 个群组发送更新包${failed ? `，${failed} 个群组发送失败` : ""}`);
  } catch (err) {
    setStatus(`分享更新包失败：${err}`);
  }
}

async function openNotificationTarget(data = {}) {
  await call("open_main_window");
  const group = state.savedGroups.find((item) => item.group_id === data.groupId);
  if (group && group.group_id !== state.session?.group_id) await activateSavedGroup(group);
  if (data.deviceId) openDirect(data.deviceId);
  else openGroup();
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
$("close-share-update").addEventListener("click", () => closeDialog("share-update-dialog"));
$("leave-button").addEventListener("click", leaveConversation);
$("share-button").addEventListener("click", openShareDialog);
$("check-update").addEventListener("click", () => checkForUpdate());
$("share-update-package").addEventListener("click", () => openShareUpdateDialog());
$("share-update-form").addEventListener("submit", shareRetainedUpdatePackage);
$("parse-share-code").addEventListener("click", parseShareIntoJoinForm);
$("copy-share-code").addEventListener("click", () => copyText($("share-code-output").value));
$("copy-share-text").addEventListener("click", () => copyText(shareText(sharePayload())));
$("open-chat-view").addEventListener("click", () => {
  state.view = "chat";
  renderAll();
});
$("open-settings-view").addEventListener("click", () => {
  state.view = "settings";
  renderAll();
});
$("save-settings").addEventListener("click", () => {
  state.settings.nickname = cleanNickname($("nickname").value);
  state.settings.sendKey = $("send-key").value;
  state.settings.closeToTray = $("close-to-tray").checked;
  state.settings.notificationsEnabled = $("notifications-enabled").checked;
  state.settings.retainInstaller = $("retain-installer").checked;
  saveSettings();
  void announceNickname();
  setStatus("配置已保存");
});

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
        ? await call("send_group_text", { content, senderNickname: senderNickname() })
        : await call("send_direct_text", { targetDeviceId: state.selected.id, content, senderNickname: senderNickname() });
    item.status = "已送达";
  } catch (err) {
    item.status = `失败：${err}`;
  }
  renderMessages();
});

$("message-input").addEventListener("keydown", (event) => {
  if (event.shiftKey) return;
  const sendByEnter = state.settings.sendKey === "enter" && event.key === "Enter" && !event.ctrlKey;
  const sendByCtrlEnter = state.settings.sendKey === "ctrl_enter" && event.key === "Enter" && event.ctrlKey;
  if (!sendByEnter && !sendByCtrlEnter) return;
  event.preventDefault();
  $("send-text").requestSubmit();
});

$("pick-file").addEventListener("click", async () => {
  try {
    $("file-path").value = await call("pick_file");
  } catch (err) {
    setStatus(`选择文件失败：${err}`);
  }
});

async function sendOneFile(path) {
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
      senderNickname: senderNickname(),
    });
    item.status = `已送达 ${sent.chunk_count} 分片`;
  } catch (err) {
    item.status = `失败：${err}`;
    markLastOutgoingFailed(path, err);
  }
  renderMessages();
}

async function sendFiles(paths) {
  const uniquePaths = [...new Set(paths.filter(Boolean))];
  if (!uniquePaths.length) return;
  if (!state.selected) {
    setStatus("请先选择会话。");
    return;
  }
  if (!confirm(`确认发送 ${uniquePaths.length} 个文件？\n\n${uniquePaths.join("\n")}`)) return;
  for (const path of uniquePaths) await sendOneFile(path);
}

function pastedImageName(file) {
  const ext = (file.type || "image/png").split("/").at(-1) || "png";
  return file.name || `pasted-image-${new Date().toISOString().replace(/[:.]/g, "-")}.${ext}`;
}

async function fileToPath(file) {
  const path = file?.path || file?.webkitRelativePath;
  if (path) return path;
  const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
  return call("save_temp_file", { fileName: pastedImageName(file), bytes });
}

async function pathsFromDataTransfer(dataTransfer) {
  const files = [...(dataTransfer?.files || [])];
  return Promise.all(files.map(fileToPath));
}

async function handleFileDrop(paths) {
  messages.classList.remove("drag-over");
  await sendFiles(paths);
}

messages.addEventListener("dragover", (event) => {
  event.preventDefault();
  messages.classList.add("drag-over");
});

messages.addEventListener("dragleave", () => messages.classList.remove("drag-over"));

messages.addEventListener("drop", async (event) => {
  event.preventDefault();
  await handleFileDrop(await pathsFromDataTransfer(event.dataTransfer));
});

$("chat-pane").addEventListener("paste", async (event) => {
  const paths = await pathsFromDataTransfer(event.clipboardData);
  if (!paths.length) return;
  event.preventDefault();
  await sendFiles(paths);
});

const currentWebview = tauri?.webview?.getCurrentWebview?.();
currentWebview?.onDragDropEvent?.((event) => {
  const payload = event.payload;
  if (payload?.type === "over") messages.classList.add("drag-over");
  if (payload?.type === "leave") messages.classList.remove("drag-over");
  if (payload?.type === "drop") handleFileDrop(payload.paths || []);
});

$("send-file").addEventListener("submit", async (event) => {
  event.preventDefault();
  const path = $("file-path").value.trim();
  await sendOneFile(path);
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
    "mesh://groups-changed",
    "mesh://update-progress",
    "mesh://update-package-ready",
    "mesh://notification-opened",
  ]) {
    listen(name, ({ payload }) => {
      log(name, payload);
      if (name === "mesh://groups-changed") {
        loadSavedGroups();
        return;
      }
      if (name === "mesh://notification-opened") {
        void openNotificationTarget(payload);
        return;
      }
      if (payload?.group_id && payload.group_id !== state.session?.group_id) {
        if (name === "mesh://message-received" && payload?.message?.type === "update_package") {
          deferUpdateMessage(payload.group_id, payload.message);
        }
        if (name === "mesh://update-package-ready") deferUpdateReady(payload);
        return;
      }
      if (name === "mesh://message-received") addIncoming(payload.message);
      if (name === "mesh://member-changed" && payload.nickname_updated) {
        const nickname = cleanNickname(payload.nickname);
        if (nickname) state.nicknames.set(payload.device_id, nickname);
        else state.nicknames.delete(payload.device_id);
      }
      if (name === "mesh://neighbor-offline") {
        markInterruptedTransfers("连接中断");
        if (state.session?.role === "leaf") setStatus("连接暂时中断，正在后台重连…");
      }
      if (name === "mesh://member-changed" || name.includes("neighbor")) refreshMembers();
      if (name === "mesh://transfer-progress") rememberTransfer(payload);
      if (name === "mesh://update-package-ready") markUpdatePackageReady(payload);
      if (name === "mesh://update-progress") {
        const total = payload.contentLength ? `/${formatBytes(payload.contentLength)}` : "";
        setStatus(payload.finished ? "更新已下载，正在安装..." : `更新下载中：${formatBytes(payload.downloaded)}${total}`);
      }
    });
  }
}

$("nickname").value = state.settings.nickname;
$("send-key").value = state.settings.sendKey;
$("close-to-tray").checked = state.settings.closeToTray;
$("notifications-enabled").checked = state.settings.notificationsEnabled;
$("retain-installer").checked = state.settings.retainInstaller;
renderSendHint();
void syncDesktopSettings();

call("app_version")
  .then((version) => {
    const label = `v${version}`;
    document.title = `LAN Mesh ${label}`;
    $("title-version").textContent = label;
    $("settings-version").textContent = label;
  })
  .catch(() => {});

renderAll();
loadNetworkInterfaces();
loadSavedGroups();
checkForUpdate({ silent: true });
