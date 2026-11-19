package org.lanmesh.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Binder
import android.os.IBinder
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID

internal data class GroupView(
    val config: GroupConfig,
    val online: Boolean = false,
    val error: String? = null,
    val members: List<Member> = emptyList(),
    val messages: List<ChatEntry> = emptyList()
)

private class GroupRuntime(val config: GroupConfig) {
    var session: MeshSession? = null
    var membersJob: Job? = null
    var membersDirty = false
}

class MeshService : Service() {
    inner class LocalBinder : Binder() { internal val service get() = this@MeshService }
    private val binder = LocalBinder()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val runtimes = mutableMapOf<UUID, GroupRuntime>()
    private val current = MutableStateFlow<Map<UUID, GroupView>>(emptyMap())
    internal val groups = current.asStateFlow()
    private var foreground = false
    private lateinit var monitor: MeshNetwork
    private val currentNetwork = MutableStateFlow("正在获取网络状态")
    internal val network = currentNetwork.asStateFlow()

    override fun onCreate() {
        super.onCreate()
        monitor = MeshNetwork(this) { route ->
            currentNetwork.value = route.transport
            for ((id, runtime) in runtimes) {
                val session = runtime.session ?: continue
                change(id) { it.copy(online = false) }
                scope.launch {
                    try { session.reconnect() }
                    catch (e: Exception) {
                        if (runtimes[id] === runtime) change(id) { it.copy(error = e.localizedMessage) }
                    }
                }
            }
        }
        monitor.start()
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, "群组连接", NotificationManager.IMPORTANCE_LOW)
        )
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // The launcher requests foreground startup only from a visible activity.
        startForeground(NOTICE, notification())
        foreground = true
        val saved = JSONArray(getSharedPreferences(PREFS, MODE_PRIVATE).getString("groups", "[]"))
        for (i in 0 until saved.length()) {
            val config = GroupConfig.from(saved.getJSONObject(i))
            if (config.groupId !in runtimes) attach(config)
        }
        updateNotification()
        return if (runtimes.isEmpty()) START_NOT_STICKY else START_STICKY
    }

    override fun onBind(intent: Intent): IBinder = binder

    internal suspend fun discover(): List<FoundRelay> = monitor.discover()

    internal fun join(relay: FoundRelay, nickname: String): UUID {
        val previous = runtimes[relay.group]?.config
        val config = GroupConfig(relay.group, previous?.deviceId ?: UUID.randomUUID(),
            relay.address, relay.name.ifBlank { relay.address }, nickname)
        if (previous != config || current.value[relay.group]?.error != null) {
            detach(relay.group)
            attach(config)
            persist()
        }
        if (!foreground) startForegroundService(Intent(this, MeshService::class.java))
        return relay.group
    }

    internal fun leave(id: UUID) {
        detach(id)
        persist()
        updateNotification()
    }

    private fun attach(config: GroupConfig) {
        val runtime = GroupRuntime(config)
        val id = config.groupId
        runtimes[id] = runtime
        current.value += id to GroupView(config)
        scope.launch {
            // Opening registers a native handle. A close or replacement during open must reclaim it.
            try {
                withContext(NonCancellable) {
                    val session = MeshSession.open(config, cacheDir) { raw ->
                        scope.launch {
                            if (runtimes[id] === runtime) event(runtime, JSONObject(raw))
                        }
                    }
                    if (runtimes[id] !== runtime) session.close() else {
                        runtime.session = session
                        refreshMembers(runtime)
                    }
                }
            } catch (e: Exception) {
                if (runtimes[id] === runtime) change(id) { it.copy(error = e.message) }
            }
        }
    }

    private fun event(runtime: GroupRuntime, event: JSONObject) {
        val id = runtime.config.groupId
        when (event.getString("type")) {
            "connection" -> {
                change(id) { it.copy(online = event.getBoolean("online"), error = null) }
                refreshMembers(runtime)
                updateNotification()
            }
            "sent" -> editMessage(id, UUID.fromString(event.getString("id"))) { it.copy(percent = event.getInt("percent")) }
            "received" -> {
                val source = event.getString("source")
                val row = ChatEntry(UUID.fromString(event.getString("id")), event.nullableString("peer")?.let(UUID::fromString),
                    false, event.nullableString("sender") ?: source.take(8), event.getString("name"), event.getLong("time"),
                    file = true, state = event.getString("state"), percent = event.getInt("percent"), path = event.nullableString("path"))
                change(id) { view ->
                    val index = view.messages.indexOfFirst { it.id == row.id }
                    val entries = if (index < 0) view.messages + row else view.messages.toMutableList().also { it[index] = row }
                    view.copy(messages = entries)
                }
            }
            "text" -> {
                val source = UUID.fromString(event.getString("source"))
                val sender = event.nullableString("name") ?: current.value[id]?.members?.find { it.id == source }?.name
                    ?: source.toString().take(8)
                val entry = ChatEntry(UUID.fromString(event.getString("id")), event.nullableString("peer")?.let(UUID::fromString),
                    false, sender, event.getString("content"), event.getLong("time"))
                change(id) { it.copy(messages = it.messages + entry) }
            }
            "members" -> refreshMembers(runtime)
            "warning" -> change(id) { it.copy(error = event.getString("text")) }
        }
    }

    private fun refreshMembers(runtime: GroupRuntime) {
        val session = runtime.session ?: return
        runtime.membersDirty = true
        if (runtime.membersJob?.isActive == true) return
        runtime.membersJob = scope.launch {
            do {
                runtime.membersDirty = false
                val members = try { session.members() } catch (e: Exception) { return@launch }
                if (runtimes[runtime.config.groupId] !== runtime) return@launch
                change(runtime.config.groupId) { it.copy(members = members) }
            } while (runtime.membersDirty)
        }
    }

    internal fun sendText(id: UUID, peer: UUID?, text: String) {
        val runtime = runtimes[id] ?: return
        val entry = ChatEntry(UUID.randomUUID(), peer, true, runtime.config.nickname, text, state = "发送中")
        send(runtime, entry) { session ->
            session.text(peer, text)
        }
    }

    internal fun sendFile(id: UUID, peer: UUID?, uri: android.net.Uri) {
        val runtime = runtimes[id] ?: return
        val entry = ChatEntry(UUID.randomUUID(), peer, true, runtime.config.nickname, "所选文件", file = true, state = "准备发送")
        send(runtime, entry) { session ->
            withPickedFile(uri) { file, name ->
                withContext(Dispatchers.Main) {
                    check(runtimes[id] === runtime) { "群组已关闭" }
                    editMessage(id, entry.id) { it.copy(text = name, state = "发送中") }
                }
                session.file(peer, entry.id, file, name)
            }
        }
    }

    private fun send(runtime: GroupRuntime, entry: ChatEntry, operation: suspend (MeshSession) -> Unit) {
        val id = runtime.config.groupId
        change(id) { it.copy(messages = it.messages + entry) }
        scope.launch {
            val state = try {
                operation(checkNotNull(runtime.session) { "群组尚未连接" })
                if (entry.file) "已发送" else ""
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { "发送失败：${e.localizedMessage}" }
            if (runtimes[id] === runtime) editMessage(id, entry.id) { it.copy(state = state) }
        }
    }

    internal fun resumeFile(group: UUID, row: UUID) {
        val runtime = runtimes[group] ?: return
        val session = runtime.session ?: return
        scope.launch {
            try { session.resume(row) }
            catch (e: Exception) {
                if (runtimes[group] === runtime) change(group) { it.copy(error = e.localizedMessage) }
            }
        }
    }

    internal fun exportFile(group: UUID, row: UUID, destination: android.net.Uri) {
        val runtime = runtimes[group] ?: return
        val path = current.value[group]?.messages?.find { it.id == row }?.path ?: return
        scope.launch {
            val result = try {
                withContext(Dispatchers.IO) {
                    java.io.File(path).inputStream().use { input ->
                        val output = contentResolver.openOutputStream(destination, "wt") ?: error("无法写入所选位置")
                        output.use { input.copyTo(it) }
                    }
                }
                "已保存"
            } catch (e: Exception) { "保存失败：" + e.localizedMessage }
            if (runtimes[group] === runtime) editMessage(group, row) { it.copy(state = result) }
        }
    }

    private fun editMessage(group: UUID, id: UUID, edit: (ChatEntry) -> ChatEntry) {
        change(group) { view -> view.copy(messages = view.messages.map { if (it.id == id) edit(it) else it }) }
    }

    private fun change(id: UUID, edit: (GroupView) -> GroupView) {
        val old = current.value[id] ?: return
        current.value += id to edit(old)
    }

    private fun persist() {
        val saved = JSONArray()
        runtimes.values.forEach { saved.put(it.config.json()) }
        getSharedPreferences(PREFS, MODE_PRIVATE).edit().putString("groups", saved.toString()).apply()
    }

    private fun detach(id: UUID) {
        val runtime = runtimes.remove(id) ?: return
        current.value -= id
        runtime.membersJob?.cancel()
        runtime.session?.let { session -> scope.launch(NonCancellable) { session.close() } }
    }

    private fun notification(): Notification {
        val launch = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val online = current.value.values.count { it.online }
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_mesh_status)
            .setContentTitle("LAN Mesh")
            .setContentText("已连接 $online / ${runtimes.size} 个群组")
            .setContentIntent(launch).setOngoing(true).setOnlyAlertOnce(true).build()
    }

    private fun updateNotification() {
        if (!foreground) return
        if (runtimes.isEmpty()) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            foreground = false
            stopSelf()
        } else {
            getSystemService(NotificationManager::class.java).notify(NOTICE, notification())
        }
    }

    override fun onDestroy() {
        monitor.close()
        runtimes.keys.toList().forEach(::detach)
        scope.cancel()
        super.onDestroy()
    }

    companion object {
        private const val CHANNEL = "mesh_connections"
        private const val NOTICE = 1
        private const val PREFS = "mesh_sessions"
        internal fun restore(context: Context) {
            val saved = context.getSharedPreferences(PREFS, MODE_PRIVATE).getString("groups", "[]")
            if (saved != "[]") context.startForegroundService(Intent(context, MeshService::class.java))
        }
    }
}
