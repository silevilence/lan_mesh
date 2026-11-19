import { $, emptyItem } from "./dom.mjs";
import { recordIssue, reportError, setStatus } from "./feedback.mjs";

export function setupUpdates({ call, state, formatBytes }) {
  async function installReceivedUpdate(item) {
    const source = item.from ? item.from.slice(0, 8) : "未知设备";
    const message = [
      `确认安装来自局域网设备 ${source} 的更新包 v${item.version}？`,
      "程序将退出并启动本地安装程序。",
      "文件已通过完整性校验。应用尚未验证发布者签名，请确认你信任发送人和安装包来源。",
    ].join("\n\n");
    if (!confirm(message)) return;
    try {
      await call("install_received_update_package", { fileId: item.file_id });
    } catch (err) {
      reportError("无法安装更新包", err);
    }
  }

  async function checkForUpdate({ silent = false } = {}) {
    if ($("check-update").disabled) return;
    $("check-update").disabled = true;
    let installing = false;
    try {
      if (!silent) setStatus("正在检查更新...");
      const update = await call("check_update");
      $("check-update").title = update ? "查看版本 " + update.version + " 的更新说明" : "检查是否有新版本";
      $("check-update").textContent = update ? "查看更新" : "检查更新";
      if (!update) {
        if (!silent) setStatus("已是最新版本");
        return;
      }

      if (silent) {
        setStatus("发现新版本 " + update.version + "，可在设置中查看。");
        return;
      }
      const notes = String(update.body ?? "").trim();
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
      installing = true;
      await call("install_update");
    } catch (err) {
      if (silent && !installing) recordIssue("自动检查更新失败", err);
      else reportError(installing ? "下载安装更新失败，请重新检查更新" : "检查更新失败", err);
    } finally {
      $("check-update").disabled = false;
    }
  }

  async function openShareUpdateDialog() {
    try {
      const update = await call("get_retained_update_package");
      $("check-update").title = update ? "查看版本 " + update.version + " 的更新说明" : "检查是否有新版本";
      $("check-update").textContent = update ? "查看更新" : "检查更新";
      if (!update) {
        setStatus("本机还没有可分享的安装包。开启“保留安装包”并保存设置，下次通过应用更新时会自动保留。");
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
      if (!state.savedGroups.some((group) => group.status === "connected")) groups.append(emptyItem("请先连接一个群组，再来分享更新包。"));
      $("confirm-share-update").disabled = true;
      $("share-update-dialog").showModal();
    } catch (err) {
      reportError("无法读取保留安装包", err);
    }
  }

  async function shareRetainedUpdatePackage(event) {
    event.preventDefault();
    const groupIds = [...document.querySelectorAll('input[name="share-update-group"]:checked')].map((input) => input.value);
    if (!groupIds.length) return;
    const send = $("confirm-share-update");
    if (send.disabled) return;
    send.disabled = true;
    send.textContent = "正在发送…";
    try {
      const result = await call("share_retained_update_package", { groupIds });
      $("share-update-dialog").close();
      const failed = result.failed_groups || [];
      const summary = `已向 ${result.shared.length} 个群组发送更新包`;
      if (failed.length) reportError(`${summary}，${failed.length} 个群组发送失败`, result.errors || failed);
      else setStatus(summary);
    } catch (err) {
      reportError("分享更新包失败", err, $("share-update-dialog"));
    } finally {
      send.textContent = "发送到所选群组";
      send.disabled = !document.querySelector('input[name="share-update-group"]:checked');
    }
  }

  $("check-update").addEventListener("click", () => checkForUpdate());
  $("share-update-package").addEventListener("click", () => openShareUpdateDialog());
  $("share-update-groups").addEventListener("change", () => {
    $("confirm-share-update").disabled = !document.querySelector('input[name="share-update-group"]:checked');
  });
  $("share-update-form").addEventListener("submit", shareRetainedUpdatePackage);
  return { checkForUpdate, installReceivedUpdate };
}
