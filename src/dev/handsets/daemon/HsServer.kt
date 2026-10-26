package dev.handsets.daemon

import android.net.LocalServerSocket
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import kotlinx.coroutines.launch
import java.io.FileDescriptor
import java.lang.reflect.Field
import java.lang.reflect.Method

/**
 * Listening socket in the abstract Unix namespace (`@hsd_<port>`), reached
 * from the host through `adb forward tcp:N localabstract:hsd_N`. Accepted on
 * the main looper.
 *
 * Unlike a TCP port on 127.0.0.1, a Unix socket tells us the peer's uid, so
 * only adbd (shell) and root get in; apps on the device are refused.
 */
internal class HsServer(private val port: Int, private val dispatch: Dispatch) {

    // LocalServerSocket closes its socket when finalized, so this reference
    // must stay live for the life of the process. [close] reads it, which
    // also stops R8 from dropping the field as write-only.
    private lateinit var server: LocalServerSocket
    private lateinit var listenFd: FileDescriptor

    fun start() {
        server = LocalServerSocket(socketName(port))
        listenFd = server.fileDescriptor
        nonBlocking(listenFd)
        Io.watch(listenFd, Io.IN or Io.ERR) { acceptAll(); Io.IN or Io.ERR }
        Logx.boot("hsd listening on @${socketName(port)}")
    }

    /** Stops accepting and frees the socket name, so a restarted daemon can bind it at once. */
    fun close() {
        Io.unwatch(listenFd)
        try { server.close() } catch (_: Throwable) {}
    }

    private fun acceptAll() {
        while (true) {
            val fd = try {
                Os.accept(listenFd, null)
            } catch (e: ErrnoException) {
                if (e.errno != OsConstants.EAGAIN && e.errno != OsConstants.EINTR) {
                    Logx.w("accept failed", e)
                }
                return
            } ?: return
            val uid = peerUid(fd)
            if (uid !in ALLOWED_UIDS) {
                Logx.w("refused connection from uid $uid")
                try { Os.close(fd) } catch (_: Throwable) {}
                continue
            }
            try {
                nonBlocking(fd)
            } catch (e: ErrnoException) {
                try { Os.close(fd) } catch (_: Throwable) {}
                continue
            }
            val c = Conn(fd)
            c.scope.launch { dispatch.session(c) }
        }
    }

    companion object {
        /** adbd runs as shell; root covers rooted devices and `adb root`. */
        private val ALLOWED_UIDS = setOf(0, 2000, Os.getuid())

        fun socketName(port: Int) = "hsd_$port"

        fun nonBlocking(fd: FileDescriptor) {
            val fl = Os.fcntlInt(fd, OsConstants.F_GETFL, 0)
            Os.fcntlInt(fd, OsConstants.F_SETFL, fl or OsConstants.O_NONBLOCK)
        }

        // Os.getsockoptUcred and StructUcred are hidden; restrictions are
        // lifted in Main before the server starts.
        private val getsockoptUcred: Method? by lazy {
            try {
                Os::class.java.getMethod("getsockoptUcred",
                    FileDescriptor::class.java, Int::class.javaPrimitiveType, Int::class.javaPrimitiveType)
            } catch (t: Throwable) {
                Logx.e("getsockoptUcred unavailable; refusing all connections", t)
                null
            }
        }
        private val ucredUid: Field? by lazy {
            try { Class.forName("android.system.StructUcred").getField("uid") } catch (_: Throwable) { null }
        }

        /** Kernel-reported uid of the connecting process, or -1 if unknown (refused). */
        private fun peerUid(fd: FileDescriptor): Int = try {
            val cred = getsockoptUcred!!.invoke(null, fd, OsConstants.SOL_SOCKET, OsConstants.SO_PEERCRED)
            ucredUid!!.getInt(cred)
        } catch (t: Throwable) {
            Logx.w("SO_PEERCRED failed", t)
            -1
        }
    }
}
