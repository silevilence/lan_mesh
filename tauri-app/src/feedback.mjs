const byId = (id) => document.getElementById(id);
const issues = [];
const limit = 30;

function describe(error) {
  if (error instanceof Error) return error.stack || error.message;
  if (typeof error === "string") return error;
  try {
    return JSON.stringify(error, null, 2) ?? String(error);
  } catch {
    return String(error);
  }
}

function feedback(message, issue, dialog) {
  const status = byId("status");
  status.textContent = message;
  status.title = message;
  const panel = dialog?.open && dialog.querySelector(".dialog-feedback");
  if (!panel || byId("error-dialog").open) return;
  const note = document.createElement("span");
  note.textContent = message;
  note.title = message;
  panel.replaceChildren(note);
  if (issue) panel.append(issueButton(issue));
}

export function setStatus(message, dialog) {
  feedback(message, null, dialog);
}

export function recordIssue(operation, error) {
  const time = new Date();
  const version = byId("title-version").textContent;
  const issue = {
    label: time.toLocaleTimeString() + " · " + operation,
    detail: [operation, "时间：" + time.toISOString(), "应用：LAN Mesh " + version, describe(error)].join("\n"),
  };
  issues.unshift(issue);
  issues.length = Math.min(limit, issues.length);
  const history = byId("error-history");
  history.classList.add("has-issues");
  history.textContent = "问题记录 (" + issues.length + ")";
  return issue;
}

export function reportError(operation, error, dialog) {
  const issue = recordIssue(operation, error);
  feedback(operation, issue, dialog);
  return issue;
}

export function issueButton(issue) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "secondary mini";
  button.textContent = "查看错误详情";
  button.addEventListener("click", () => openErrors(issue));
  return button;
}

export function showIssue(operation, error) {
  openErrors(recordIssue(operation, error));
}

function openErrors(issue = issues[0]) {
  if (!issue) {
    byId("error-dialog").querySelector(".modal").classList.add("no-issues");
    byId("copy-result").textContent = "本次运行还没有问题记录。";
    byId("error-dialog").showModal();
    return;
  }
  byId("error-dialog").querySelector(".modal").classList.remove("no-issues");
  const entries = [issue, ...issues.filter((entry) => entry !== issue)];
  const choice = byId("error-choice");
  choice.replaceChildren(...entries.map((entry, i) => new Option(entry.label, String(i))));
  const select = () => {
    byId("error-detail").value = entries[choice.selectedIndex].detail;
    byId("copy-result").textContent = "";
  };
  choice.onchange = select;
  select();
  byId("error-dialog").showModal();
}

export async function copyText(value) {
  const dialog = document.activeElement?.closest("dialog");
  try {
    await navigator.clipboard.writeText(value);
    setStatus("已复制到剪贴板", dialog);
  } catch (err) {
    reportError("复制失败，请手动选择并复制文字", err, dialog);
  }
}

byId("error-history").addEventListener("click", () => openErrors());
byId("close-errors").addEventListener("click", () => byId("error-dialog").close());
byId("copy-error").addEventListener("click", async () => {
  const field = byId("error-detail");
  try {
    await navigator.clipboard.writeText(field.value);
    byId("copy-result").textContent = "错误详情已复制，可以粘贴到反馈消息里。";
  } catch {
    field.focus();
    field.select();
    byId("copy-result").textContent = "已选中错误详情，按 Ctrl+C 复制。";
  }
});
for (const dialog of document.querySelectorAll("dialog")) {
  if (dialog.id === "error-dialog" || dialog.id === "group-options") continue;
  const panel = document.createElement("div");
  panel.className = "dialog-feedback";
  panel.setAttribute("role", "status");
  dialog.append(panel);
  dialog.addEventListener("close", () => panel.replaceChildren());
}
window.addEventListener("error", (event) => reportError("界面遇到问题", event.error || event.message));
window.addEventListener("unhandledrejection", (event) => reportError("操作未完成", event.reason));
