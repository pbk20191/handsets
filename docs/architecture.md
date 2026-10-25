# Architecture

```
host                                  device (shell UID, app_process)
─────                                  ──────────────────────────────
hs <verb> ──► TCP forward ────►     HsServer → Dispatch (one coroutine per connection)
                                       ├─ Server.respond     sync verbs (Pm Am Wm Props Providers …)
                                       ├─ Gestures           input, wait_for_*, tap_and_*
                                       ├─ Transfers          push/pull, install, uninstall
                                       ├─ ProcStream         dumpsys logcat shell monitor clip_watch
                                       ├─ Streams            stream, stream_tilejpeg, stream_h264
                                       ├─ State + StateRefresher  (push-cached snapshot, state_watch)
                                       └─ UiEvents / Waits   (accessibility event fan-out)
```

The daemon runs as the shell UID via `app_process` with hidden-API
restrictions lifted. The host runs a background `state-daemon` that
subscribes to `state_watch` and atomically rewrites
`~/.handsets/state-<port>.json` on every event-driven refresh. `hs info` /
`hs info` / `hs show` all read straight out of that file.

## Threads

One main thread runs `Looper.loop()`. Sockets, process pipes and the dumpsys
pipe are registered with the main `MessageQueue` (epoll) through `Io`; each
connection is a `Conn` driven by a coroutine on `Dispatchers.Main`.
Accessibility events, display/package/config listeners, `ImageReader` and
`MediaCodec` callbacks, and all timers (`delay`) run there too. Nothing on the
main thread may block.

`Loop.Work` (`hs-work-1..4`) takes CPU work and binder calls that can take
long: JPEG/PNG encoding, tile diffs, mirror pixel copies, accessibility tree
dumps, PackageInstaller and file I/O. Results come back to main.

Output is queued per connection: `send` applies backpressure past 4 MiB;
`sendLatest` keeps only the newest unsent frame (JPEG stream, `state_watch`).
H.264 never drops: an encoder output buffer is released only after its frame
is queued, so a slow client stalls just its own encoder.

`wire-smoke.py run` / `diff` (in `scripts/`) records normalized responses for
every verb so two daemon builds can be compared on a device.

## Layout

```
src/dev/handsets/daemon/      on-device daemon (Java + Kotlin → dex → jar)
handsets-cli/                 host CLI — short-verb surface (`hs use`, `hs see`, …)
handsets-viewer/              GUI mirror — winit + Metal + zero-copy VideoToolbox
build.gradle.kts              javac/kotlinc → R8 (SDK) → dex → build/hs.jar  (build.sh wraps it)
```
