"""Run against prepare-dist.ps1 output using Python Playwright and an installed Edge."""
from contextlib import contextmanager
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread

from playwright.sync_api import expect, sync_playwright


class Assets(SimpleHTTPRequestHandler):
    extensions_map = {".mjs": "text/javascript", ".js": "text/javascript"}

    def log_message(self, *args):
        pass


@contextmanager
def frontend():
    directory = Path(__file__).resolve().parents[1] / "dist"
    with ThreadingHTTPServer(("127.0.0.1", 0), partial(Assets, directory=str(directory))) as server:
        worker = Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}"
        finally:
            server.shutdown()
            worker.join()


BRIDGE = """
const group = {group_id:"g", device_id:"self", group_name:"项目讨论", role:"relay", status:"connected"};
const members = [{device_id:"peer", online:true, nickname:"同事"}];
const handlers = {};
window.emit = (name, data) => handlers[name]({payload:{group_id:"g", ...data}});
window.message = (content, at) => emit("message-received", {message:{type:"text",
  header:{source_device_id:"peer", timestamp_ms:at}, payload:{content}}});
window.__TAURI__ = {
  event: {listen: (name, fn) => {handlers[name.slice(7)] = fn}},
  core: {invoke: async (name) => {
    switch (name) {
      case "list_saved_groups": return [group];
      case "activate_saved_group": return group;
      case "get_members": return members.slice();
      case "get_connection_status": return {routes:[]};
      case "list_network_interfaces": return [];
      case "pick_file": throw "读取失败";
      case "app_version": return "0.4.1";
      case "check_update": return new Promise(resolve => {
        window.finishUpdateCheck = () => resolve({version:"2.0.0", body:"测试更新"});
      });
    }
  }}
};
"""


def rectangles(page, selectors):
    return {s: page.locator(s).bounding_box() for s in selectors}


def exercise(page, width):
    click = page.click
    narrow = width <= 680
    # Opening a saved conversation enters the chat pane at either breakpoint.
    click("#session-list .contact")
    expect(page.locator("#peer-title")).to_have_text("项目讨论")
    field = page.locator("#message-input")
    field.fill("还在写的草稿")
    field.evaluate("e => e.setSelectionRange(2, 4)")
    controls = ["#open-chat-view", "#open-settings-view", "#error-history",
                "#send-text button[type=submit]", "#message-input", "#attach-file", "#chat-options"]
    before = rectangles(page, controls)
    page.evaluate("""() => {
      for (let i=0; i<75; i++) message("讨论记录 " + i, 1000+i);
      emit("desktop-error",{operation:"连接中断，需要重新连接。".repeat(50), error:"offline"});
      emit("member-changed",{});
      emit("groups-changed",{});
      finishUpdateCheck();
    }""")
    expect(page.locator("#status")).to_contain_text("发现新版本")
    expect(field).to_be_focused()
    assert field.input_value() == "还在写的草稿"
    assert field.evaluate("e => [e.selectionStart, e.selectionEnd]") == [2, 4]
    assert rectangles(page, controls) == before

    # Earlier timestamps can arrive late. Preserve the visible message, including
    # its selected text, while an older row is inserted and new rows arrive below.
    anchor = page.get_by_text("讨论记录 31", exact=True)
    anchor.evaluate("""e => {
      e.scrollIntoView({block:'center'});
      const range = document.createRange(); range.selectNodeContents(e);
      getSelection().removeAllRanges(); getSelection().addRange(range);
    }""")
    top = anchor.bounding_box()["y"]
    page.evaluate("() => {message('迟到的消息', 500); message('刚到的消息', 4000)}")
    assert abs(anchor.bounding_box()["y"] - top) < 1
    assert page.evaluate("getSelection().toString()") == "讨论记录 31"
    page.evaluate("getSelection().removeAllRanges()")
    click("#open-settings-view")
    page.evaluate("message('后台补到的旧消息', 450)")
    click("#open-chat-view")
    expect(anchor).to_be_in_viewport()
    assert abs(anchor.bounding_box()["y"] - top) < 1
    click("#latest-message")
    expect(page.locator("#latest-message")).to_be_hidden()
    assert rectangles(page, controls) == before

    if narrow:
        click("#back-list")
    click("[data-panel=members]")
    target = page.locator("#member-list .contact")
    target.focus()
    box = target.bounding_box()
    page.mouse.move(box["x"] + 20, box["y"] + 12)
    page.mouse.down()
    page.evaluate("""() => {
      members.push({device_id:'aaa', online:true, nickname:'新成员'});
      members[0].nickname = '已更新的昵称';
      emit('member-changed',{});
    }""")
    expect(page.locator("#member-list .contact")).to_have_count(2)
    expect(target.first).to_be_focused()
    first = target.first.bounding_box()
    assert (box["x"], box["y"]) == (first["x"], first["y"])
    page.mouse.up()
    expect(page.locator("#peer-title")).to_contain_text("已更新的昵称")

    # A group message received in a direct chat marks the group without opening it.
    page.evaluate("message('群里有新消息', 5000)")
    expect(page.locator("#peer-title")).to_contain_text("已更新的昵称")
    expect(page.locator("[data-panel=groups]")).to_have_attribute("aria-label", "群组，有未读消息")
    if narrow:
        click("#back-list")
    click("[data-panel=files]")
    page.evaluate("""() => { window.progress = (id, index, status) => emit('transfer-progress',{
      file_id:id, file_name:'会议资料.pdf', chunk_index:index, chunk_count:2, direction:'incoming',
      from:'peer', total_size:100, status, path:status==='done'?'example.pdf':null
    }); progress('first',0,'running');progress('first',1,'done'); }""")
    save = page.locator("#transfers button:has-text('另存为')").first
    save.focus()
    save_box = save.bounding_box()
    page.evaluate("() => { progress('second',0,'running');progress('second',1,'done'); }")
    expect(save).to_be_focused()
    assert save.bounding_box() == save_box
    if narrow:
        click("#back-chat")
    click("#attach-file")
    page.fill("#file-path", "example.txt")
    send = page.locator("#send-file button[type=submit]")
    dialog_before = send.bounding_box()
    click("#pick-file")
    local_detail = page.locator("#file-dialog .dialog-feedback button")
    local_detail.focus()
    expect(local_detail).to_be_focused()
    page.evaluate("emit('desktop-error',{operation:'读取文件失败'.repeat(25),error:'test'})")
    assert dialog_before == send.bounding_box()
    expect(local_detail).to_be_focused()
    expect(page.locator("#file-dialog .dialog-feedback")).to_contain_text("选择文件失败")
    click("#close-file")
    click("#back-group")
    expect(field).to_have_value("还在写的草稿")
    assert page.evaluate("document.documentElement.scrollWidth <= innerWidth")
    print(f"{width}px: stable controls, caret, history anchor, pointer target and dialog")


if __name__ == "__main__":
    with frontend() as url, sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel="msedge", headless=True)
        for width, height in [(960, 640), (700, 520), (375, 667), (320, 480)]:
            page = browser.new_page(viewport={"width": width, "height": height}, locale="zh-CN")
            failures = []
            page.on("pageerror", lambda error: failures.append(str(error)))
            page.on("dialog", lambda dialog: (failures.append(dialog.message), dialog.dismiss()))
            page.add_init_script(BRIDGE)
            page.goto(url)
            exercise(page, width)
            assert not failures, failures
            page.close()
        browser.close()
