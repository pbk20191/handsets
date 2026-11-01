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
  server kills and restarts it, which drops open connections and can hang a
  pipeline waiting on output. Point everything at one SDK, or pin
  `MOBILEAX_ADB`.
- A set-but-empty `ANDROID_HOME` makes `android info` report an empty SDK
  path; `mobax` drops empty `ANDROID_HOME` / `ANDROID_SDK_ROOT` before asking it.
- `android emulator start` has no `-gpu` flag; host GPU rendering has to come
  from `hw.gpu.mode=host` in the AVD's `config.ini` ([Emulators](emulator.md)).
- Running one AVD more than once needs `-read-only` on every instance, and a
  read-only emulator refuses snapshot save and load ("Snapshot save is
  disabled because "-read-only" was specified"). Keep the emulator you reset
  with snapshots as a single writable instance.
- After a snapshot load or a reboot done by another tool, the daemon on the
  device may be gone. `mobax` and the MCP server start it again on the next
  call; `@N` refs from before are refused and the current screen comes back.
- `logcat -t N` takes the last N lines *before* its priority, tag and regex
  filters, so `logcat -t 50 *:E` is often empty. `mobax logs --level / --tag /
  --grep / --exclude` reads the buffer and keeps the last N matching lines
  itself.
- `mobax logs --grep` is logcat's own regex (ECMAScript, on the device). It has
  no case-insensitive flag, so `-i` spells each letter both ways (`[aA]`);
  letters at the ends of a class range like `[a-z]` stay as written.
  `--exclude` is plain text, matched on the host.
- `mobax logs --since 5m` reads the device's clock, not the host's.
