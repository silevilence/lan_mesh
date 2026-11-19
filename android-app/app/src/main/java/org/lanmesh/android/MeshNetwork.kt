package org.lanmesh.android

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

internal data class NetworkRoute(val handle: Long?, val transport: String, val iface: String?, val addresses: Set<String>)

internal class MeshNetwork(context: Context, private val changed: (NetworkRoute) -> Unit) {
    private val app = context.applicationContext
    private val connectivity = app.getSystemService(ConnectivityManager::class.java)
    private val handler = Handler(Looper.getMainLooper())
    private val lan = linkedSetOf<Network>()
    private val scan = Mutex()
    private var route: NetworkRoute? = null
    private var bound: Network? = null
    private var active = false
    private val update = Runnable { if (active) refresh() }

    private fun schedule() {
        handler.removeCallbacks(update)
        handler.post(update)
    }

    private fun watcher(local: Boolean) = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            if (local) lan.add(network)
            schedule()
        }
        override fun onLost(network: Network) {
            if (local) lan.remove(network)
            schedule()
        }
        override fun onLinkPropertiesChanged(network: Network, props: LinkProperties) = schedule()
        override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) = schedule()
    }
    private val defaultWatcher = watcher(false)
    private val lanWatcher = watcher(true)

    fun start() {
        active = true
        connectivity.registerDefaultNetworkCallback(defaultWatcher, handler)
        val local = NetworkRequest.Builder().clearCapabilities()
            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
            .addTransportType(NetworkCapabilities.TRANSPORT_ETHERNET).build()
        connectivity.registerNetworkCallback(local, lanWatcher, handler)
        refresh()
    }

    private fun refresh() {
        val default = connectivity.activeNetwork
        val defaultCaps = connectivity.getNetworkCapabilities(default)
        // Wi-Fi without internet can coexist with a cellular default route. Keep VPN defaults intact.
        val preferLocal = default == null || defaultCaps?.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) == true &&
            !defaultCaps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)
        val preferred = if (preferLocal) lan.lastOrNull() ?: default else default
        val wanted = preferred?.takeIf { it != default }
        if (bound != wanted && connectivity.bindProcessToNetwork(wanted)) bound = wanted
        val network = bound ?: default
        val props = network?.let(connectivity::getLinkProperties)
        val caps = connectivity.getNetworkCapabilities(network)
        val transport = when {
            network == null -> "网络已断开"
            caps?.hasTransport(NetworkCapabilities.TRANSPORT_VPN) == true -> "VPN"
            caps?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true -> "Wi-Fi"
            caps?.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) == true -> "以太网"
            caps?.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) == true -> "移动网络"
            else -> "网络已连接"
        }
        val current = NetworkRoute(network?.networkHandle, transport, props?.interfaceName,
            props?.linkAddresses.orEmpty().map { it.toString() }.toSet())
        if (current != route) {
            route = current
            changed(current)
        }
    }

    suspend fun discover(): List<FoundRelay> = scan.withLock {
        val wifi = app.getSystemService(WifiManager::class.java)
        val lock = wifi?.createMulticastLock("LAN Mesh discovery")
        lock?.acquire()
        try { MeshSession.discover() } finally { if (lock?.isHeld == true) lock.release() }
    }

    fun close() {
        active = false
        handler.removeCallbacks(update)
        connectivity.unregisterNetworkCallback(lanWatcher)
        connectivity.unregisterNetworkCallback(defaultWatcher)
        if (bound != null) connectivity.bindProcessToNetwork(null)
    }
}
