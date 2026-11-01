<p align="center">
  <img src="logo.svg" alt="mobileAX" width="540">
  <br><br>
  <em>Drive Android from the shell, in milliseconds. One jar; no app, no root.</em>
  <br><br>
  <a href="https://github.com/pbk20191/mobileax/releases/latest"><img alt="release" src="https://img.shields.io/github/v/release/pbk20191/mobileax?color=blue"></a>
  <a href="LICENSE"><img alt="license" src="https://img.shields.io/badge/license-MIT-green"></a>
</p>

> mobileAX started as a fork of [elliotgao2/handsets](https://github.com/elliotgao2/handsets)
> (MIT). The daemon here is built with the Android Gradle Plugin and listens
> on an abstract Unix socket. The PyPI package and the blog posts are upstream's.

---

```bash
$ mobax use                              # connect, start the on-device daemon
daemon up on tcp:9008

$ mobax ui                               # flat table of tappable nodes
fill  EditText  "Email"     #email     540,540
fill  EditText  "Password"  #password  540,640  [password]
tap   Button    "Continue"  #continue  540,860

$ mobax tap  "Continue"                  # text-lookup tap
$ mobax wait "Welcome"                   # block on success text
```

When more than one ADB device is attached, select one with `mobax use SERIAL` or
`mobax --device SERIAL use`. Use the serial shown by `adb devices`.

`ui → label → tap → wait`. Pipe `mobax ui` into a model, take the label, hand it back.
`mobax ui` and `mobax tap` cover the app's dialogs, popups and bottom sheets too;
image-only cells show up as `#grid_id[i]`, which `mobax tap` accepts.
Raw `screenshot` defaults to 768px-long-edge JPEG, the fast agent path;
`mobax see out.jpg` saves a native-resolution export unless you pass `--size 768`.
Use WebP for compact lossy exports and PNG only for debug/lossless files.

|  | **mobileAX** | `adb shell` | uiautomator2 | Appium |
|---|---|---|---|---|
| Single-call latency | **2–7 ms** | 40–700 ms | 30–100 ms | 100–500 ms |
| Find by label, not coords | **yes** | no | yes | yes |
| On-device install | **1 jar (~few hundred KB)** | none | 2 APKs + `atx-agent` | driver APK + Node server |
| Wire | TCP, length-prefixed binary | ADB protocol | HTTP/JSON | WebDriver |
| Driven from | **any language via subprocess** | shell / subprocess | Python only | multi-lang via WebDriver |

Reproduce with `mobax bench -n 50`; methodology in [docs/benchmark.md](docs/benchmark.md). uiautomator2 and Appium ship things mobileAX doesn't — recorders, pytest plugins, HTML reports, iOS support. mobileAX is the lean CLI for tap-heavy work where per-call cost matters.

## `mobax tui` — drive any Android app from your keyboard

<p align="center">
  <img src="tui.gif" alt="mobax tui — keyboard-driven Android inspector" width="720">
</p>

A live, video-stream terminal UI of the device's interactive elements. A
background watcher polls the accessibility tree at ~10 fps, so the list on
screen *is* the live state of the device — no manual refresh, no waiting for
idle, no race between tap and re-dump.

- **One screen for the whole flow.** `↑↓` / `jk` to move, **Enter** to tap a
  Button or open an input modal for an EditText, **PgDn/PgUp** or **Shift+J/K**
  to swipe, **←** / **Esc** for BACK, **q** to quit. No more copy-paste between
  `mobax ui` and `mobax tap "…"`.
- **What you see is what you'd script.** Rows render the same verb-led table
  as `mobax ui -i` — `fill EditText "Email" #email`, `tap Button "Continue"
  #continue` — so once the flow works in `tui`, you already know the
  selectors for `mobax run` automation.
- **Fast.** Actions are fire-and-forget over a dedicated socket; the watcher
  picks up the new screen on its next 100ms tick. Mash keys without waiting.
- **Works on animated pages.** No `wait_for_idle` blocking — even constantly
  animating screens render smoothly at 10–20 fps.

- **Sees popups.** Lists every app window, so popups and bottom sheets that
  live in their own window show up too. `mobax tui --active` lists only the
  focused window.

Open it with `mobax tui` after `mobax use`. Built as a sibling crate
(`mobax-tui`, [ratatui](https://ratatui.rs) + crossterm) so the core `mobax`
binary stays zero-dep.

## Selectors

CSS-like, Playwright-inspired:

```bash
mobax tap   'EditText[hint~=Email]'             --visible --unique
mobax fill  'EditText[hint~=Password]'          'hunter2'
mobax tap   'Button:has-text("Sign in")'        --visible --unique --timeout 5s
mobax wait  "Dashboard"                         --timeout 15s
```

Vocabulary: `#id` `[a=v]` `[a~=sub]` `[a^=pre]` `[a$=suf]` · `:visible :clickable :enabled :focused :checked :editable :heading` · `:has-text("x") :text-is("x")` · `:in(SEL) :below(SEL) :right-of(SEL) :near(SEL, PX)`. Comma is OR. Compose (and androidx) nodes also carry `role` and `state`, in the device's language: `mobax tap '[role=Switch][state=Off]'`. More patterns in [docs/cookbook.md](docs/cookbook.md).

## How it works

```
host                                  device (shell UID via app_process)
─────                                  ─────────────────────────────────
  mobax ─── adb forward ────► @mobaxd_9008 ─► Server.java
                                         ├─ accessibility dump
                                         ├─ binder reflection (Pm/Am/Wm/Settings)
                                         └─ screenshots + state mirror
```

The daemon listens on an abstract Unix socket (`@mobaxd_<port>`) and only accepts
adbd (shell) or root, so other apps on the device can't reach it.
Length-prefixed binary frames over `adb forward tcp:<port> localabstract:mobaxd_<port>`:

```
host → device:   [u32 BE len][ascii: verb [k=v ...]]
device → host:   [u32 BE len][bytes]              ok body
                 [u32 BE 0]                       end-of-stream
                 ERR:<CODE>:<detail>              failure
```

Full protocol: [docs/wire.md](docs/wire.md). Reflection details and sharp edges: [docs/architecture.md](docs/architecture.md), [docs/sharp-edges.md](docs/sharp-edges.md).

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/pbk20191/mobileax/main/install.sh | bash
```

Needs `adb`: `mobax` finds it through Google's Android CLI (`android info`), then `$ANDROID_HOME`, `$PATH` and Android Studio's default SDK; `MOBILEAX_ADB` overrides. macOS and Linux (Windows: the release zip). Pin a version with `MOBILEAX_VERSION=v0.3.3`. Emulator setup (Android CLI, host GPU): [docs/emulator.md](docs/emulator.md).

MCP: each release ships `mobileax-<platform>.mcpb`, a one-click bundle for MCP hosts such as Claude Desktop (it carries `mobax` and the daemon). Or point any MCP client at `mobax mcp` (stdio), which serves the agent loop as tools: `use`, `ui`, `screenshot`, `tap`, `type`, `fill`, `swipe`, `go`, `wait`, `find`, `open`, `close`, `logcat` (filter by app, level, tag or regex), `info`, `drop`, plus `mobax` for any other verb.

Python bindings: `pip install ./bindings/python` (not on PyPI yet).

```python
from mobileax import Session
with Session() as d:
    d.tap("Continue")
    d.wait(text="Welcome", timeout="15s")
```

For other languages, drive `mobax --json` as a subprocess and parse one JSON line per call.

### Building the daemon

`./gradlew mobaxJar` (or `./build.sh`, a thin wrapper) builds `build/mobax.jar`
with the Android Gradle Plugin: the daemon is an unsigned release APK whose
`classes.dex` R8 has shrunk together with its runtime libraries (kotlin-stdlib,
kotlinx-coroutines). `app_process` accepts an APK on `CLASSPATH`, so it ships
under the old name. The same project imports into Android Studio / IntelliJ
for navigation and completion.

The SDK is found via `local.properties` (`sdk.dir`) or `ANDROID_HOME`. AGP
downloads the pinned platform (`compileSdk = 36`) and build-tools if they are
missing, once the SDK licenses are accepted (`sdkmanager --licenses`).

---

Exit codes: `0` ok · `1` failure · `2` NOT_FOUND · `3` TIMEOUT · `4` AMBIGUOUS. Full structured `error.code` in `mobax --json` output for the long tail.

`mobax --help` is the verb table. Recipes in the [Cookbook](docs/cookbook.md), long-form posts on the [Blog](https://elliotgao2.github.io/handsets/blog/) (upstream).

Pre-1.0. CLI surface stable since v0.1.0. Wire protocol versioned via the `info` verb. MIT — see [LICENSE](LICENSE).
