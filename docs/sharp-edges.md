# Sharp edges

- Settings provider rejects our `app_process` identity; `SettingsDirect`
  uses `IActivityManager.getContentProviderExternal` (the `cmd content`
  path).
- `am start` goes via `IActivityTaskManager.startActivityAsUser` with
  `callingPackage="com.android.shell"`; the system Context can't host
  `startActivity` from our anonymous process.
- `BroadcastReceiver` registrations from our Context aren't routed —
  `appCounts` cache TTL'd at 4.5 s instead.
- Most binder lookups go via reflection so the jar compiles against the
  public SDK. If a new SDK level renames a method, widen the matcher in
  the relevant helper.

## adb and the Android SDK

- Two SDKs on one machine (say Android Studio's and the one `android info`
  reports) ship different adb versions. Whichever client talks to the other's
  server kills and restarts it, which drops port forwards and can hang a
  pipeline waiting on output. Point everything at one SDK, or pin
  `HANDSETS_ADB`.
- A set-but-empty `ANDROID_HOME` makes `android info` report an empty SDK
  path; `hs` drops empty `ANDROID_HOME` / `ANDROID_SDK_ROOT` before asking it.
- `android emulator start` has no `-gpu` flag; host GPU rendering has to come
  from `hw.gpu.mode=host` in the AVD's `config.ini` ([Emulators](emulator.md)).
- `logcat -t N` takes the last N lines *before* its priority, tag and regex
  filters, so `logcat -t 50 *:E` is often empty. `hs logs --level / --tag /
  --grep` reads the buffer and keeps the last N matching lines itself.
