package org.lanmesh.android

import android.app.ActivityManager
import android.app.NotificationManager
import android.content.*
import android.os.IBinder
import android.os.SystemClock
import android.provider.MediaStore
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.SdkSuppress
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.*
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit

/** Runs against the Windows peer from android-app/acceptance/relay.rs in an isolated emulator. */
@RunWith(AndroidJUnit4::class)
@SdkSuppress(minSdkVersion = 29)
class InteropTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext
    private val device = UiDevice.getInstance(instrumentation)
    private val args = InstrumentationRegistry.getArguments()
    private lateinit var service: MeshService
    private val binding = CompletableFuture<MeshService>()
    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            binding.complete((binder as MeshService.LocalBinder).service)
        }
        override fun onServiceDisconnected(name: ComponentName) = Unit
    }

    private fun waitFor(label: String, predicate: () -> Boolean) {
        val end = SystemClock.elapsedRealtime() + 25_000
        while (!predicate()) {
            check(SystemClock.elapsedRealtime() < end) { "Timed out waiting for $label" }
            SystemClock.sleep(80)
        }
    }

    private fun find(selector: BySelector, timeout: Long = 10_000): UiObject2 =
        checkNotNull(device.wait(Until.findObject(selector), timeout)) { "Missing $selector" }
    private fun tap(text: String) { find(By.text(text)).click() }
    private fun send(text: String) {
        val input = By.clazz("android.widget.EditText")
        waitFor("message editor accepting input") {
            try {
                val field = device.findObject(input) ?: return@waitFor false
                if (field.text != text) field.text = text
                device.findObject(input)?.text == text
            } catch (_: StaleObjectException) { false }
        }
        tap("发送")
    }
    private fun foundMessage(group: UUID, text: String) =
        service.groups.value[group]?.messages?.any { it.text == text } == true
    private fun launch() {
        val intent = context.packageManager.getLaunchIntentForPackage(context.packageName)!!
        context.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }
    private fun log(stage: String) { android.util.Log.i("MeshAcceptance", stage) }

    @Test fun windowsRelayAndAndroidClient() {
        // Network toggling is restricted to the dedicated emulator run.
        assumeTrue(args.getString("isolatedEmulator") == "yes" && android.os.Build.HARDWARE in setOf("ranchu", "goldfish"))
        val portA = requireNotNull(args.getString("portA"))
        val groupA = UUID.fromString(args.getString("groupA"))
        val groupB = UUID.fromString(args.getString("groupB"))
        val peerA = UUID.fromString(args.getString("peerA"))
        val run = System.currentTimeMillis()
        val inputName = "mesh-acceptance-$run.bin"
        val savedName = "mesh-saved-$run.bin"
        val data = ByteArray(1024 * 1024 + 19) { (it * 17 + 3).toByte() }
        val hash = MessageDigest.getInstance("SHA-256").digest(data).joinToString("") { "%02x".format(it) }
        val content = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, inputName)
            put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream")
            put(MediaStore.Downloads.RELATIVE_PATH, "Download/")
        }
        val input = context.contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, content)!!
        context.contentResolver.openOutputStream(input)!!.use { it.write(data) }
        launch()
        context.bindService(Intent(context, MeshService::class.java), connection, Context.BIND_AUTO_CREATE)
        service = binding.get(15, TimeUnit.SECONDS)
        try {
            log("manual join")
            tap("加入群组")
            tap("手动加入")
            waitFor("manual fields") { device.findObjects(By.clazz("android.widget.EditText")).size == 3 }
            val fields = device.findObjects(By.clazz("android.widget.EditText"))
            fields[0].text = "Android-Test"
            fields[1].text = "10.0.2.2"
            fields[2].text = portA
            tap("连接群组")
            device.wait(Until.findObject(By.res("com.android.permissioncontroller", "permission_allow_button")), 2500)?.click()
            waitFor("Alpha connected with distant peer") {
                val view = service.groups.value[groupA]
                view?.online == true && view.members.any { it.id == peerA && it.online }
            }
            send("mesh-test-group-群聊📬")
            find(By.text("Peer-Alpha/mesh-test-group-群聊📬"))
            log("two-hop group reply")
            tap("成员")
            tap("Peer-Alpha")
            send("mesh-test-direct-私聊")
            find(By.text("Peer-Alpha/mesh-test-direct-私聊"))
            assertFalse(device.hasObject(By.text("Peer-Alpha/mesh-test-group-群聊📬")))
            assertTrue(service.groups.value[groupA]!!.messages.any { it.peer == peerA && it.text == "Peer-Alpha/mesh-test-direct-私聊" })
            log("isolated direct reply")

            tap("发送文件")
            if (!device.wait(Until.hasObject(By.text(inputName)), 2500)) downloadsFolder()
            tap(inputName)
            waitFor("verified returned file") {
                service.groups.value[groupA]?.messages?.any { it.peer == peerA && it.text == "mesh-roundtrip.bin" && it.path != null } == true
            }
            tap("另存为")
            downloadsFolder()
            find(By.clazz("android.widget.EditText")).text = savedName
            val save = device.findObject(By.res("android", "button1"))
                ?: find(By.text(java.util.regex.Pattern.compile("SAVE|Save|保存")))
            save.click()
            waitFor("export over SAF") {
                service.groups.value[groupA]?.messages?.any { it.state == "已保存" } == true
            }
            val actual = device.executeShellCommand("sha256sum /sdcard/Download/$savedName").substringBefore(' ')
            assertEquals(hash, actual)
            log("document pick, resume in both directions, SHA256, document export $hash")

            tap("返回")
            tap("返回")
            tap("加入群组")
            tap("Beta")
            waitFor("Beta connected") { service.groups.value[groupB]?.online == true }
            waitFor("two-group notification") {
                context.getSystemService(NotificationManager::class.java).activeNotifications.any {
                    it.notification.extras.getCharSequence("android.text").toString().contains("2 / 2")
                }
            }
            send("mesh-test-background")
            device.pressHome()
            waitFor("background receive") { foundMessage(groupB, "Peer-Beta/mesh-test-background") }
            val manager = context.getSystemService(ActivityManager::class.java)
            @Suppress("DEPRECATION")
            val services = manager.getRunningServices(99).filter { it.service.className == MeshService::class.java.name }
            assertEquals(1, services.size)
            assertTrue(services.single().foreground)
            assertFalse(foundMessage(groupA, "Peer-Beta/mesh-test-background"))
            log("discovery, two groups, single foreground notification and background receive")

            val identities = service.groups.value.mapValues { it.value.config.deviceId }
            device.executeShellCommand("svc data disable")
            device.executeShellCommand("svc wifi disable")
            waitFor("network loss") { service.network.value == "网络已断开" && service.groups.value.values.all { !it.online } }
            device.executeShellCommand("svc wifi enable")
            // Android may disable automatic association on this internet-isolated AVD.
            // Selecting the saved WLAN restores the network; Mesh must reconnect by itself.
            device.executeShellCommand("am start -f 0x10008000 -a android.settings.WIFI_SETTINGS")
            find(By.text("AndroidWifi"), 20_000).click()
            waitFor("Wi-Fi reconnect") {
                service.network.value == "Wi-Fi" && service.groups.value.values.count { it.online } == 2
            }
            assertEquals(identities, service.groups.value.mapValues { it.value.config.deviceId })
            launch()
            if (!device.wait(Until.hasObject(By.clazz("android.widget.EditText")), 2500)) tap("Beta")
            send("mesh-test-reconnected")
            find(By.text("Peer-Beta/mesh-test-reconnected"))
            log("Wi-Fi loss and verified reconnection of both identities")
        } finally {
            device.executeShellCommand("svc wifi enable")
            device.executeShellCommand("svc data enable")
            context.unbindService(connection)
            context.contentResolver.delete(input, null, null)
        }
    }

    @Test fun restoresIdentitiesAndStopsAfterLastGroup() {
        assumeTrue(args.getString("isolatedEmulator") == "yes" && android.os.Build.HARDWARE in setOf("ranchu", "goldfish"))
        val expected = org.json.JSONArray(context.getSharedPreferences("mesh_sessions", Context.MODE_PRIVATE)
            .getString("groups", "[]"))
        assertEquals("Run the interoperability test first", 2, expected.length())
        val identities = List(expected.length()) { GroupConfig.from(expected.getJSONObject(it)) }
            .associate { it.groupId to it.deviceId }
        launch()
        context.bindService(Intent(context, MeshService::class.java), connection, Context.BIND_AUTO_CREATE)
        service = binding.get(15, TimeUnit.SECONDS)
        try {
            waitFor("both sessions restored") { service.groups.value.values.count { it.online } == 2 }
            assertEquals(identities, service.groups.value.mapValues { it.value.config.deviceId })
            val beta = UUID.fromString(args.getString("groupB"))
            val original = service.groups.value.getValue(beta).config
            instrumentation.runOnMainSync {
                val relay = FoundRelay(beta, original.name, original.address)
                service.join(relay, "rejoin-1")
                service.join(relay, "rejoin-2")
            }
            waitFor("replacement Leaf connected") {
                val view = service.groups.value[beta]
                view?.online == true && view.config.deviceId == original.deviceId &&
                    view.members.any { it.id == original.deviceId && it.name == "rejoin-2" }
            }
            tap("Beta")
            send("mesh-test-restored")
            find(By.text("Peer-Beta/mesh-test-restored"))
            log("process restart retained device identities and restored messaging")
            tap("退出群组")
            tap("退出")
            waitFor("Beta removed") { beta !in service.groups.value }
            val alpha = service.groups.value.values.single()
            tap(alpha.config.name)
            tap("退出群组")
            tap("退出")
            waitFor("last group removed") { service.groups.value.isEmpty() }
            waitFor("connection notification removed") {
                context.getSystemService(NotificationManager::class.java).activeNotifications.isEmpty()
            }
            val saves = context.getSharedPreferences("mesh_sessions", Context.MODE_PRIVATE)
            assertEquals("[]", saves.getString("groups", ""))
            if (device.hasObject(By.text("后台连接设置"))) {
                tap("后台连接设置")
                tap("打开系统设置")
                waitFor("system battery settings") { device.currentPackageName == "com.android.settings" }
                device.pressBack()
            }
            log("last group release and battery settings flow")
        } finally { context.unbindService(connection) }
    }

    private fun downloadsFolder() {
        val drawer = device.findObject(By.desc(java.util.regex.Pattern.compile("Show roots|显示根目录|打开导航抽屉")))
            ?: device.findObject(By.desc(java.util.regex.Pattern.compile(".*[Nn]avigation.*")))
        if (drawer != null) {
            drawer.click()
            find(By.text(java.util.regex.Pattern.compile("Downloads|下载(内容)?"))).click()
        }
    }
}
