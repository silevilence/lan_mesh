import test from "node:test";
import assert from "node:assert/strict";
import {
  conversationPresentation,
  displayName,
  routeLabel,
  syncMemberNicknames,
  visibleMembers,
} from "./ui-model.mjs";

test("a routed member is described without claiming a direct connection", () => {
  const member = { device_id: "remote", online: true };
  const routes = [{ target_device_id: "remote", path: ["relay", "remote"] }];
  assert.equal(routeLabel(member, routes), "路由可达");
});

test("an online member supersedes its stale offline row after reconnect", () => {
  const members = [
    { device_id: "old-id", nickname: "虚拟云", online: false },
    { device_id: "new-id", nickname: "虚拟云", online: true },
  ];
  assert.deepEqual(visibleMembers(members), [members[1]]);
});

test("the local member can use its synchronized nickname when settings are empty", () => {
  const nicknames = new Map([["self-id", "虚拟云"]]);
  assert.equal(displayName("self-id", {
    sessionDeviceId: "self-id",
    settingsNickname: "",
    nicknames,
  }), "虚拟云(self-id)");
});

test("a blank member snapshot does not erase a nickname learned from a message", () => {
  const nicknames = new Map([["peer-id", "虚拟云"]]);
  syncMemberNicknames(nicknames, [{ device_id: "peer-id", nickname: null }]);
  assert.equal(nicknames.get("peer-id"), "虚拟云");
});

test("direct chat shows the synchronized name and returns to group chat", () => {
  const nicknames = new Map([["peer-id", "虚拟云"]]);
  const nameOf = (deviceId) => displayName(deviceId, {
    sessionDeviceId: "self-id",
    settingsNickname: "Surface",
    nicknames,
  });
  assert.deepEqual(
    conversationPresentation(
      { role: "leaf", group_name: "测试群组" },
      { type: "direct", id: "peer-id" },
      nameOf,
    ),
    {
      title: "单聊 虚拟云(peer-id)",
      leaveLabel: "返回群聊",
      leaveAction: "open-group",
    },
  );
});
