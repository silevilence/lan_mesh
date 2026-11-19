import { $ } from "./dom.mjs";
import { issueButton } from "./feedback.mjs";

// Message objects belong to a single in-memory conversation. Keep their DOM nodes
// while that conversation is open so selection, focus and pending clicks survive.
export function createTimeline({ displayName, saveReceivedFile, installReceivedUpdate }) {
  const viewport = $("messages");
  const latest = $("latest-message");
  const positions = new WeakMap();
  let active = null;
  let rows = new Map();
  const nearEnd = () => viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop < 8;

  const itemsByNode = new WeakMap();
  let wasVisible = false;

  function firstVisible() {
    const nodes = viewport.children;
    const top = viewport.getBoundingClientRect().top;
    let lo = 0, hi = nodes.length;
    while (lo < hi) {
      const mid = Math.floor((lo + hi) / 2);
      if (nodes[mid].getBoundingClientRect().bottom <= top) lo = mid + 1;
      else hi = mid;
    }
    return nodes[lo];
  }

  function updateLatest() {
    if (!viewport.clientHeight || !active) return;
    latest.hidden = nearEnd();
    const first = latest.hidden ? null : firstVisible();
    positions.set(active, first ? {
      item: itemsByNode.get(first),
      offset: first.getBoundingClientRect().top - viewport.getBoundingClientRect().top,
    } : null);
  }

  function restorePosition() {
    if (!active || !viewport.clientHeight) return;
    const saved = positions.get(active);
    const row = rows.get(saved?.item);
    if (row) viewport.scrollTop += row.node.getBoundingClientRect().top - viewport.getBoundingClientRect().top - saved.offset;
    else viewport.scrollTop = viewport.scrollHeight;
    updateLatest();
  }
  viewport.addEventListener("scroll", updateLatest, { passive: true });
  new ResizeObserver(() => {
    const visible = viewport.clientHeight > 0;
    if (visible && !wasVisible) restorePosition();
    else updateLatest();
    wasVisible = visible;
  }).observe(viewport);
  latest.onclick = () => {
    viewport.scrollTop = viewport.scrollHeight;
    $("message-input").focus({ preventScroll: true });
    updateLatest();
  };

  function makeRow(item) {
    const node = document.createElement("article");
    node.className = item.mine ? "message mine" : "message";
    const author = document.createElement("strong");
    const time = document.createElement("time");
    const meta = document.createElement("header");
    meta.append(author, time);
    const body = document.createElement("div");
    body.className = "bubble";
    body.textContent = item.content;
    const status = document.createElement("small");
    node.append(meta, body, status);
    const date = new Date(item.at);
    if (Number.isFinite(date.getTime())) {
      time.dateTime = date.toISOString();
      time.title = date.toLocaleString();
      time.textContent = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    }
    const row = { node, author, status, action: null };
    rows.set(item, row);
    itemsByNode.set(node, item);
    paint(item);
    return node;
  }

  function paint(item) {
    const row = rows.get(item);
    if (!row) return;
    const name = item.mine ? "我 · " + displayName(item.from) : displayName(item.from);
    if (row.author.textContent !== name) row.author.textContent = name;
    const status = item.kind === "text" && !item.mine ? "" : item.status;
    if (row.status.textContent !== status) row.status.textContent = status;
    row.status.hidden = !status;
    row.status.classList.toggle("failed", Boolean(item.issue));
    if (row.action) return;
    if (item.issue) {
      row.action = issueButton(item.issue);
    } else if (!item.mine && ((item.kind === "file" && item.path) || (item.kind === "update" && item.ready))) {
      row.action = document.createElement("button");
      row.action.type = "button";
      row.action.className = "secondary mini";
      row.action.textContent = item.kind === "file" ? "另存为" : "安装更新";
      row.action.onclick = () => item.kind === "file" ? saveReceivedFile(item) : installReceivedUpdate(item);
    }
    if (row.action) row.node.append(row.action);
  }

  function preserveReading(change, ownSend = false) {
    if (!viewport.clientHeight) { change(); return; }
    const selection = window.getSelection();
    const selected = !selection.isCollapsed && viewport.contains(selection.anchorNode)
      ? selection.anchorNode.parentElement?.closest(".message") : null;
    const target = viewport.querySelector("button:hover") || viewport.querySelector("button:focus");
    const follow = ownSend || (nearEnd() && !selected && !target);
    const anchor = follow ? null : (target || selected || firstVisible());
    const top = anchor?.getBoundingClientRect().top;
    change();
    if (follow) viewport.scrollTop = viewport.scrollHeight;
    else if (anchor?.isConnected) viewport.scrollTop += anchor.getBoundingClientRect().top - top;
    updateLatest();
  }

  function show(list, isGroup) {
    if (list === active) {
      if (list) for (const item of list) paint(item);
      return;
    }
    updateLatest();
    active = list;
    rows = new Map();
    viewport.replaceChildren();
    if (!list) return;
    if (!list.length) {
      const empty = document.createElement("p");
      empty.className = "hint";
      empty.textContent = isGroup ? "还没有群消息。写条消息，或邀请成员加入。" : "还没有单聊消息。";
      viewport.append(empty);
    } else {
      const fragment = document.createDocumentFragment();
      for (const item of list) fragment.append(makeRow(item));
      viewport.append(fragment);
    }
    restorePosition();
  }

  function insert(list, item, index) {
    if (list !== active) return;
    preserveReading(() => {
      viewport.querySelector(":scope > .hint")?.remove();
      viewport.insertBefore(makeRow(item), rows.get(list[index + 1])?.node || null);
    }, item.mine);
  }

  function update(item) {
    if (rows.has(item)) preserveReading(() => paint(item));
  }
  return { show, insert, update };
}
