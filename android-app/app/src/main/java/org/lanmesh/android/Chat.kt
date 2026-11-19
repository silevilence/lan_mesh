package org.lanmesh.android

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.unit.dp
import java.text.DateFormat
import java.util.Date
import java.util.UUID

internal data class ChatEntry(
    val id: UUID,
    val peer: UUID?,
    val own: Boolean,
    val sender: String,
    val text: String,
    val time: Long = System.currentTimeMillis(),
    val file: Boolean = false,
    val state: String = "",
    val percent: Int? = null,
    val path: String? = null
)

internal fun List<ChatEntry>.conversation(peer: UUID?) = filter { it.peer == peer }

@Composable
internal fun ChatPane(service: MeshService, group: GroupView, peer: UUID?) {
    var draft by rememberSaveable(group.config.groupId.toString(), peer?.toString()) { mutableStateOf("") }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) service.sendFile(group.config.groupId, peer, uri)
    }
    var exporting by rememberSaveable { mutableStateOf<String?>(null) }
    val destination = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        val row = exporting?.let(UUID::fromString)
        if (row != null && uri != null) service.exportFile(group.config.groupId, row, uri)
        exporting = null
    }
    val focus = LocalFocusManager.current
    val entries = remember(group.messages, peer) { group.messages.conversation(peer).asReversed() }
    val scroll = rememberLazyListState()
    LaunchedEffect(entries.firstOrNull()?.id) {
        if (scroll.firstVisibleItemIndex <= 1 || entries.firstOrNull()?.own == true) scroll.scrollToItem(0)
    }
    Column(Modifier.fillMaxSize()) {
        if (entries.isEmpty()) Box(Modifier.fillMaxWidth().weight(1f), contentAlignment = Alignment.Center) {
            Text("暂无消息", color = MaterialTheme.colorScheme.onSurfaceVariant)
        } else LazyColumn(Modifier.fillMaxWidth().weight(1f), state = scroll, reverseLayout = true,
            contentPadding = PaddingValues(vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            items(entries, key = { it.id.toString() }) { entry ->
                MessageBubble(entry, group.online, save = {
                    exporting = entry.id.toString()
                    destination.launch(entry.text.substringAfterLast('/').substringAfterLast('\\').ifBlank { "文件" })
                }, resume = { service.resumeFile(group.config.groupId, entry.id) })
            }
        }
        HorizontalDivider()
        OutlinedTextField(draft, { draft = it }, Modifier.fillMaxWidth().padding(top = 8.dp),
            label = { Text(if (peer == null) "群消息" else "私聊消息") }, maxLines = 4)
        Row(Modifier.fillMaxWidth().padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(enabled = group.online, onClick = { picker.launch(arrayOf("*/*")) }) { Text("发送文件") }
            Button(enabled = group.online && draft.isNotBlank(), onClick = {
                service.sendText(group.config.groupId, peer, draft.trim())
                draft = ""
                focus.clearFocus()
            }) { Text("发送") }
        }
    }
}

@Composable
private fun MessageBubble(entry: ChatEntry, online: Boolean, save: () -> Unit, resume: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    val clock = remember { DateFormat.getTimeInstance(DateFormat.SHORT) }
    Column(Modifier.fillMaxWidth(), horizontalAlignment = if (entry.own) Alignment.End else Alignment.Start) {
        Text("${entry.sender} · ${clock.format(Date(entry.time))}", style = MaterialTheme.typography.labelSmall,
            color = colors.onSurfaceVariant)
        Surface(Modifier.padding(top = 3.dp).widthIn(max = 320.dp),
            shape = RoundedCornerShape(14.dp),
            color = if (entry.own) colors.primaryContainer else colors.surfaceContainerHigh) {
            Column(Modifier.padding(horizontal = 14.dp, vertical = 10.dp)) {
                if (entry.file) Text("文件", style = MaterialTheme.typography.labelSmall)
                Text(entry.text, style = MaterialTheme.typography.bodyLarge)
                if (entry.file && entry.percent != null) {
                    LinearProgressIndicator(progress = { entry.percent / 100f }, modifier = Modifier.width(160.dp).padding(vertical = 6.dp))
                }
                if (entry.state.isNotBlank()) Text(listOfNotNull(entry.state, entry.percent?.let { "$it%" }).joinToString(" · "),
                    style = MaterialTheme.typography.labelSmall)
                if (entry.path != null) TextButton(onClick = save) { Text("另存为") }
                else if (!entry.own && online && entry.state in listOf("接收中", "等待续传")) {
                    TextButton(onClick = resume) { Text("请求续传") }
                }
            }
        }
    }
}
