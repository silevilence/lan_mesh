import { $, emptyItem } from "./dom.mjs";
import { visibleMembers } from "./ui-model.mjs";

// Existing rows keep their positions. A newly discovered device or transfer is
// appended, even if its name or ID sorts before the item under the pointer.
function listView(host) {
  const rows = new Map();
  return (items, keyOf, make, placeholder) => {
    const keys = new Set();
    for (const item of items) {
      const key = keyOf(item);
      keys.add(key);
      let row = rows.get(key);
      if (!row) {
        row = make();
        rows.set(key, row);
        host.querySelector(":scope > .muted")?.remove();
        host.append(row.node);
      }
      row.update(item);
    }
    for (const [key, row] of rows) {
      if (keys.has(key)) continue;
      row.node.remove();
      rows.delete(key);
    }
    if (!rows.size && host.firstChild?.textContent !== placeholder) host.replaceChildren(emptyItem(placeholder));
  };
}

function button(label, className) {
  const node = document.createElement("button");
  node.type = "button";
  node.className = className;
  node.textContent = label;
  return node;
}

function contactRow(withOptions) {
  const node = document.createElement("div");
  node.className = "contact-row";
  const open = button("", "contact");
  const avatar = document.createElement("span");
  avatar.className = "contact-avatar";
  avatar.setAttribute("aria-hidden", "true");
  const lines = document.createElement("span");
  const title = document.createElement("strong");
  const subtitle = document.createElement("small");
  lines.append(title, subtitle);
  open.append(avatar, lines);
  node.append(open);
  const options = withOptions ? button("···", "row-options ghost") : null;
  if (options) node.append(options);
  function label(name, detail, active, kind) {
    node.classList.toggle("active", active);
    avatar.classList.toggle("person", kind === "person");
    avatar.textContent = Array.from(name)[0] || "#";
    title.textContent = name;
    subtitle.textContent = detail;
    open.title = name + " · " + detail;
    open.setAttribute("aria-current", active ? "true" : "false");
  }
  return { node, open, options, label };
}

export function createSidebar({ state, displayName, routeLabel, openGroup, openDirect, activateSavedGroup,
  retrySavedGroup, deleteSavedGroup, openJoinDialog, closeCurrentSession, showIssue, reportError,
  saveReceivedFile, retryTransfer, formatBytes, transferredBytes, transferSpeed, markConversationRead }) {
  const groups = listView($("session-list"));
  const members = listView($("member-list"));
  const files = listView($("transfers"));
  const listButton = $("back-list");
  const back = $("back-chat");
  const nav = [...document.querySelectorAll(".rail-nav button")];
  let managed = null;

  function showList() {
    $("chat-view").classList.add("browsing");
    nav.find((tab) => tab.getAttribute("aria-pressed") === "true").focus({ preventScroll: true });
  }
  function showChat() {
    $("chat-view").classList.remove("browsing");
    back.hidden = !state.selected;
  }
  listButton.onclick = showList;
  back.onclick = () => { showChat(); markConversationRead(); $("message-input").focus(); };
  for (const tab of nav) {
    tab.onclick = () => {
      for (const other of nav) {
        const selected = tab === other;
        other.setAttribute("aria-pressed", String(selected));
        $(other.getAttribute("aria-controls")).hidden = !selected;
      }
    };
  }

  function manage(group) {
    if (!group) return;
    managed = group;
    $("managed-group").textContent = group.group_name || "群组";
    const current = state.session?.group_id === group.group_id;
    $("leave-button").textContent = current ? (group.role === "relay" ? "停止本机群组" : "退出群组") : "从本机移除群组";
    $("group-error").hidden = !group.last_error;
    $("group-options").showModal();
  }
  $("chat-options").onclick = () => manage(state.savedGroups.find((g) => g.group_id === state.session?.group_id) || state.session);
  $("group-error").onclick = () => showIssue("连接群组失败 · " + managed.group_name, managed.last_error);
  $("close-options").onclick = () => $("group-options").close();
  $("leave-button").onclick = async () => {
    const group = managed;
    $("group-options").close();
    try {
      if (group.group_id === state.session?.group_id) await closeCurrentSession();
      else await deleteSavedGroup(group);
    } catch (err) {
      reportError("移除群组失败", err);
    }
  };

  function groupRow() {
    const row = contactRow(true);
    row.update = (group) => {
      const saved = group.status !== undefined;
      const current = state.session?.group_id === group.group_id;
      const reachable = group.status === "connected";
      const name = group.group_name || "群聊";
      let detail = saved ? (reachable ? (group.role === "relay" ? "我创建的群组" : "已加入的群组") : "连接中断 · 点击重连") : "附近的群组 · 点击加入";
      if (state.savedGroups.some((g) => g.group_id !== group.group_id && g.group_name === name)) detail += " · " + group.group_id.slice(0, 8);
      const unread = state.conversations.get(group.group_id)?.unread.size > 0;
      row.open.classList.toggle("unread", unread);
      if (unread) detail = "有未读消息 · " + detail;
      row.label(name, detail, current && state.selected?.type === "group", "group");
      row.options.hidden = !saved;
      row.options.setAttribute("aria-label", "管理群组 " + name);
      row.options.onclick = () => manage(group);
      row.open.onclick = () => {
        if (!saved) return openJoinDialog(group);
        if (!reachable) return retrySavedGroup(group);
        if (current) return openGroup();
        return activateSavedGroup(group);
      };
    };
    return row;
  }
  function renderSessionList() {
    const savedIds = new Set(state.savedGroups.map((group) => group.group_id));
    const nearby = state.relays.filter((g) => !savedIds.has(g.group_id));
    const unread = state.savedGroups.some((g) => state.conversations.get(g.group_id)?.unread.size);
    nav[0].classList.toggle("unread", unread);
    nav[0].setAttribute("aria-label", unread ? "群组，有未读消息" : "群组");
    groups([...state.savedGroups, ...nearby], (group) => group.status === undefined ? group.group_id + group.tcp_addr : group.group_id, groupRow, "还没有群组。可以新建或加入一个群组。");
  }

  function memberRow() {
    const row = contactRow(false);
    row.update = (member) => {
      const self = member.device_id === state.session?.device_id;
      const name = displayName(member.device_id);
      const unread = state.unread?.has(member.device_id);
      row.open.classList.toggle("unread", Boolean(unread));
      const detail = self ? "本机" : (member.previous ? "单聊记录" : member.online ? "在线 · " + routeLabel(member) : "离线");
      row.label(name, unread ? "未读单聊 · " + detail : detail, state.selected?.id === member.device_id, "person");
      row.open.disabled = self;
      row.open.setAttribute("aria-label", self ? name + "，本机" : "与 " + name + " 单聊，" + detail);
      row.open.onclick = () => openDirect(member.device_id);
    };
    return row;
  }
  function renderMembers() {
    const unread = [...(state.unread || [])].some((key) => key !== "group");
    nav[1].classList.toggle("unread", unread);
    nav[1].setAttribute("aria-label", unread ? "成员，有未读单聊" : "成员");
    const known = new Set(state.members.map((member) => member.device_id));
    const previous = [...state.directMessages.keys()].filter((peer) => !known.has(peer))
      .map((device_id) => ({ device_id, previous: true }));
    members([...visibleMembers(state.members), ...previous], (m) => state.session.group_id + m.device_id, memberRow, state.session ? "暂无群成员。" : "打开群组后可查看成员。");
  }

  function transferRow() {
    const node = document.createElement("div");
    node.className = "transfer-row";
    const title = document.createElement("strong");
    const detail = document.createElement("small");
    const progress = document.createElement("progress");
    const actions = document.createElement("div");
    const inspect = button("错误详情", "secondary mini");
    const retry = button("续传", "secondary mini");
    const save = button("另存为", "secondary mini");
    actions.append(inspect, retry, save);
    node.append(title, detail, progress, actions);
    function update(item) {
      const name = item.file_name || item.file_id.slice(0, 8);
      const incoming = item.direction === "incoming";
      const failed = item.status === "failed";
      title.textContent = (incoming ? "接收 " : "发送 ") + name;
      title.title = title.textContent;
      detail.textContent = failed ? "传输中断" : formatBytes(transferredBytes(item)) + " / " + formatBytes(item.total_size) + (item.status === "done" ? " · 已完成" : " · " + formatBytes(Math.round(transferSpeed(item))) + "/s");
      detail.title = item.path || detail.textContent;
      progress.setAttribute("aria-label", title.textContent);
      progress.max = item.chunk_count || 1;
      progress.value = item.done_chunks || 0;
      inspect.hidden = !failed;
      retry.hidden = !failed || item.done_chunks >= item.chunk_count;
      save.hidden = !incoming || item.status !== "done" || !item.path;
      inspect.onclick = () => showIssue("文件传输失败 · " + name, item.error || "连接中断");
      save.onclick = () => saveReceivedFile(item);
      retry.onclick = async () => {
        try { await retryTransfer(item); }
        catch (err) {
          item.status = "failed";
          item.error = String(err);
          reportError("文件续传失败", err);
          renderTransfers();
        }
      };
    }
    return { node, update };
  }
  function renderTransfers() {
    files(state.transfers.values(), (item) => item.group_id + item.file_id, transferRow, "暂无文件传输。");
  }
  return { renderSessionList, renderMembers, renderTransfers, showChat };
}
