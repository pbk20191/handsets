package dev.handsets.daemon

import android.net.LocalServerSocket
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import kotlinx.coroutines.launch
import java.io.FileDescriptor
import java.io.IOException

/**
 * Listening socket in the abstract Unix namespace (`@hsd_<port>`), reached
 * from the host through `adb forward tcp:N localabstract:hsd_N`. Accepted on
 * the main looper.
 *
 * Unlike a TCP port on 127.0.0.1, a Unix socket tells us the peer's uid, so
 * only adbd (shell) and root get in; apps on the device are refused.
 */
internal class HsServer(private val port: Int, private val dispatch: Dispatch) {

    // LocalServerSocket (like LocalSocket) closes its socket when finalized,
    // so this reference must stay live for the life of the process. [close] reads it, which
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
            // listenFd is non-blocking, so accept() returns EAGAIN (wrapped
            // in an IOException) once the backlog is drained.
            val socket = try {
                server.accept()
            } catch (e: IOException) {
                val errno = (e.cause as? ErrnoException)?.errno
                if (errno != OsConstants.EAGAIN && errno != OsConstants.EINTR) {
                    Logx.w("accept failed", e)
                }
                return
            }
            val uid = try { socket.peerCredentials.uid } catch (_: IOException) { -1 }
            if (uid !in ALLOWED_UIDS) {
                Logx.w("refused connection from uid $uid")
                try { socket.close() } catch (_: Throwable) {}
                continue
            }
            try {
                nonBlocking(socket.fileDescriptor)
            } catch (_: ErrnoException) {
                try { socket.close() } catch (_: Throwable) {}
                continue
            }
            val c = Conn(socket)
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
    }
}
