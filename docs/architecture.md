# Architecture

```
host                                  device (shell UID, app_process)
─────                                  ──────────────────────────────
hs <verb> ──► adb forward ────►     HsServer → Dispatch (one coroutine per connection)
                                       ├─ Server.respond     sync verbs (Pm Am Wm Props Providers …)
                                       ├─ Gestures           input, wait_for_*, tap_and_*
                                       ├─ Transfers          push/pull, install, uninstall
                                       ├─ ProcStream         dumpsys logcat shell monitor clip_watch
                                       ├─ Streams            stream, stream_tilejpeg, stream_h264
                                       ├─ State + StateRefresher  (push-cached snapshot, state_watch)
                                       └─ UiEvents / Waits   (accessibility event fan-out)
```

The daemon runs as the shell UID via `app_process` with hidden-API
restrictions lifted. It listens on the abstract Unix socket `@hsd_<port>`
(host side: `adb forward tcp:<port> localabstract:hsd_<port>`) and refuses
any peer whose `SO_PEERCRED` uid isn't shell or root. The host runs a
background `state-daemon` that subscribes to `state_watch` and atomically rewrites
`~/.handsets/state-<port>.json` on every event-driven refresh. `hs info` /
`hs info` / `hs show` all read straight out of that file.

## Threads

One main thread runs `Looper.loop()`. Sockets, process pipes and the dumpsys
pipe are registered with the main `MessageQueue` (epoll) through `Io`; each
connection is a `Conn` driven by a coroutine on `Dispatchers.Main`.
Accessibility events, display/package/config listeners, `ImageReader` and
`MediaCodec` callbacks, and all timers (`delay`) run there too. Nothing on the
main thread may block.

Everything off main runs on the kotlinx-coroutines scheduler, so all of it
shares one worker pool (`DefaultDispatcher-worker-N`):

- `Loop.Work` (`Default.limitedParallelism(≤4)`): CPU work and binder calls
  that can take long — JPEG/PNG encoding, tile diffs, accessibility tree
  dumps, PackageInstaller and file I/O.
- `Loop.Blocking` (`IO`): calls that wait on a peer — binder dumps into a
  pipe a slow client drains, MediaCodec / VirtualDisplay setup and teardown.
- `Loop.FrameCopy` (`IO.limitedParallelism(1)`): mirror pixel copies, which
  block on the GPU fence once per frame.

Gestures pace MOVE events on `Choreographer` vsync callbacks; `stream fps=N`
sends on the first vsync after each slot. `state_watch` and `clip_watch`
collect `StateFlow`s (one clipboard poller per interval, only while watched).

Output is queued per connection: `send` applies backpressure past 4 MiB;
`sendLatest` keeps only the newest unsent frame (JPEG stream, `state_watch`).
`stream_h264` connections with the same parameters share one encoder; each
access unit is copied once. Joiners and lagging viewers restart from cached
SPS/PPS + the frames since the last IDR, so the shared encoder never waits.

`wire-smoke.py run` / `diff` (in `scripts/`) records normalized responses for
every verb so two daemon builds can be compared on a device.

## Layout

```
src/dev/handsets/daemon/      on-device daemon (Kotlin → dex → jar)
handsets-cli/                 host CLI — short-verb surface (`hs use`, `hs see`, …)
handsets-viewer/              GUI mirror — winit + Metal + zero-copy VideoToolbox
build.gradle.kts              AGP: kotlinc → R8 → unsigned APK → build/hs.jar  (build.sh wraps it)
```
