import { decodeShare, encodeShare } from "./ui-model.mjs";
import { $, emptyItem, fillItem, whileBusy } from "./dom.mjs";
import { copyText, reportError, setStatus } from "./feedback.mjs";

const isLoopback = (ip) => ip?.startsWith("127.");

function parseHostPort(value) {
  const raw = String(value ?? "").trim();
  const index = raw.lastIndexOf(":");
  if (index <= 0) return {};
  return { host: raw.slice(0, index), port: raw.slice(index + 1) };
}

export function setupGroupForms({ state, call, setSession, renderSessionList }) {
  function bindAddr() {
    return $("create-bind-preset").value || $("create-bind").value || "0.0.0.0:0";
  }

  function selectedLocalIp() {
    return $("join-interface").value || $("manual-local-ip").value;
  }

  function renderRelays(items) {
    const relays = $("relays");
    relays.innerHTML = "";
    const visibleItems = items.filter((relay) => relayState(relay) !== "own");
    if (!visibleItems.length) {
      relays.append(emptyItem("没有找到附近的群组。可以粘贴分享码，或手动填写群组信息。"));
      return;
    }
    for (const relay of visibleItems) {
      const state = relayState(relay);
      const node = document.createElement("button");
      node.type = "button";
      node.disabled = state === "joined";
      node.className = `item ${state === "joined" ? "disabled" : "clickable"}`;
      fillItem(node, relay.group_name || "群聊", state === "joined" ? "已加入" : "选择这个群组", relay.tcp_addr);
      if (state !== "joined") node.addEventListener("click", () => fillJoinForm(relay));
      relays.append(node);
    }
  }

  function relayState(relay) {
    const saved = state.savedGroups.find((group) => group.group_id === relay.group_id);
    if (saved?.role === "relay") return "own";
    return saved?.status === "connected" ? "joined" : "available";
  }

  function fillJoinForm(relay) {
    selectionVersion++;
    $("join-dialog").querySelector(".dialog-feedback")?.replaceChildren();
    fillLocalIp("");
    $("manual-group-id").value = relay.group_id;
    $("manual-relay-addr").value = relay.tcp_addr;
    $("manual-group-name").value = relay.group_name || "群聊";
    $("join-preview").textContent = "准备加入「" + $("manual-group-name").value + "」";
    $("join-submit").disabled = false;
    $("join-submit").focus();
  }

  function openJoinDialog(relay = null) {
    selectionVersion++;
    $("manual-join").reset();
    $("manual-local-ip").disabled = false;
    $("share-code-input").value = "";
    $("join-details").open = false;
    $("join-submit").disabled = true;
    $("join-preview").textContent = "也可以手动填写群组信息。";
    $("join-dialog").showModal();
    if (state.relays.length) renderRelays(state.relays);
    if (relay) fillJoinForm(relay);
  }

  function renderNetworkInterfaces(items) {
    state.networkInterfaces = items;
    const create = $("create-bind-preset");
    const discover = $("discover-bind");
    const joinSelect = $("join-interface");
    create.length = 3;
    discover.length = 2;
    joinSelect.length = 1;

    for (const item of items) {
      create.add(new Option(`${item.name} (${item.ip_addr})`, item.bind_addr));
      discover.add(new Option(`${item.name} (${item.ip_addr})`, item.discovery_bind_addr));
      joinSelect.add(new Option(`${item.name} (${item.ip_addr})`, item.ip_addr));
    }
  }

  async function loadNetworkInterfaces() {
    try {
      renderNetworkInterfaces(await call("list_network_interfaces"));
    } catch (err) {
      reportError("读取本机网络信息失败", err);
    }
  }

  function relayAddressCandidates() {
    if (state.session?.role !== "relay") {
      return state.session?.relay_addr ? [{ name: "群组地址", ip: parseHostPort(state.session.relay_addr).host, addr: state.session.relay_addr }] : [];
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
      group_name: state.session.group_name || "群聊",
      relay_port: Number(parseHostPort(candidates[0]?.addr).port) || null,
      relay_addrs: candidates.map((item) => item.addr),
      relay_interfaces: candidates,
    };
  }

  function shareText(payload) {
    return [
      `群组：${payload.group_name}`,
      `群组编号： ${payload.group_id}`,
      "群组地址：",
      ...payload.relay_addrs.map((addr) => `- ${addr}`),
      `分享码：${encodeShare(payload)}`,
    ].join("\n");
  }

  function openShareDialog() {
    if (!state.session) return;
    const payload = sharePayload();
    $("share-summary").textContent = `${payload.group_name} · 群组编号 ${payload.group_id}`;
    const list = $("share-addresses");
    list.innerHTML = "";
    for (const item of payload.relay_interfaces) {
      const node = document.createElement("div");
      node.className = "item";
      const title = document.createElement("div");
      title.className = "title";
      title.textContent = item.name;
      const addr = document.createElement("span");
      addr.textContent = item.addr;
      const copy = document.createElement("button");
      copy.type = "button";
      copy.className = "secondary mini";
      copy.textContent = "复制地址";
      copy.onclick = () => copyText(item.addr);
      node.append(title, addr, copy);
      list.append(node);
    }
    if (!list.children.length) list.append(emptyItem("没有可分享的网卡地址。"));
    const available = payload.relay_addrs.length > 0;
    $("copy-share-code").disabled = !available;
    $("copy-share-text").disabled = !available;
    $("share-code-output").value = available ? encodeShare(payload) : "";
    $("share-guidance").textContent = available
      ? "把分享码发给同一局域网里的成员。对方打开“加入群组”后，粘贴并读取分享码即可选择这个群组。"
      : "没有可分享的网络地址。请连接局域网后再试。";
    if (available && payload.relay_interfaces.every((item) => isLoopback(item.ip))) {
      $("share-guidance").textContent = "这个群组仅限本机测试。其他设备要加入，请用局域网地址重新建群。";
    }
    $("share-dialog").showModal();
  }

  function fillLocalIp(localIp = "") {
    $("join-interface").value = localIp;
    $("manual-local-ip").value = localIp;
    $("manual-local-ip").disabled = Boolean($("join-interface").value);
  }

  let selectionVersion = 0;
  function clearCandidate() {
    selectionVersion++;
    $("join-dialog").querySelector(".dialog-feedback")?.replaceChildren();
    $("manual-group-id").value = "";
    $("manual-group-name").value = "";
    $("manual-relay-addr").value = "";
    fillLocalIp("");
    $("join-submit").disabled = true;
    $("join-preview").textContent = "读取分享码，或手动填写群组信息。";
  }

  async function parseShareIntoJoinForm() {
    clearCandidate();
    try {
      const payload = decodeShare($("share-code-input").value);
      const relayAddrs = payload.relay_addrs || payload.relayAddrs || [];
      if (typeof payload.group_id !== "string" || !Array.isArray(relayAddrs) || !relayAddrs.length ||
          !relayAddrs.every((addr) => typeof addr === "string" && addr.trim())) {
        throw new Error("分享码缺少群组编号或地址");
      }
      fillJoinForm({ group_id: payload.group_id, group_name: payload.group_name, tcp_addr: relayAddrs[0] });
      fillLocalIp("");
      const version = selectionVersion;
      const localIps = state.networkInterfaces.map((item) => item.ip_addr).filter((ip) => ip && !isLoopback(ip));
      try {
        const probe = await call("probe_relay_addr", { relayAddrs, localIps, timeoutMs: 250 });
        if (version !== selectionVersion || !$("join-dialog").open) return;
        $("manual-relay-addr").value = probe.relay_addr;
        fillLocalIp(probe.local_ip || "");
        setStatus("已找到群组地址，点击“加入群组”即可连接。", $("join-dialog"));
      } catch (err) {
        if (version !== selectionVersion || !$("join-dialog").open) return;
        reportError("暂时连不上分享码里的地址。可先尝试加入，或向群成员索要新地址。", err, $("join-dialog"));
      }
    } catch (err) {
      reportError("读不出分享码，请重新复制完整的分享码", err, $("join-dialog"));
    }
  }

  async function join(groupId, relayAddr, localIp = "") {
    selectionVersion++;
    const groupName = $("manual-group-name").value.trim();
    const session = await call("join_group", {
      deviceId: null,
      groupId,
      groupName: groupName || null,
      relayAddr,
      localIp: localIp || null,
    });
    session.relay_addr = relayAddr;
    setSession(session, groupName);
  }

  $("create-group").addEventListener("submit", async (event) => {
    event.preventDefault();
    await whileBusy(event.currentTarget.querySelector('button[type="submit"]'), "正在创建…", async () => {
      try {
        const groupName = $("group-name").value.trim() || "LAN Mesh";
        const session = await call("create_group", {
          deviceId: null,
          groupId: null,
          groupName,
          bindAddr: bindAddr(),
        });
        $("create-dialog").close();
        setSession(session, groupName);
      } catch (err) {
        reportError("创建群组失败，请检查网络设置", err, $("create-dialog"));
      }
    });
  });

  $("discover-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    await whileBusy(event.currentTarget.querySelector('button[type="submit"]'), "正在搜索…", async () => {
      try {
        state.relays = await call("discover_relays", { bindAddr: $("discover-bind").value, durationMs: 1000 });
        renderRelays(state.relays);
        renderSessionList();
      } catch (err) {
        reportError("搜索失败，请检查网络连接后重试", err, $("join-dialog"));
      }
    });
  });

  $("manual-join").addEventListener("submit", async (event) => {
    event.preventDefault();
    await whileBusy(event.currentTarget.querySelector('button[type="submit"]'), "正在加入…", async () => {
      try {
        await join($("manual-group-id").value, $("manual-relay-addr").value, selectedLocalIp());
        $("join-dialog").close();
      } catch (err) {
        reportError("加入失败，请核对群组编号和地址", err, $("join-dialog"));
      }
    });
  });

  $("join-interface").addEventListener("change", () => {
    fillLocalIp($("join-interface").value);
  });

  $("share-button").addEventListener("click", openShareDialog);
  $("parse-share-code").addEventListener("click", () =>
    whileBusy($("parse-share-code"), "正在读取…", parseShareIntoJoinForm));
  $("copy-share-code").addEventListener("click", () => copyText($("share-code-output").value));
  $("copy-share-text").addEventListener("click", () => copyText(shareText(sharePayload())));
  $("create-bind-preset").addEventListener("change", () => {
    const manual = !$("create-bind-preset").value;
    $("manual-bind-field").hidden = !manual;
    $("create-bind").disabled = !manual;
    $("create-bind").required = manual;
  });
  $("share-code-input").addEventListener("input", clearCandidate);
  $("manual-join").addEventListener("input", (event) => {
    selectionVersion++;
    if (!["manual-group-id", "manual-relay-addr"].includes(event.target.id)) return;
    $("manual-group-name").value = "";
    $("join-preview").textContent = "请核对群组编号和地址后加入。";
    $("join-submit").disabled = !($("manual-group-id").value.trim() && $("manual-relay-addr").value.trim());
  });
  return { openJoinDialog, loadNetworkInterfaces };
}
