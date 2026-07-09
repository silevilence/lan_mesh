const tauri = window.__TAURI__;
const invoke = tauri?.core?.invoke ?? tauri?.invoke;
const listen = tauri?.event?.listen;

const state = {
  session: null,
  view: "group",
  directTarget: "",
  groupMessages: [],
  directMessages: new Map(),
  transfers: new Map(),
};

const $ = (id) => document.querySelector(`#${id}`);
const status = $("status");
const events = $("events");
const relays = $("relays");
const members = $("members");
const messages = $("messages");
const transfers = $("transfers");

const text = (value) => String(value ?? "");
const short = (value) => text(value).slice(0, 8);
const headerOf = (message) => message?.header ?? {};
const payloadOf = (message) => message?.payload ?? {};
const sourceOf = (message) => headerOf(message).source_device_id ?? headerOf(message).sourceDeviceId;
const targetOf = (message) => headerOf(message).target ?? {};

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

function bindAddr() {
  return $("create-bind-preset").value || $("create-bind").value || "0.0.0.0:0";
}

function setSession(session) {
  state.session = session;
  setStatus(
    `${session.role === "relay" ? "Relay" : "Leaf"} device=${short(session.device_id)} group=${short(session.group_id)} ${
      session.bind_addr ? `addr=${session.bind_addr}` : ""
    }`,
  );
  $("manual-group-id").value = session.group_id;
  refreshMembers();
}

function renderRelays(items) {
  relays.innerHTML = "";
  if (!items.length) {
    const empty = document.createElement("div");
    empty.className = "muted";
    empty.textContent = "未发现 Relay";
    relays.append(empty);
    return;
  }
  for (const relay of items) {
    const node = document.createElement("div");
    node.className = "item";
    const title = document.createElement("strong");
    const group = document.createElement("div");
    const addr = document.createElement("div");
    const button = document.createElement("button");
    title.textContent = relay.group_name || "LAN Mesh";
    group.className = "muted";
    group.textContent = `group=${relay.group_id}`;
    addr.className = "muted";
    addr.textContent = `relay=${relay.tcp_addr}`;
    button.type = "button";
    button.textContent = "加入";
    button.addEventListener("click", () => join(relay.group_id, relay.tcp_addr));
    node.append(title, group, addr, button);
    relays.append(node);
  }
}

function routeLabel(member, routes, selfId) {
  if (member.device_id === selfId) return "本机";
  const route = routes.find((item) => item.target_device_id === member.device_id);
  if (!member.online) return "离线";
  if (!route) return "可达状态未知";
  return route.path.length <= 2 ? "直连可达" : `多跳可达(${route.path.length - 1}跳)`;
}

async function refreshMembers() {
  if (!state.session) return;
  const [memberList, statusSnapshot] = await Promise.all([
    call("get_members"),
    call("get_connection_status"),
  ]);
  members.innerHTML = "";
  for (const member of memberList.sort((a, b) => text(a.device_id).localeCompare(text(b.device_id)))) {
    const node = document.createElement("div");
    node.className = "item";
    const title = document.createElement("strong");
    const online = document.createElement("span");
    const route = document.createElement("div");
    const button = document.createElement("button");
    title.textContent = short(member.device_id);
    online.textContent = member.online ? "在线" : "离线";
    route.className = "muted";
    route.textContent = routeLabel(member, statusSnapshot.routes, statusSnapshot.device_id);
    button.type = "button";
    button.className = "link";
    button.textContent = "单聊";
    button.disabled = member.device_id === statusSnapshot.device_id;
    button.addEventListener("click", () => openDirect(member.device_id));
    node.append(title, " ", online, route, button);
    members.append(node);
  }
}

function openGroup() {
  state.view = "group";
  $("group-tab").setAttribute("aria-selected", "true");
  $("direct-tab").setAttribute("aria-selected", "false");
  $("chat-title").textContent = "群聊";
  renderMessages();
}

function openDirect(deviceId) {
  state.view = "direct";
  state.directTarget = deviceId;
  $("group-tab").setAttribute("aria-selected", "false");
  $("direct-tab").setAttribute("aria-selected", "true");
  $("direct-tab").textContent = `单聊：${short(deviceId)}`;
  $("chat-title").textContent = `单聊 ${deviceId}`;
  if (!state.directMessages.has(deviceId)) state.directMessages.set(deviceId, []);
  renderMessages();
}

function activeMessages() {
  if (state.view === "group") return state.groupMessages;
  if (!state.directMessages.has(state.directTarget)) state.directMessages.set(state.directTarget, []);
  return state.directMessages.get(state.directTarget);
}

function pushMessage(list, item) {
  item.at ??= Date.now();
  list.push(item);
  list.sort((a, b) => a.at - b.at);
  renderMessages();
  return item;
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
    messages.append(node);
  }
  messages.scrollTop = messages.scrollHeight;
}

function addIncoming(message) {
  if (!message) return;
  const type = message.type;
  const source = sourceOf(message);
  const target = targetOf(message);
  const payload = payloadOf(message);
  const item = {
    from: source,
    mine: source === state.session?.device_id,
    status: "已送达",
    content: type === "text" ? payload.content : `文件分片 ${payload.file_id || ""}`,
    kind: type === "file_chunk" ? "file" : "text",
    at: headerOf(message).timestamp_ms || Date.now(),
  };

  if (target.kind === "device" || target.device_id || target.deviceId) {
    const peer = source === state.session?.device_id ? target.device_id ?? target.deviceId : source;
    if (!state.directMessages.has(peer)) state.directMessages.set(peer, []);
    pushMessage(state.directMessages.get(peer), item);
    return;
  }
  pushMessage(state.groupMessages, item);
}

function renderTransfers() {
  transfers.innerHTML = "";
  for (const item of state.transfers.values()) {
    const done = item.chunk_count ? Math.round((item.done_chunks / item.chunk_count) * 100) : 0;
    const node = document.createElement("div");
    node.className = "item";
    node.textContent = `${item.direction} ${short(item.file_id)} ${done}% (${item.done_chunks}/${item.chunk_count})`;
    transfers.append(node);
  }
}

async function join(groupId, relayAddr, localIp = "") {
  const session = await call("join_group", {
    deviceId: null,
    groupId,
    relayAddr,
    localIp: localIp || null,
  });
  setSession(session);
}

$("create-group").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    const session = await call("create_group", {
      deviceId: null,
      groupId: null,
      groupName: $("group-name").value,
      bindAddr: bindAddr(),
    });
    setSession(session);
  } catch (err) {
    setStatus(`创建失败：${err}`);
  }
});

$("discover-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    renderRelays(await call("discover_relays", { bindAddr: $("discover-bind").value, durationMs: 1000 }));
  } catch (err) {
    setStatus(`发现失败：${err}`);
  }
});

$("manual-join").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await join($("manual-group-id").value, $("manual-relay-addr").value, $("manual-local-ip").value);
  } catch (err) {
    setStatus(`加入失败：${err}`);
  }
});

$("group-tab").addEventListener("click", openGroup);
$("direct-tab").addEventListener("click", () => state.directTarget && openDirect(state.directTarget));

$("send-text").addEventListener("submit", async (event) => {
  event.preventDefault();
  const content = $("message-input").value.trim();
  if (!content) return;
  const list = activeMessages();
  const item = { mine: true, from: state.session?.device_id, content, status: "发送中", kind: "text" };
  pushMessage(list, item);
  $("message-input").value = "";
  try {
    item.messageId =
      state.view === "group"
        ? await call("send_group_text", { content })
        : await call("send_direct_text", { targetDeviceId: state.directTarget, content });
    item.status = "已送达";
  } catch (err) {
    item.status = `失败：${err}`;
  }
  renderMessages();
});

$("file-input").addEventListener("change", (event) => {
  const file = event.target.files?.[0];
  $("file-path").value = file?.path || "";
  if (file && !file.path) setStatus("已选择文件；当前环境未暴露路径，请手动填写文件绝对路径。");
});

$("send-file").addEventListener("submit", async (event) => {
  event.preventDefault();
  const path = $("file-path").value.trim();
  if (!path) {
    setStatus("请先选择文件，或手动填写文件绝对路径。");
    return;
  }
  const item = { mine: true, from: state.session?.device_id, content: path, status: "发送中", kind: "file" };
  pushMessage(activeMessages(), item);
  try {
    const sent = await call("send_file", {
      path,
      targetDeviceId: state.view === "direct" ? state.directTarget : null,
    });
    item.status = `已送达 ${sent.chunk_count} 分片`;
  } catch (err) {
    item.status = `失败：${err}`;
  }
  renderMessages();
});

if (listen) {
  for (const name of [
    "mesh://neighbor-online",
    "mesh://neighbor-offline",
    "mesh://message-received",
    "mesh://member-changed",
    "mesh://transfer-progress",
  ]) {
    listen(name, ({ payload }) => {
      log(name, payload);
      if (name === "mesh://message-received") addIncoming(payload.message);
      if (name === "mesh://member-changed" || name.includes("neighbor")) refreshMembers();
      if (name === "mesh://transfer-progress") {
        state.transfers.set(payload.file_id, payload);
        renderTransfers();
      }
    });
  }
}

openGroup();
