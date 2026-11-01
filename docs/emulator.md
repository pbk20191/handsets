# Emulators and the Android CLI

mobileAX drives any device `adb` can see, API 28 or later. For emulators the
recommended setup is Google's Android CLI (`android`) plus an AVD that
renders on the host GPU.

## Android CLI

Install the `android` CLI (on Windows: `winget install Google.AndroidCLI`).
`mobax` uses it to find `adb`, so nothing has to be on `PATH`:

```bash
android info                  # sdk: <path>  — adb is <path>/platform-tools/adb
android emulator list         # AVDs
android emulator start NAME   # returns once the emulator has booted
android emulator stop NAME
```

`android emulator start` takes `--cold` (skip the snapshot) and `--headless`
(no window); `android emulator create` makes new AVDs (`--help` lists its
options).

## Host GPU, always

Run emulators with host GPU rendering. `android emulator start` has no
`-gpu` option, so set it in the AVD's config, `~/.android/avd/NAME.avd/config.ini`:

```ini
hw.gpu.mode=host
```

Starting the emulator directly, pass it on the command line instead:

```bash
emulator -avd NAME -gpu host
```

The default `auto` can choose a slower renderer. Every mirror frame is read
back from the GPU (the daemon's main cost for `stream`), so the renderer
shows up directly in frame rates, and numbers from different GPU modes
aren't comparable.

## CPU and memory

The stock phone AVD has 4 cores and 2 GB. More helps: JPEG encoding, the app
under test and SurfaceFlinger all share the guest's CPUs. In the same
`config.ini`:

```ini
hw.cpu.ncore=6      # the emulator caps cores at 6
hw.ramSize=4096
```

On a Windows host (API 36 x86_64 AVD, 1080×2400, `-gpu host`) going from 4
cores / 2 GB to 6 cores / 4 GB took a native-size `jpeg` stream from 25–28 to
29–32 fps, `--size 1200` from 42–50 to 56 fps, and `h264` from 24 to 50 fps.

## Stream size

At native resolution the mirror's GPU readback (25–36 ms per 1080×2400 frame
on that host) caps a JPEG stream near 30 fps. Asking for a smaller frame cuts
the readback, the encode and the decode together: `mobax-viewer --size
1200` reaches about 56 fps, and the window rarely shows more pixels than that.
`h264` avoids the readback entirely.

## How `mobax` finds adb

In order, resolved once per process:

1. `$MOBILEAX_ADB`, then `$ADB`
2. `~/.mobileax/adb-path` — the cached result of 3 (`android info` takes ~0.7 s)
3. `android info` → `<sdk>/platform-tools/adb`
4. `$ANDROID_HOME`, then `$ANDROID_SDK_ROOT`
5. `adb` on `PATH`
6. Android Studio's default SDK (`~/Library/Android/sdk`, `~/Android/Sdk`,
   `%LOCALAPPDATA%\Android\Sdk`)

The last step is for MCP hosts and other GUI apps, which often start `mobax`
without the shell's `PATH`. See also [Sharp edges](sharp-edges.md#adb-and-the-android-sdk).
