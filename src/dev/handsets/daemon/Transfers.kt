package dev.handsets.daemon

import android.content.Context
import android.content.Intent
import android.content.IntentSender
import android.content.pm.PackageInstaller
import android.os.Binder
import android.os.IBinder
import android.os.Parcel
import android.system.Os
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.OutputStream

/**
 * Receives a PackageInstaller status through a local Binder implementing only
 * `IIntentSender.send` (transaction 1, stable across API levels), so the
 * daemon needs no package name or BroadcastReceiver.
 */
class StatusReceiver : Binder() {
    private val result = CompletableDeferred<Intent>()

    val intentSender: IntentSender = IntentSender::class.java
        .getDeclaredConstructor(IBinder::class.java).apply { isAccessible = true }.newInstance(this)

    /** null on timeout. */
    suspend fun await(timeoutMs: Long): Intent? = withTimeoutOrNull(timeoutMs) { result.await() }

    override fun onTransact(code: Int, data: Parcel, reply: Parcel?, flags: Int): Boolean {
        if (code != 1) return super.onTransact(code, data, reply, flags)
        try { data.enforceInterface("android.content.IIntentSender") } catch (_: Throwable) {}
        var intent: Intent? = null
        try {
            data.readInt()   // sendCode
            if (data.readInt() != 0) intent = Intent.CREATOR.createFromParcel(data)
        } catch (_: Throwable) {}
        result.complete(intent ?: Intent())
        reply?.writeNoException()
        return true
    }
}

/**
 * pull / push / install / install_multi / pm_uninstall. Wire format: chunked
 * `[len][bytes]… [len=0]` in either direction, then one reply frame. Socket
 * I/O stays on the looper; file and PackageInstaller I/O runs on Work.
 */
internal class Transfers(private val ctx: Context, private val installer: Installer) {

    // ── chunk streams ───────────────────────────────────────────────────

    /** Reads chunk frames until len=0, handing each to [sink] on Work. */
    private suspend fun Conn.copyChunks(sink: OutputStream): Long {
        var total = 0L
        while (true) {
            val n = readInt()
            if (n == 0) return total
            if (n < 0 || n > MAX_CHUNK) throw IOException("bad-chunk-size:$n")
            val chunk = readFully(n)
            withContext(Loop.Work) { sink.write(chunk) }
            total += n
        }
    }

    /** Best-effort discard of an inbound chunk stream after a fatal error. */
    private suspend fun Conn.drainChunks() {
        try {
            while (true) {
                val n = readInt()
                if (n <= 0 || n > MAX_CHUNK) return
                readFully(n)
            }
        } catch (e: IOException) {
        }
    }

    suspend fun pull(c: Conn, path: String?) {
        if (path == null) return c.errAndEnd("pull-needs-path")
        val f = File(path)
        // exists/isDirectory can stall on FUSE-backed storage: ask on Work.
        val (exists, isDir) = withContext(Loop.Work) { f.exists() to f.isDirectory }
        if (!exists) return c.errAndEnd("not-found:$path")
        if (isDir) return c.errAndEnd("is-directory:$path")
        var input: FileInputStream? = null
        try {
            input = withContext(Loop.Work) { FileInputStream(f) }
            while (true) {
                val chunk = withContext(Loop.Work) {
                    val buf = ByteArray(CHUNK)
                    val n = input!!.read(buf)
                    if (n > 0) buf.copyOf(n) else null
                } ?: break
                c.send(chunk)
            }
            c.sendRaw(END)
        } catch (e: IOException) {
            // Like the old Files.pull: terminate the stream, keep the connection.
            if (c.closed) throw e
            Logx.w("pull failed", e)
            c.sendRaw(END)
        } finally {
            input?.let { i -> withContext(NonCancellable + Loop.Work) { try { i.close() } catch (_: IOException) {} } }
        }
    }

    suspend fun push(c: Conn, path: String, mode: Int, expectedSize: Long): ByteArray {
        val parent = File(path).parentFile
        if (parent != null && !withContext(Loop.Work) { parent.isDirectory }) {
            c.drainChunks()
            return err("parent-missing:$parent")
        }
        var out: FileOutputStream? = null
        try {
            out = withContext(Loop.Work) { FileOutputStream(path, false) }
            val received = c.copyChunks(out)
            withContext(Loop.Work) {
                out.flush()
                try { out.fd.sync() } catch (_: IOException) {}
            }
            if (mode > 0) {
                try { Os.chmod(path, mode) } catch (t: Throwable) { return err("chmod-failed:${t.message}") }
            }
            if (expectedSize > 0 && received != expectedSize) {
                return err("size-mismatch:got=$received:want=$expectedSize")
            }
            return ok("ok bytes=$received")
        } catch (e: IOException) {
            return err("write-failed:${e.message}")
        } finally {
            out?.let { o -> withContext(NonCancellable + Loop.Work) { try { o.close() } catch (_: IOException) {} } }
        }
    }

    suspend fun install(c: Conn, size: Long, reinstall: Boolean, grantAll: Boolean): ByteArray =
        installSession(c, if (size > 0) longArrayOf(size) else longArrayOf(0), reinstall, grantAll, multi = false)

    suspend fun installMulti(c: Conn, sizes: LongArray, reinstall: Boolean, grantAll: Boolean): ByteArray {
        if (sizes.isEmpty()) { c.drainChunks(); return err("install-multi-no-sizes") }
        return installSession(c, sizes, reinstall, grantAll, multi = true)
    }

    private suspend fun installSession(c: Conn, sizes: LongArray, reinstall: Boolean, grantAll: Boolean, multi: Boolean): ByteArray {
        val pi = withContext(Loop.Work) { ctx.packageManager.packageInstaller }
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        val total = sizes.sum()
        if (total > 0) params.setSize(total)
        if (reinstall) Installer.trySetInstallFlags(params, 0x2)   // INSTALL_REPLACE_EXISTING
        // USER_ACTION_NOT_REQUIRED on API 31+; ignored before.
        Installer.trySetRequireUserAction(params, 2)

        val sessionId = try {
            withContext(Loop.Work) { pi.createSession(params) }
        } catch (e: IOException) {
            // Keep the socket in sync.
            if (multi) repeat(sizes.size) { c.drainChunks() } else c.drainChunks()
            return err("createSession-failed:${e.message}")
        }

        var session: PackageInstaller.Session? = null
        var written = 0L
        try {
            session = withContext(Loop.Work) { pi.openSession(sessionId) }
            for (i in sizes.indices) {
                val slot = if (i == 0) "base.apk" else "split_$i.apk"
                val want = sizes[i]
                val apk = withContext(Loop.Work) { session.openWrite(slot, 0, if (want > 0) want else -1) }
                val got = try {
                    c.copyChunks(apk).also { withContext(Loop.Work) { session.fsync(apk) } }
                } finally {
                    withContext(NonCancellable + Loop.Work) { try { apk.close() } catch (_: IOException) {} }
                }
                if (want > 0 && got != want) {
                    withContext(Loop.Work) { session.abandon() }
                    return err(if (multi) "split-$i-size-mismatch:got=$got:want=$want" else "size-mismatch:got=$got:want=$want")
                }
                written += got
            }

            val status = StatusReceiver()
            withContext(Loop.Work) { session.commit(status.intentSender) }
            val result = status.await(if (multi) 180_000 else 120_000) ?: return err("install-timeout")
            val code = result.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
            val pkg = result.getStringExtra(PackageInstaller.EXTRA_PACKAGE_NAME)
            val msg = result.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)
            if (code == PackageInstaller.STATUS_SUCCESS) {
                if (grantAll && pkg != null) withContext(Loop.Work) { installer.grantAllRequested(pkg) }
                return ok("ok package=${pkg ?: "?"}" + (if (multi) " splits=${sizes.size}" else "") + " bytes=$written")
            }
            return err("install-failed:status=$code pkg=$pkg msg=$msg")
        } catch (t: Throwable) {
            val s = session
            try { if (s != null) withContext(NonCancellable + Loop.Work) { s.abandon() } } catch (_: Throwable) {}
            if (t is kotlinx.coroutines.CancellationException) throw t
            return err((if (multi) "install-multi-threw:" else "install-threw:") + t.javaClass.simpleName + ":" + t.message)
        } finally {
            val s = session
            if (s != null) try { s.close() } catch (_: Throwable) {}
        }
    }

    suspend fun uninstall(pkg: String): ByteArray = try {
        val pi = withContext(Loop.Work) { ctx.packageManager.packageInstaller }
        val status = StatusReceiver()
        withContext(Loop.Work) { pi.uninstall(pkg, status.intentSender) }
        val result = status.await(60_000)
        if (result == null) err("uninstall-timeout")
        else {
            val code = result.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
            val msg = result.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)
            if (code == PackageInstaller.STATUS_SUCCESS) ok("ok package=$pkg")
            else err("uninstall-failed:status=$code msg=$msg")
        }
    } catch (t: Throwable) {
        if (t is kotlinx.coroutines.CancellationException) throw t
        err("uninstall-threw:${t.javaClass.simpleName}:${t.message}")
    }

    private suspend fun Conn.errAndEnd(tail: String) {
        send(err(tail))
        sendRaw(END)
    }

    private companion object {
        const val CHUNK = 256 * 1024
        const val MAX_CHUNK = 8 * 1024 * 1024
        val END = byteArrayOf(0, 0, 0, 0)
        fun ok(s: String) = s.toByteArray(Charsets.UTF_8)
        fun err(tail: String) = "ERR:$tail".toByteArray(Charsets.UTF_8)
    }
}
