package org.lanmesh.android

import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import java.util.UUID

@Composable
internal fun MeshApp(service: MeshService, batteryRestricted: Boolean, batterySettings: () -> Unit,
    requestNotifications: () -> Unit) {
    val groups by service.groups.collectAsStateWithLifecycle()
    var page by rememberSaveable { mutableStateOf("home") }
    var selected by rememberSaveable { mutableStateOf<String?>(null) }
    val group = selected?.let { groups[UUID.fromString(it)] }
    val back = { page = "home" }
    BackHandler(enabled = page != "home", onBack = back)
    val scheme = if (isSystemInDarkTheme()) darkColorScheme(primary = Color(0xFF75DBC9))
        else lightColorScheme(primary = Color(0xFF006B61), secondary = Color(0xFF42635D),
            primaryContainer = Color(0xFF9DF2E0), onPrimaryContainer = Color(0xFF003B34))
    MaterialTheme(colorScheme = scheme) {
        Surface(Modifier.fillMaxSize()) {
            Column(Modifier.safeDrawingPadding().imePadding().fillMaxSize()
                .widthIn(max = 720.dp).padding(horizontal = 20.dp)) {
                when {
                    page == "join" -> JoinScreen(service, back) { relay, nickname ->
                        selected = service.join(relay, nickname).toString()
                        page = "group"
                        requestNotifications()
                    }
                    page == "group" && group != null -> GroupScreen(group, back) {
                        service.leave(group.config.groupId)
                        back()
                    }
                    else -> {
                        Row(Modifier.fillMaxWidth().padding(vertical = 16.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.SpaceBetween) {
                            Text("我的群组", style = MaterialTheme.typography.headlineMedium)
                            Button(onClick = { page = "join" }) { Text("加入群组") }
                        }
                        if (groups.isEmpty()) {
                            Box(Modifier.fillMaxWidth().weight(1f), contentAlignment = Alignment.Center) {
                                Text("尚未加入群组", color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                        } else LazyColumn(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            items(groups.values.toList(), key = { it.config.groupId.toString() }) { item ->
                                FilledTonalCard(item.config.name, item.config.address, status(item)) {
                                    selected = item.config.groupId.toString()
                                    page = "group"
                                }
                            }
                        }
                        if (batteryRestricted) BatterySettings(batterySettings)
                    }
                }
            }
        }
    }
}

@Composable
private fun FilledTonalCard(title: String, detail: String, state: String, select: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surfaceContainerLow, shape = RoundedCornerShape(14.dp)) {
        ListItem(modifier = Modifier.clickable(onClick = select),
            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
            leadingContent = { Initial(title) }, headlineContent = { Text(title, maxLines = 2) },
            supportingContent = { Text(detail, maxLines = 1, overflow = TextOverflow.Ellipsis) },
            trailingContent = { Text(state, style = MaterialTheme.typography.labelMedium) })
    }
}

@Composable
internal fun Initial(name: String) {
    Box(Modifier.size(42.dp).background(MaterialTheme.colorScheme.primaryContainer, RoundedCornerShape(12.dp)),
        contentAlignment = Alignment.Center) {
        Text(name.take(1).uppercase(), color = MaterialTheme.colorScheme.onPrimaryContainer,
            fontWeight = FontWeight.SemiBold)
    }
}

internal fun status(group: GroupView) = if (group.online) "已连接" else "等待连接"

@Composable
internal fun ScreenTitle(text: String, back: () -> Unit, action: @Composable () -> Unit = {}) {
    Row(Modifier.fillMaxWidth().heightIn(min = 60.dp), verticalAlignment = Alignment.CenterVertically) {
        TextButton(onClick = back) { Text("返回") }
        Text(text, Modifier.weight(1f).padding(horizontal = 6.dp), maxLines = 1, overflow = TextOverflow.Ellipsis,
            style = MaterialTheme.typography.titleLarge)
        action()
    }
}

@Composable
private fun JoinScreen(service: MeshService, back: () -> Unit, join: (FoundRelay, String) -> Unit) {
    var manual by rememberSaveable { mutableStateOf(false) }
    var host by rememberSaveable { mutableStateOf("") }
    var port by rememberSaveable { mutableStateOf("") }
    var nickname by rememberSaveable { mutableStateOf(Build.MODEL) }
    var relays by remember { mutableStateOf(emptyList<FoundRelay>()) }
    var scanning by remember { mutableStateOf(false) }
    var joining by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var scan by remember { mutableIntStateOf(0) }
    val scope = rememberCoroutineScope()
    LaunchedEffect(manual, scan) {
        error = null
        if (!manual) {
            scanning = true
            try { relays = service.discover() }
            catch (e: CancellationException) { throw e }
            catch (e: Exception) { error = "发现失败：${e.localizedMessage}" }
            finally { scanning = false }
        }
    }
    ScreenTitle("加入群组", back)
    TabRow(selectedTabIndex = if (manual) 1 else 0) {
        Tab(selected = !manual, enabled = !joining, onClick = { manual = false }, text = { Text("附近群组") })
        Tab(selected = manual, enabled = !joining, onClick = { manual = true }, text = { Text("手动加入") })
    }
    Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp)) {
        OutlinedTextField(nickname, { nickname = it }, Modifier.fillMaxWidth(), singleLine = true,
            label = { Text("我的昵称") })
        if (manual) {
            OutlinedTextField(host, { host = it }, Modifier.fillMaxWidth(), singleLine = true,
                label = { Text("中继 IP 地址") }, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri))
            OutlinedTextField(port, { port = it }, Modifier.fillMaxWidth(), singleLine = true,
                label = { Text("TCP 端口") }, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number))
            Button(enabled = !joining && host.isNotBlank() && port.isNotBlank() && nickname.isNotBlank(), onClick = {
                joining = true
                error = null
                scope.launch {
                    try {
                        val address = relayAddress(host, port)
                        val group = MeshSession.probe(address)
                        join(FoundRelay(group, service.groups.value[group]?.config?.name ?: address, address), nickname.trim())
                    } catch (e: CancellationException) { throw e }
                    catch (e: Exception) { error = "加入失败：${e.localizedMessage}" }
                    finally { joining = false }
                }
            }) { Text(if (joining) "正在连接…" else "连接群组") }
        } else {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically) {
                Text("局域网中的群组", style = MaterialTheme.typography.titleMedium)
                TextButton(enabled = !scanning, onClick = { scan++ }) { Text("刷新") }
            }
            if (scanning) LinearProgressIndicator(Modifier.fillMaxWidth())
            else if (relays.isEmpty() && error == null) Text("未发现群组。可用中继的 IP 地址和端口手动加入。")
            for (relay in relays) {
                FilledTonalCard(relay.name, relay.address, "加入") {
                    if (nickname.isBlank()) error = "请输入昵称" else join(relay, nickname.trim())
                }
            }
        }
        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    }
}

@Composable
private fun GroupScreen(group: GroupView, back: () -> Unit, leave: () -> Unit) {
    var confirm by remember { mutableStateOf(false) }
    ScreenTitle(group.config.name, back) {
        TextButton(onClick = { confirm = true }) { Text("退出群组") }
    }
    Text("${status(group)} · ${group.config.address}", style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant)
    group.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    Text("成员", Modifier.padding(top = 20.dp), style = MaterialTheme.typography.titleMedium)
    MemberList(group)
    if (confirm) AlertDialog(onDismissRequest = { confirm = false }, title = { Text("退出此群组？") },
        text = { Text("将断开连接并移除本机保存的群组身份。") },
        confirmButton = { TextButton(onClick = leave) { Text("退出") } },
        dismissButton = { TextButton(onClick = { confirm = false }) { Text("取消") } })
}

@Composable
internal fun MemberList(group: GroupView, select: ((Member) -> Unit)? = null) {
    LazyColumn {
        items(group.members.sortedWith(compareByDescending<Member> { it.online }.thenBy { it.name }),
            key = { it.id.toString() }) { member ->
            val me = member.id == group.config.deviceId
            ListItem(modifier = if (select != null && !me) Modifier.clickable { select(member) } else Modifier,
                leadingContent = { Initial(member.name) },
                headlineContent = { Text(if (me) "${member.name}（我）" else member.name) },
                supportingContent = { Text(member.id.toString(), maxLines = 1, overflow = TextOverflow.Ellipsis) },
                trailingContent = { Text(if (group.online && member.online) "在线" else "离线") })
        }
    }
}
