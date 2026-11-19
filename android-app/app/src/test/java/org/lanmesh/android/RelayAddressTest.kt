package org.lanmesh.android

import org.junit.Assert.*
import org.junit.Test

class RelayAddressTest {
    @Test fun numericAddresses() {
        assertEquals("192.168.1.20:4321", relayAddress(" 192.168.1.020 ", "4321"))
        assertEquals("[::1]:1", relayAddress("[::1]", "1"))
    }

    @Test fun rejectsInvalidEndpoints() {
        for ((host, port) in listOf("300.1.2.3" to "5", "localhost" to "9", "::1" to "65536",
            "1.2.3.4" to "0", "1.2.3.4" to "abc")) {
            assertThrows(IllegalArgumentException::class.java) { relayAddress(host, port) }
        }
    }
}
