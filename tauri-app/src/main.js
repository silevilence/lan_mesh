const events = document.querySelector("#events");

for (const name of [
  "mesh://neighbor-online",
  "mesh://neighbor-offline",
  "mesh://message-received",
  "mesh://member-changed",
  "mesh://transfer-progress",
]) {
  window.__TAURI__.event.listen(name, ({ payload }) => {
    events.textContent += `${name} ${JSON.stringify(payload)}\n`;
  });
}
