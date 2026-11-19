package org.lanmesh.android

import android.Manifest
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import android.os.PowerManager
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

class MainActivity : ComponentActivity() {
    private var service by mutableStateOf<MeshService?>(null)
    private var batteryRestricted by mutableStateOf(false)
    private val notificationPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}
    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            service = (binder as MeshService.LocalBinder).service
        }
        override fun onServiceDisconnected(name: ComponentName) { service = null }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        bindService(Intent(this, MeshService::class.java), connection, Context.BIND_AUTO_CREATE)
        setContent {
            val connected = service
            if (connected == null) Box(Modifier.fillMaxSize(), contentAlignment = androidx.compose.ui.Alignment.Center) {
                CircularProgressIndicator()
            } else MeshApp(connected, batteryRestricted, ::openBatterySettings, ::requestConnectionNotification)
        }
    }

    override fun onStart() {
        super.onStart()
        MeshService.restore(this)
    }

    override fun onDestroy() {
        unbindService(connection)
        service = null
        super.onDestroy()
    }

    override fun onResume() {
        super.onResume()
        batteryRestricted = !getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(packageName)
    }

    internal fun requestConnectionNotification() {
        if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
            != PackageManager.PERMISSION_GRANTED) notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
    }

    private fun openBatterySettings() {
        val intent = Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)
        if (intent.resolveActivity(packageManager) != null) startActivity(intent)
        else startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
            android.net.Uri.fromParts("package", packageName, null)))
    }
}

@Composable
internal fun BatterySettings(open: () -> Unit) {
    var show by remember { mutableStateOf(false) }
    TextButton(onClick = { show = true }) { Text("后台连接设置") }
    if (show) AlertDialog(onDismissRequest = { show = false }, title = { Text("保持后台连接") },
        text = { Text("如果锁屏后经常掉线，可在系统电池优化列表中找到 LAN Mesh，允许它在后台运行。") },
        confirmButton = { TextButton(onClick = { show = false; open() }) { Text("打开系统设置") } },
        dismissButton = { TextButton(onClick = { show = false }) { Text("关闭") } })
}
