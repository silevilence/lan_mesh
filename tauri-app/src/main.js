import { createTimeline } from "./chat-timeline.mjs";
import { createSidebar } from "./chat-sidebar.mjs";
import { setupUpdates } from "./update-ui.mjs";
import { $ } from "./dom.mjs";
import { setupGroupForms } from "./group-forms.mjs";
import { showIssue, recordIssue, reportError, setStatus } from "./feedback.mjs";
import {
  cleanNickname,
  conversationPresentation,
  displayName as formatDisplayName,
  rememberNickname as cacheNickname,
  routeLabel as formatRouteLabel,
  syncMemberNicknames,
} from "./ui-model.mjs";

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
  groupMessages: [],
  directMessages: new Map(),
  transfers: new Map(),
  conversations: new Map(),
};

const text = (value) => String(value ?? "");
const short = (value) => text(value).slice(0, 8);
const headerOf = (message) => message?.header ?? {};
const payloadOf = (message) => message?.payload ?? {};
const sourceOf = (message) => headerOf(message).source_device_id ?? headerOf(message).sourceDeviceId;
const targetOf = (message) => headerOf(message).target ?? {};
const groupNameOf = (session) => session?.group_name || "群聊";
const nicknameOf = (payload) => payload?.sender_nickname ?? payload?.senderNickname ?? "";

function loadSettings() {
  const defaults = {
    sendKey: "ctrl_enter", nickname: "", closeToTray: true,
    notificationsEnabled: true, retainInstaller: true,
  };
  try {
    return { ...defaults, ...JSON.parse(localStorage.getItem("lanMeshSettings") || "{}") };
  } catch (err) {
    recordIssue("读取设置失败，已使用默认设置", err);
    return defaults;
  }
}

async function saveSettings() {
  localStorage.setItem("lanMeshSettings", JSON.stringify(state.settings));
  $("nickname").value = state.settings.nickname;
  $("send-key").value = state.settings.sendKey;
  $("close-to-tray").checked = state.settings.closeToTray;
  $("notifications-enabled").checked = state.settings.notificationsEnabled;
  $("retain-installer").checked = state.settings.retainInstaller;
  await syncDesktopSettings();
  renderSendHint();
  renderAll();
}

async function syncDesktopSettings() {
  await Promise.all([
    call("set_close_to_tray", { enabled: state.settings.closeToTray }),
    call("set_notifications_enabled", { enabled: state.settings.notificationsEnabled }),
    call("set_retain_installer", { enabled: state.settings.retainInstaller }),
  ]);
}

const { checkForUpdate, installReceivedUpdate } = setupUpdates({ call, state, formatBytes });
const timeline = createTimeline({ displayName, saveReceivedFile, installReceivedUpdate });
const { renderSessionList, renderMembers, renderTransfers, showChat } = createSidebar({
  state, displayName, routeLabel, openGroup, openDirect, activateSavedGroup, retrySavedGroup,
  deleteSavedGroup, closeCurrentSession, openJoinDialog: (relay) => openJoinDialog(relay), showIssue,
  reportError, saveReceivedFile, retryTransfer, formatBytes, transferredBytes, transferSpeed, markConversationRead,
});
const { openJoinDialog, loadNetworkInterfaces } = setupGroupForms({ state, call, setSession, renderSessionList });
const messages = $("messages");
const events = $("events");

const eventLines = [];
function log(name, payload) {
  const context = [payload?.group_id, payload?.file_id, payload?.message?.type, payload?.status].filter(Boolean);
  eventLines.push([new Date().toLocaleTimeString(), name, ...context].join(" · "));
  if (eventLines.length > 100) eventLines.shift();
  if (events.parentElement.open) events.textContent = eventLines.join("\n");
}
events.parentElement.addEventListener("toggle", () => {
  if (events.parentElement.open) events.textContent = eventLines.join("\n");
});

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

function sessionLabel() {
  if (!state.session) return "未连接";
  return state.session.role === "relay" ? "本机创建的群组" : "已加入群组";
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

function conversationsFor(groupId) {
  if (!state.conversations.has(groupId)) {
    state.conversations.set(groupId, { groupMessages: [], directMessages: new Map(), transfers: new Map(), drafts: new Map(), unread: new Set() });
  }
  return state.conversations.get(groupId);
}

function saveDraft() {
  if (!state.selected) return;
  const key = state.selected.id || "group";
  const message = $("message-input").value;
  const file = $("file-path").value;
  if (message || file) state.drafts.set(key, { message, file });
  else state.drafts.delete(key);
}

function restoreDraft() {
  if ($("file-dialog").open) closeDialog("file-dialog");
  if ($("group-options").open) closeDialog("group-options");
  const draft = state.drafts?.get(state.selected?.id || "group");
  $("message-input").value = draft?.message || "";
  $("file-path").value = draft?.file || "";
}

let navigationVersion = 0;

function setSession(session, groupName = "") {
  navigationVersion++;
  session.group_name = groupName || session.group_name || groupNameOf(session);
  saveDraft();
  state.session = session;
  state.selected = { type: "group" };
  state.members = [];
  state.routes = [];
  Object.assign(state, conversationsFor(session.group_id));
  restoreDraft();
  showChat();
  rememberNickname(session.device_id, state.settings.nickname);
  setStatus("已打开 " + groupNameOf(session));
  void announceNickname();
  loadSavedGroups();
  renderAll();
}

function clearSession() {
  navigationVersion++;
  state.session = null;
  state.selected = null;
  state.members = [];
  state.routes = [];
  state.groupMessages = [];
  state.directMessages = new Map();
  state.transfers = new Map();
  state.drafts = null;
  state.unread = null;
  restoreDraft();
  showChat();
  setStatus("尚未打开群组");
  renderAll();
}

async function loadSavedGroups() {
  try {
    state.savedGroups = await call("list_saved_groups");
    renderSessionList();
  } catch (err) {
    reportError("读取群组列表失败", err);
  }
}

function forgetRelayGroup(groupId) {
  state.relays = state.relays.filter((relay) => relay.group_id !== groupId);
}

function renderAll() {
  renderView();
  markConversationRead();
  renderSessionList();
  renderMembers();
  renderConversation();
  renderTransfers();
}

function renderView() {
  $("chat-view").classList.toggle("hidden", state.view !== "chat");
  $("settings-view").classList.toggle("hidden", state.view !== "settings");
  $("open-chat-view").setAttribute("aria-pressed", String(state.view === "chat"));
  $("open-settings-view").setAttribute("aria-pressed", String(state.view === "settings"));
  $("open-chat-view").classList.toggle("active", state.view === "chat");
  $("open-chat-view").classList.toggle("secondary", state.view !== "chat");
  $("open-settings-view").classList.toggle("active", state.view === "settings");
  $("open-settings-view").classList.toggle("secondary", state.view !== "settings");
}

function renderSendHint() {
  $("send-hint").textContent =
    state.settings.sendKey === "enter"
      ? "Enter 发送，Shift+Enter 或 Ctrl+Enter 换行。"
      : "Ctrl+Enter 发送，Enter 换行。";
}

function routeLabel(member) {
  return formatRouteLabel(member, state.routes);
}

function openGroup() {
  if (!state.session) return;
  navigationVersion++;
  saveDraft();
  state.selected = { type: "group" };
  restoreDraft();
  showChat();
  renderAll();
}

function openDirect(deviceId) {
  if (!state.session || !deviceId) return;
  navigationVersion++;
  saveDraft();
  state.selected = { type: "direct", id: deviceId };
  restoreDraft();
  showChat();
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
  $("chat-view").classList.toggle("has-conversation", hasSelection);
  if (!hasSelection) { timeline.show(null); return; }

  const isGroup = state.selected.type === "group";
  const presentation = conversationPresentation(state.session, state.selected, displayName);
  $("peer-title").textContent = presentation.title;
  $("message-input").placeholder = isGroup ? "写消息给群成员…" : "写消息给" + displayName(state.selected.id) + "…";
  $("peer-subtitle").textContent = isGroup ? sessionLabel() : groupNameOf(state.session) + " · 单聊";
  $("peer-title").title = presentation.title;
  $("chat-avatar").textContent = Array.from(isGroup ? presentation.title : displayName(state.selected.id))[0];
  $("chat-avatar").classList.toggle("person", !isGroup);
  $("chat-options").hidden = !isGroup;
  $("share-button").classList.toggle("hidden", !isGroup);
  $("back-group").hidden = isGroup;
  timeline.show(activeMessages(), isGroup);
  markConversationRead();
}

function markConversationRead() {
  if (!state.selected || state.view !== "chat" || !$("chat-pane").getClientRects().length || document.hidden) return;
  if (state.unread.delete(state.selected.id || "group")) {
    renderSessionList();
    renderMembers();
  }
}

function markUnread(chat, key) {
  if (chat.unread.has(key)) return;
  if (chat.unread === state.unread && (state.selected?.id || "group") === key &&
      state.view === "chat" && $("chat-pane").getClientRects().length && !document.hidden) return;
  chat.unread.add(key);
  renderSessionList();
  if (chat.unread === state.unread) renderMembers();
}

document.addEventListener("visibilitychange", markConversationRead);
function pushMessage(list, item, visible = true) {
  item.at ??= Date.now();
  let index = list.length;
  while (index > 0 && list[index - 1].at > item.at) index--;
  list.splice(index, 0, item);
  if (visible) timeline.insert(list, item, index);
}

function addIncoming(message, groupId) {
  if (!message || !groupId) return;
  if (message.type !== "text" && message.type !== "update_package") return;
  const current = groupId === state.session?.group_id;
  const chat = conversationsFor(groupId);
  const self = current ? state.session.device_id : state.savedGroups.find((group) => group.group_id === groupId)?.device_id;
  const source = sourceOf(message);
  const target = targetOf(message);
  const payload = payloadOf(message);
  const learnedNickname = rememberNickname(source, nicknameOf(payload));
  const item = {
    from: source,
    mine: source === self,
    status: "已接收",
    content: message.type === "text" ? payload.content : `更新包 v${payload.version} · ${payload.file_name}`,
    kind: message.type === "update_package" ? "update" : "text",
    file_id: payload.file_id,
    version: payload.version,
    ready: false,
    at: headerOf(message).timestamp_ms || Date.now(),
  };

  if (target.kind === "device" || target.device_id || target.deviceId) {
    const peer = source === self ? target.device_id ?? target.deviceId : source;
    if (!chat.directMessages.has(peer)) chat.directMessages.set(peer, []);
    pushMessage(chat.directMessages.get(peer), item, current);
    if (!item.mine) markUnread(chat, peer);
    if (current && learnedNickname) { renderMembers(); renderConversation(); }
    return;
  }
  pushMessage(chat.groupMessages, item, current);
  if (!item.mine) markUnread(chat, "group");
  if (current && learnedNickname) { renderMembers(); renderConversation(); }
}

function markUpdatePackageReady(payload) {
  const chat = conversationsFor(payload.group_id);
  const messagesByConversation = [chat.groupMessages, ...chat.directMessages.values()];
  for (const list of messagesByConversation) {
    const item = list.find((message) => message.kind === "update" && message.file_id === payload.file_id);
    if (item) {
      item.ready = true;
      item.status = "文件已校验，可以安装";
      if (payload.group_id === state.session?.group_id) timeline.update(item);
      return;
    }
  }
}

async function refreshMembers() {
  if (!state.session) return;
  const groupId = state.session.group_id;
  try {
    const [memberList, statusSnapshot] = await Promise.all([
      call("get_members", { groupId }),
      call("get_connection_status", { groupId }),
    ]);
    if (state.session?.group_id !== groupId) return;
    syncMemberNicknames(state.nicknames, memberList);
    state.members = memberList.sort((a, b) => text(a.device_id).localeCompare(text(b.device_id)));
    state.routes = statusSnapshot.routes;
    renderMembers();
    renderConversation();
  } catch (err) {
    if (state.session?.group_id === groupId) reportError("读取群成员失败", err);
  }
}

async function announceNickname() {
  if (!state.session) return;
  try {
    await call("announce_nickname", { groupId: state.session.group_id, nickname: senderNickname() });
  } catch (err) {
    reportError("同步昵称失败", err);
  }
  await refreshMembers();
}

async function activateSavedGroup(group) {
  const version = ++navigationVersion;
  try {
    const active = await call("activate_saved_group", { groupId: group.group_id });
    if (version !== navigationVersion) return false;
    setSession(active, active.group_name);
    return true;
  } catch (err) {
    reportError("打开群组失败", err);
    loadSavedGroups();
  }
}

async function retrySavedGroup(group) {
  const version = ++navigationVersion;
  try {
    const active = await call("retry_saved_group", { groupId: group.group_id });
    if (version !== navigationVersion) return false;
    setStatus(`已重新连接 ${active.group_name}`);
    setSession(active, active.group_name);
    return true;
  } catch (err) {
    reportError("重连失败", err);
    loadSavedGroups();
  }
}

async function deleteSavedGroup(group) {
  if (!confirm(`从本机移除“${group.group_name}”？此操作会断开本机与该群的连接，并删除保存的群组记录。`)) return;
  try {
    await call("delete_saved_group", { groupId: group.group_id });
    state.conversations.delete(group.group_id);
    if (group.group_id === state.session?.group_id) clearSession();
    await loadSavedGroups();
    setStatus("已删除群组记录");
  } catch (err) {
    reportError("删除失败", err);
  }
}

async function closeCurrentSession() {
  if (!state.session) return;
  const action = state.session.role === "relay" ? "停止本机群组" : "退出群组";
  const groupId = state.session.group_id;
  const effect = state.session.role === "relay"
    ? "本机将停止为这个群组转发消息，并删除本机的群组记录。"
    : "本机将断开与这个群组的连接，并删除群组记录。";
  if (!confirm(`${action}“${groupNameOf(state.session)}”？\n\n${effect}`)) return;
  await call("close_session", { groupId });
  forgetRelayGroup(groupId);
  state.conversations.delete(groupId);
  if (state.session?.group_id === groupId) clearSession();
  loadSavedGroups();
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
  const chat = conversationsFor(payload.group_id);
  const old = chat.transfers.get(payload.file_id) || { firstSeen: now };
  const chunks = old.chunks || new Set();
  if (payload.status !== "failed") chunks.add(payload.chunk_index);
  if (payload.status === "failed" && payload.error?.startsWith("hash mismatch:")) chunks.clear();
  if (payload.status === "failed" && (old.status !== "failed" || old.error !== payload.error)) {
    recordIssue("文件传输失败 · " + (payload.file_name || payload.file_id), payload.error);
  }
  const done_chunks = chunks.size;
  const status = payload.direction === "outgoing" && payload.status !== "failed"
    ? (done_chunks >= payload.chunk_count ? "done" : "running")
    : payload.status;
  chat.transfers.set(payload.file_id, {
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
  if (payload.group_id === state.session?.group_id) renderTransfers();
}

function addReceivedFile(payload) {
  const chat = conversationsFor(payload.group_id);
  const current = payload.group_id === state.session?.group_id;
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
    if (!chat.directMessages.has(peer)) chat.directMessages.set(peer, []);
    pushMessage(chat.directMessages.get(peer), item, current);
    if (!item.mine) markUnread(chat, peer);
    return;
  }
  pushMessage(chat.groupMessages, item, current);
  if (!item.mine) markUnread(chat, "group");
}

async function saveReceivedFile(item) {
  try {
    const saved = await call("save_file_as", { path: item.path, fileName: item.file_name });
    setStatus(`已另存为：${saved}`);
  } catch (err) {
    if (err !== "未选择保存位置") reportError("另存文件失败", err);
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

async function retryTransfer(item) {
  const missing = missingChunks(item);
  if (!missing.length) return;
  item.status = "running";
  item.error = "";
  renderTransfers();
  const groupId = item.group_id || state.session?.group_id;
  if (!groupId) throw new Error("没有可用于续传的群组");
  if (item.direction === "outgoing") {
    await call("resume_file_transfer", { groupId, fileId: item.file_id, missingChunks: missing });
  } else {
    await call("request_file_resume", { groupId, fileId: item.file_id, missingChunks: missing, targetDeviceId: item.from || null });
  }
}

async function openNotificationTarget(data = {}) {
  const version = ++navigationVersion;
  await call("open_main_window");
  if (!state.savedGroups.some((item) => item.group_id === data.groupId)) await loadSavedGroups();
  const group = state.savedGroups.find((item) => item.group_id === data.groupId);
  if (version !== navigationVersion) return;
  if (group && group.group_id !== state.session?.group_id && !await activateSavedGroup(group)) return;
  if (data.groupId !== state.session?.group_id) return;
  state.view = "chat";
  if (data.deviceId) openDirect(data.deviceId);
  else openGroup();
}

$("welcome-create").onclick = () => showDialog("create-dialog");
$("welcome-join").onclick = () => openJoinDialog();
$("open-create").addEventListener("click", () => showDialog("create-dialog"));
$("open-join").addEventListener("click", () => openJoinDialog());
$("close-create").addEventListener("click", () => closeDialog("create-dialog"));
$("close-join").addEventListener("click", () => closeDialog("join-dialog"));
$("close-share").addEventListener("click", () => closeDialog("share-dialog"));
$("close-share-update").addEventListener("click", () => closeDialog("share-update-dialog"));
$("back-group").onclick = openGroup;
$("attach-file").onclick = () => {
  const name = state.selected.type === "group" ? groupNameOf(state.session) : displayName(state.selected.id);
  $("file-recipient").textContent = "发送给「" + name + "」";
  showDialog("file-dialog");
};
$("close-file").onclick = () => closeDialog("file-dialog");
$("open-chat-view").addEventListener("click", () => {
  state.view = "chat";
  renderAll();
});
$("open-settings-view").addEventListener("click", () => {
  navigationVersion++;
  state.view = "settings";
  renderAll();
});
$("save-settings").addEventListener("click", async () => {
  state.settings.nickname = cleanNickname($("nickname").value);
  state.settings.sendKey = $("send-key").value;
  state.settings.closeToTray = $("close-to-tray").checked;
  state.settings.notificationsEnabled = $("notifications-enabled").checked;
  state.settings.retainInstaller = $("retain-installer").checked;
  try {
    await saveSettings();
  } catch (err) {
    reportError("设置未完全生效，请重新保存", err);
    return;
  }
  void announceNickname();
  setStatus("设置已保存");
});

$("send-text").addEventListener("submit", async (event) => {
  event.preventDefault();
  const content = $("message-input").value.trim();
  if (!content || !state.selected) { $("message-input").focus(); return; }
  const selection = state.selected;
  const item = { mine: true, from: state.session?.device_id, content, status: "发送中", kind: "text" };
  pushMessage(activeMessages(), item);
  $("message-input").value = "";
  try {
    item.messageId =
      state.selected.type === "group"
        ? await call("send_group_text", { groupId: state.session.group_id, content, senderNickname: senderNickname() })
        : await call("send_direct_text", { groupId: state.session.group_id, targetDeviceId: state.selected.id, content, senderNickname: senderNickname() });
    item.status = "已发送";
  } catch (err) {
    item.status = "发送失败";
    item.issue = reportError("消息发送失败", err);
    if (state.selected === selection && !$("message-input").value) $("message-input").value = content;
  }
  timeline.update(item);
});

$("message-input").addEventListener("keydown", (event) => {
  if (event.isComposing || event.keyCode === 229 || event.shiftKey) return;
  if (state.settings.sendKey === "enter" && event.key === "Enter" && event.ctrlKey) {
    event.preventDefault();
    const field = event.currentTarget;
    field.setRangeText("\n", field.selectionStart, field.selectionEnd, "end");
    return;
  }
  const sendByEnter = state.settings.sendKey === "enter" && event.key === "Enter" && !event.ctrlKey;
  const sendByCtrlEnter = state.settings.sendKey === "ctrl_enter" && event.key === "Enter" && event.ctrlKey;
  if (!sendByEnter && !sendByCtrlEnter) return;
  event.preventDefault();
  $("send-text").requestSubmit();
});

$("pick-file").addEventListener("click", async () => {
  const selection = state.selected;
  try {
    const path = await call("pick_file");
    if (state.selected === selection && $("file-dialog").open) $("file-path").value = path;
  } catch (err) {
    if (err !== "未选择文件") reportError("选择文件失败", err, $("file-dialog"));
  }
});

async function sendOneFile(path) {
  if (!path || !state.selected) {
    setStatus("请先选择会话和文件。");
    return;
  }
  const selection = state.selected;
  const item = { mine: true, from: state.session?.device_id, content: path.split(/[\\/]/).at(-1), status: "发送中", kind: "file" };
  pushMessage(activeMessages(), item);
  try {
    await call("send_file", {
      groupId: state.session.group_id,
      path,
      targetDeviceId: state.selected.type === "direct" ? state.selected.id : null,
      senderNickname: senderNickname(),
    });
    item.status = "已发送";
    if (state.selected === selection && $("file-path").value === path) $("file-path").value = "";
  } catch (err) {
    item.status = "发送失败";
    item.issue = reportError("文件发送失败", err);
  }
  timeline.update(item);
}

async function sendFiles(paths) {
  const uniquePaths = [...new Set(paths.filter(Boolean))];
  if (!uniquePaths.length) return;
  if (!state.selected) {
    setStatus("请先选择会话。");
    return;
  }
  const groupId = state.session.group_id;
  const target = state.selected;
  const recipient = target.type === "group" ? groupNameOf(state.session) : displayName(target.id);
  if (!confirm(`向「${recipient}」发送 ${uniquePaths.length} 个文件？\n\n${uniquePaths.join("\n")}`)) return;
  for (const path of uniquePaths) {
    if (state.session?.group_id !== groupId || state.selected !== target) {
      setStatus("聊天对象已切换，剩余文件未发送。");
      break;
    }
    await sendOneFile(path);
  }
}

function pastedImageName(file) {
  const ext = (file.type || "image/png").split("/").at(-1) || "png";
  return file.name || `pasted-image-${new Date().toISOString().replace(/[:.]/g, "-")}.${ext}`;
}

async function fileToPath(file) {
  const path = file?.path;
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

async function sendDroppedOrPastedFiles(event) {
  const data = event.clipboardData || event.dataTransfer;
  if (!data?.files.length) return;
  event.preventDefault();
  messages.classList.remove("drag-over");
  const selection = state.selected;
  try {
    const paths = await pathsFromDataTransfer(data);
    if (state.selected === selection) await sendFiles(paths);
    else setStatus("聊天对象已切换，文件未发送。");
  } catch (err) {
    reportError("读取待发送文件失败", err);
  }
}

messages.addEventListener("drop", sendDroppedOrPastedFiles);
$("chat-pane").addEventListener("paste", sendDroppedOrPastedFiles);

const currentWebview = tauri?.webview?.getCurrentWebview?.();
currentWebview?.onDragDropEvent?.((event) => {
  const { type, position, paths } = event.payload;
  const hit = position && document.elementFromPoint(position.x / devicePixelRatio, position.y / devicePixelRatio);
  const inChat = state.view === "chat" && state.selected && !document.querySelector("dialog[open]") && messages.contains(hit ?? null);
  messages.classList.toggle("drag-over", Boolean(inChat && (type === "over" || type === "enter")));
  if (type === "drop" && inChat) void handleFileDrop(paths || []);
});

$("send-file").addEventListener("submit", async (event) => {
  event.preventDefault();
  const path = $("file-path").value.trim();
  if (!path) return;
  closeDialog("file-dialog");
  await sendOneFile(path);
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
    "mesh://desktop-error",
  ]) {
    listen(name, ({ payload }) => {
      log(name, payload);
      if (name === "mesh://desktop-error") return reportError(payload.operation, payload.error);
      if (name === "mesh://groups-changed") {
        loadSavedGroups();
        if (state.session) void refreshMembers();
        return;
      }
      if (name === "mesh://notification-opened") {
        void openNotificationTarget(payload);
        return;
      }
      if (name === "mesh://message-received") return addIncoming(payload.message, payload.group_id);
      if (name === "mesh://transfer-progress") return rememberTransfer(payload);
      if (name === "mesh://update-package-ready") return markUpdatePackageReady(payload);
      if (payload?.group_id && payload.group_id !== state.session?.group_id) return;
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
void syncDesktopSettings().catch((err) => reportError("应用桌面设置失败", err));

call("app_version")
  .then((version) => {
    const label = `v${version}`;
    document.title = `LAN Mesh ${label}`;
    $("title-version").textContent = label;
    $("settings-version").textContent = label;
  })
  .catch((err) => recordIssue("读取版本号失败", err));

renderAll();
loadNetworkInterfaces();
loadSavedGroups();
checkForUpdate({ silent: true });
