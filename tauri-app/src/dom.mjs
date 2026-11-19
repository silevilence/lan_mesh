export const $ = (id) => document.getElementById(id);

export function emptyItem(content) {
  const node = document.createElement("div");
  node.className = "item muted";
  node.textContent = content;
  return node;
}

export function fillItem(node, title, badge, detail, online = false) {
  const head = document.createElement("div");
  head.className = "item-head";
  const label = document.createElement("span");
  label.className = "title";
  label.textContent = title;
  const tag = document.createElement("span");
  tag.className = online ? "badge online" : "badge";
  tag.textContent = badge;
  const description = document.createElement("div");
  description.className = "muted";
  description.textContent = detail;
  head.append(label, tag);
  node.append(head, description);
}

export async function whileBusy(button, label, action) {
  if (button.disabled) return;
  const original = button.textContent;
  button.disabled = true;
  button.textContent = label;
  try {
    await action();
  } finally {
    button.textContent = original;
    button.disabled = false;
  }
}
