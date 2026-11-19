package org.lanmesh.android

import java.net.InetAddress

internal fun relayAddress(input: String, port: String): String {
    val host = input.trim().removeSurrounding("[", "]")
    val number = port.trim().toIntOrNull()
    require(number != null && number in 1..65535) { "端口范围为 1–65535" }
    if (':' in host) {
        require(runCatching { InetAddress.getByName(host) }.isSuccess) { "IPv6 地址无效" }
        return "[$host]:$number"
    }
    val parts = host.split('.')
    require(parts.size == 4 && parts.all { part -> part.isNotEmpty() && part.length <= 3 &&
        part.all { it in '0'..'9' } && part.toInt() in 0..255 }) { "请输入有效的中继 IP 地址" }
    return parts.joinToString(".") { it.toInt().toString() } + ":$number"
}
