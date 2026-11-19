const text = (value) => String(value ?? "");
const short = (value) => text(value).slice(0, 8);

export function cleanNickname(value) {
  return text(value).trim().slice(0, 24);
}

export function rememberNickname(nicknames, deviceId, nickname) {
  const clean = cleanNickname(nickname);
  if (!deviceId || !clean) return false;
  const changed = nicknames.get(deviceId) !== clean;
  nicknames.set(deviceId, clean);
  return changed;
}

export function displayName(deviceId, { sessionDeviceId, settingsNickname, nicknames }) {
  if (!deviceId) return "未知设备";
  const nickname = deviceId === sessionDeviceId
    ? cleanNickname(settingsNickname) || nicknames.get(deviceId)
    : nicknames.get(deviceId);
  return nickname ? `${nickname}(${short(deviceId)})` : short(deviceId);
}

export function visibleMembers(members) {
  const onlineNicknames = new Set(
    members
      .filter((member) => member.online)
      .map((member) => cleanNickname(member.nickname))
      .filter(Boolean),
  );
  return members.filter((member) => {
    const nickname = cleanNickname(member.nickname);
    return member.online || !nickname || !onlineNicknames.has(nickname);
  });
}

export function syncMemberNicknames(nicknames, members) {
  for (const member of members) {
    const nickname = cleanNickname(member.nickname);
    if (nickname) nicknames.set(member.device_id, nickname);
  }
}

export function routeLabel(member, routes) {
  if (!member.online) return "离线";
  if (!routes.some((item) => item.target_device_id === member.device_id)) return "尚未确认能否联系";
  return "可以联系";
}

export function conversationPresentation(session, selected, nameOf) {
  const isGroup = selected.type === "group";
  return {
    title: isGroup ? session.group_name || "群聊" : `单聊 ${nameOf(selected.id)}`,
    leaveLabel: isGroup
      ? session.role === "relay" ? "停止本机群组" : "退出群组"
      : "返回群聊",
    leaveAction: isGroup ? "close-session" : "open-group",
  };
}

export function encodeShare(value) {
  const bytes = new TextEncoder().encode(JSON.stringify(value));
  return `lanmesh:${btoa(String.fromCharCode(...bytes))}`;
}

export function decodeShare(value) {
  const input = String(value ?? "").trim();
  const line = input.split(/\r?\n/).at(-1).trim();
  const code = line.match(/^(?:分享码\s*[:：]\s*)?lanmesh:([A-Za-z0-9+/=]+)$/i)?.[1] || input;
  const bytes = Uint8Array.from(atob(code), (char) => char.charCodeAt(0));
  return JSON.parse(new TextDecoder().decode(bytes));
}
