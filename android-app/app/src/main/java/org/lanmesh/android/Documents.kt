package org.lanmesh.android

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.io.IOException

internal suspend fun Context.withPickedFile(uri: Uri, send: suspend (File, String) -> Unit) {
    withContext(Dispatchers.IO) {
        val name = contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { if (it.moveToFirst()) it.getString(0) else null }
        val file = File.createTempFile("outgoing-", ".bin", cacheDir)
        try {
            val input = contentResolver.openInputStream(uri) ?: throw IOException("无法打开所选文件")
            input.use { src -> file.outputStream().use { dst -> src.copyTo(dst) } }
            send(file, name?.takeIf { it.isNotBlank() } ?: "文件")
        } finally { file.delete() }
    }
}
