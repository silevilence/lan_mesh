package org.lanmesh.android

import org.junit.Assert.assertEquals
import org.junit.Test
import java.util.UUID

class ConversationTest {
    @Test fun scopesBroadcastAndDirectMessagesByPeer() {
        val alice = UUID.randomUUID()
        val bob = UUID.randomUUID()
        fun entry(peer: UUID?, own: Boolean) = ChatEntry(UUID.randomUUID(), peer, own, "设备", "消息")
        val broadcast = entry(null, false)
        val incoming = entry(alice, false)
        val outgoing = entry(alice, true)
        val messages = listOf(broadcast, incoming, entry(bob, false), outgoing)
        assertEquals(listOf(broadcast), messages.conversation(null))
        assertEquals(listOf(incoming, outgoing), messages.conversation(alice))
    }
}
