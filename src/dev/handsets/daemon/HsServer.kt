package dev.handsets.daemon

import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import kotlinx.coroutines.launch
import java.io.FileDescriptor
import java.net.InetAddress

/** Listening socket on 127.0.0.1, accepted on the main looper. */
internal class HsServer(private val port: Int, private val dispatch: Dispatch) {

    private lateinit var listenFd: FileDescriptor

    fun start() {
        listenFd = Os.socket(OsConstants.AF_INET, OsConstants.SOCK_STREAM, 0)
        Os.setsockoptInt(listenFd, OsConstants.SOL_SOCKET, OsConstants.SO_REUSEADDR, 1)
        Os.bind(listenFd, InetAddress.getByName("127.0.0.1"), port)
        Os.listen(listenFd, 8)
        nonBlocking(listenFd)
        Io.watch(listenFd, Io.IN or Io.ERR) { acceptAll(); Io.IN or Io.ERR }
        Logx.boot("hsd listening on 127.0.0.1:$port")
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
            try {
                nonBlocking(fd)
                Os.setsockoptInt(fd, OsConstants.IPPROTO_TCP, OsConstants.TCP_NODELAY, 1)
            } catch (e: ErrnoException) {
                try { Os.close(fd) } catch (_: Throwable) {}
                continue
            }
            val c = Conn(fd)
            c.scope.launch { dispatch.session(c) }
        }
    }

    companion object {
        fun nonBlocking(fd: FileDescriptor) {
            val fl = Os.fcntlInt(fd, OsConstants.F_GETFL, 0)
            Os.fcntlInt(fd, OsConstants.F_SETFL, fl or OsConstants.O_NONBLOCK)
        }
    }
}
