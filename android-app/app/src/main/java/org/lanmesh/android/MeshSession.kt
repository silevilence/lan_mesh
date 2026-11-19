package org.lanmesh.android

import androidx.annotation.Keep
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import org.json.JSONTokener
import java.io.File
import java.util.UUID

@Keep
internal object Native {
    init { System.loadLibrary("mesh_android") }
    external fun open(config: String, listener: MeshListener): Long
    external fun call(handle: Long, request: String): String
    external fun close(handle: Long)
}

@Keep
internal fun interface MeshListener {
    fun onEvent(json: String)
}

internal data class GroupConfig(
    val groupId: UUID, val deviceId: UUID, val address: String, val name: String, val nickname: String
) {
    fun json() = JSONObject().put("group_id", groupId.toString()).put("device_id", deviceId.toString())
        .put("address", address).put("name", name).put("nickname", nickname)

    companion object {
        fun from(json: JSONObject) = GroupConfig(UUID.fromString(json.getString("group_id")),
            UUID.fromString(json.getString("device_id")), json.getString("address"),
            json.getString("name"), json.getString("nickname"))
    }
}

internal data class Member(val id: UUID, val name: String, val online: Boolean)
internal data class FoundRelay(val group: UUID, val name: String, val address: String)

internal class MeshSession private constructor(private val handle: Long) {
    suspend fun members(): List<Member> = withContext(Dispatchers.IO) {
        val rows = JSONArray(Native.call(handle, request("members").toString()))
        List(rows.length()) {
            val item = rows.getJSONObject(it)
            val id = UUID.fromString(item.getString("id"))
            Member(id, item.nullableString("name") ?: id.toString().take(8), item.getBoolean("online"))
        }
    }

    suspend fun text(peer: UUID?, content: String): UUID = withContext(Dispatchers.IO) {
        val raw = Native.call(handle, request("text").put("peer", peer?.toString()).put("content", content).toString())
        UUID.fromString(JSONTokener(raw).nextValue() as String)
    }

    suspend fun file(peer: UUID?, id: UUID, file: File) = withContext(Dispatchers.IO) {
        Native.call(handle, request("file").put("peer", peer?.toString())
            .put("file_id", id.toString()).put("path", file.absolutePath).toString())
        Unit
    }

    suspend fun reconnect() = withContext(Dispatchers.IO) { Native.call(handle, request("reconnect").toString()); Unit }
    suspend fun close() = withContext(Dispatchers.IO) { Native.close(handle) }

    companion object {
        private fun request(type: String) = JSONObject().put("type", type)

        suspend fun open(config: GroupConfig, listener: MeshListener): MeshSession =
            withContext(Dispatchers.IO) { MeshSession(Native.open(config.json().toString(), listener)) }

        suspend fun discover(): List<FoundRelay> = withContext(Dispatchers.IO) {
            val rows = JSONArray(Native.call(0, request("discover").toString()))
            List(rows.length()) {
                val row = rows.getJSONObject(it)
                FoundRelay(UUID.fromString(row.getString("group_id")), row.getString("group_name"), row.getString("tcp_addr"))
            }
        }

        suspend fun probe(address: String): UUID = withContext(Dispatchers.IO) {
            val raw = Native.call(0, request("probe").put("address", address).toString())
            UUID.fromString(JSONTokener(raw).nextValue() as String)
        }
    }
}

internal fun JSONObject.nullableString(key: String): String? = if (isNull(key)) null else getString(key)
