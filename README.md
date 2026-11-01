<p align="center">
  <img src="logo.svg" alt="mobileAX" width="540">
  <br><br>
  <em>Accessibility-native Android control for agents and scripts.<br>One jar on the device: no app, no root, no host ports.</em>
  <br><br>
  <a href="https://github.com/pbk20191/mobileax/releases/latest"><img alt="release" src="https://img.shields.io/github/v/release/pbk20191/mobileax?color=blue"></a>
  <a href="LICENSE"><img alt="license" src="https://img.shields.io/badge/license-MIT-green"></a>
</p>

mobileAX reads an Android screen through its accessibility tree and acts on
it: one row per thing you can tap or fill, in a few milliseconds. It is
built for the loop an LLM agent runs (look, act, check) and for scripts
that need the same thing without a model. The AX stands for both:
accessibility, and the agent experience on top of it.

It is a small Rust CLI (`mobax`) on the host and one jar that runs as the
shell user on the device. The same binary is an MCP server.

> mobileAX started as a fork of [elliotgao2/handsets](https://github.com/elliotgao2/handsets) (MIT).

---

## The agent loop

Over MCP (`mobax mcp`), the screen comes back as rows numbered `@1`, `@2`, …
An action names a row, waits for the screen to settle, and replies with what
changed, so the agent rarely has to look again:

```
→ ui
com.example.mail · 6 rows
@1  tap   ImageButton   "Back"  44,120
@2  fill  EditText      "Email"  #email  540,540
@3  fill  EditText      "Password"  #password  540,640  [password]
@4  tap   CheckBox      "Remember me"  #remember  120,760  [check]
@5  tap   Button        "Continue"  #continue  540,860
@6  tap   LinearLayout  "Need help?" · "Reset your password"  540,1020

→ fill @2 "ada@example.com"
ok set_text
settled in 140 ms · com.example.mail
~ @2  fill  EditText  "ada@example.com"  #email  540,540

→ tap @5
ok
settled in 610 ms · com.example.mail · new screen, 4 rows
@1  -    TextView  "Welcome, Ada"  540,300
…

→ watch until "Inbox"
after 1830 ms · com.example.mail · new screen, 12 rows
…
```

- **One row per thing to act on.** A clickable container takes the text it
  holds (`"Need help?" · "Reset your password"`); dialogs, popups and bottom
  sheets are listed too. Compose roles and states come in the device's
  language (`[role=Switch][state=On]`).
- **Refs that can't misfire.** A row keeps its `@N` while it stays on the
  screen, even as it moves or its text changes. A ref from a screen that has
  since gone is refused, with the current screen in the reply, instead of
  tapping whatever is there now.
- **Replies carry the change.** `tap`, `fill`, `type`, `swipe`, `go` and
  `open` wait for the screen to settle (the device's own accessibility
  events, not a fixed sleep) and answer with `+` added, `~` changed, `-`
  removed rows, or the new screen. A screen still sliding in is waited out,
  so the coordinates in a reply are where the rows end up.
- **Several steps, one call.** `batch` runs a known sequence (tap the search
  bar, type, wait for a result) in one round trip, one line per step, then
  what changed over the whole batch.
- **Any language.** `type` inserts at the cursor, so Korean, CJK and emoji go
  in as written, and a Korean keyboard layout can't turn `abc` into `뮻`.
- **Waiting is free.** `watch` returns when the screen differs from what the
  agent last saw, or when some text shows. The device pushes changes, so
  nothing is polled while it waits.
- **Errors say what to do next.** `ERR:NOT_FOUND:…`, then: call `ui`, or
  `swipe` to scroll.

Each release ships `mobileax-<platform>.mcpb`, a one-click bundle for MCP hosts
such as Claude Desktop, or point any MCP client at `mobax mcp` (stdio). Tools:
`use`, `ui`, `screenshot`, `tap`, `type`, `fill`, `swipe`, `go`, `batch`,
`wait`, `watch`, `find`, `open`, `close`, `logcat`, `info`, `drop`, and `mobax`
for any other verb.

## From the shell

```bash
$ mobax use                              # start the daemon on the device
daemon up on emulator-5554

$ mobax ui                               # the same rows, without refs
fill  EditText  "Email"     #email     540,540
fill  EditText  "Password"  #password  540,640  [password]
tap   Button    "Continue"  #continue  540,860

$ mobax fill "Email" ada@example.com     # by label, id or selector
$ mobax tap  "Continue"
$ mobax wait "Welcome"                   # block until it shows
```

Every verb returns a structured exit code, and `--json` gives one JSON line
per call, so scripts can branch without parsing text. `mobax run SCRIPT`
runs a batch of verbs over one warm connection.

## Many devices at once

Devices are named by serial, and every connection goes through the adb
server itself, so a device costs no host port:

```bash
$ mobax use emulator-5554 && mobax use emulator-5556
$ mobax -s emulator-5554 open com.android.settings
$ mobax -s emulator-5556 open com.google.android.deskclock
$ mobax fan emulator-5554,emulator-5556 -- info     # the same verb on each, in parallel
```

Without `--device`, a call goes to `$MOBILEAX_SERIAL` / `$ANDROID_SERIAL`, the
device `mobax use` picked last, or the only one attached.

One MCP server drives several devices: each tool takes `device`, calls run in
parallel on warm connections, and actions on one device run one at a time.
Two emulators driven together finish in the time of the slower one, and `ui`
answers in about 10 ms. If another tool restarts a device or loads a
snapshot, the daemon is started again on the next call.

## Following the screen

```bash
$ mobax watch                  # until Ctrl-C
#1 com.android.settings · new screen, 10 rows
@1   tap  LinearLayout  "Search settings"  #search_action_bar  540,178
…
#2 com.android.settings
~ @3  tap  LinearLayout  "Battery" · "100%"  540,849
```

`mobax watch --json` prints one NDJSON line per change, for code that reacts
to the screen without a model (a test harness, a fast control loop). Nothing
is sent while the screen is still; a scroll or an animation in flight is sent
once it comes to rest (`--moves` sends it as it happens).

## `mobax tui`

<p align="center">
  <img src="tui.gif" alt="mobax tui — keyboard-driven Android inspector" width="720">
</p>

A live terminal view of the device's rows: `↑↓` / `jk` to move, **Enter** to
tap a button or fill a field, **PgDn/PgUp** to swipe, **Esc** for BACK. What it
shows is what you would script, so a flow that works in `tui` already has its
selectors for `mobax run`. Built as a sibling crate (`mobax-tui`) so `mobax`
itself stays dependency-free.

## Selectors

CSS-like, Playwright-inspired:

```bash
mobax tap   'EditText[hint~=Email]'             --visible --unique
mobax fill  'EditText[hint~=Password]'          'hunter2'
mobax tap   'Button:has-text("Sign in")'        --visible --unique --timeout 5s
mobax wait  "Dashboard"                         --timeout 15s
```

Vocabulary: `#id` `[a=v]` `[a~=sub]` `[a^=pre]` `[a$=suf]` · `:visible :clickable :enabled :focused :checked :editable :heading` · `:has-text("x") :text-is("x")` · `:in(SEL) :below(SEL) :right-of(SEL) :near(SEL, PX)`. Comma is OR. More patterns in [docs/cookbook.md](docs/cookbook.md).

## How it works

```
host                                     device (shell UID via app_process)
─────                                    ──────────────────────────────────
mobax / mobax mcp ── adb server ──► @mobaxd_9008 ─► MobaxServer → Dispatch
                                              ├─ accessibility tree + events
                                              ├─ input (UiAutomation)
                                              ├─ binder (Pm/Am/Wm/Settings)
                                              └─ screenshots, streams, state
```

The daemon listens on an abstract Unix socket and only accepts adbd (shell)
or root, so other apps on the device can't reach it. The host opens it
through the adb server (`host:transport:SERIAL`, then
`localabstract:mobaxd_<port>`); the adb server's address follows adb's own
`$ADB_SERVER_SOCKET` / `$ANDROID_ADB_SERVER_PORT`. Frames are length-prefixed:

```
host → device:   [u32 BE len][ascii: verb [k=v ...]]
device → host:   [u32 BE len][bytes]              ok body
                 [u32 BE 0]                       end-of-stream
                 ERR:<CODE>:<detail>              failure
```

Full protocol: [docs/wire.md](docs/wire.md). Internals and limits:
[docs/architecture.md](docs/architecture.md), [docs/sharp-edges.md](docs/sharp-edges.md).

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/pbk20191/mobileax/main/install.sh | bash
```

Needs `adb`: `mobax` finds it through Google's Android CLI (`android info`),
then `$ANDROID_HOME`, `$PATH` and Android Studio's default SDK;
`MOBILEAX_ADB` overrides. macOS and Linux (Windows: the release zip). Pin a
version with `MOBILEAX_VERSION=v0.5.1`. Emulator setup (Android CLI, host
GPU): [docs/emulator.md](docs/emulator.md).

Python bindings: `pip install ./bindings/python` (not on PyPI yet).

```python
from mobileax import Session
with Session() as d:
    d.tap("Continue")
    d.wait(text="Welcome", timeout="15s")
```

For other languages, drive `mobax --json` as a subprocess and parse one JSON
line per call.

### Building

```bash
cargo build --release             # mobax and mobax-tui (add -p mobax-viewer for the viewer)
./gradlew mobaxJar                # the daemon: build/mobax.jar
```

The daemon is an unsigned release APK whose `classes.dex` R8 has shrunk with
its runtime libraries; `app_process` accepts an APK on `CLASSPATH`. The SDK is
found via `local.properties` (`sdk.dir`) or `ANDROID_HOME`, and the project
imports into Android Studio for navigation.

---

Exit codes: `0` ok · `1` failure · `2` NOT_FOUND · `3` TIMEOUT · `4` AMBIGUOUS;
the full `error.code` is in `--json` output. `mobax --help` lists the verbs;
recipes are in the [Cookbook](docs/cookbook.md). Pre-1.0. MIT, see
[LICENSE](LICENSE).
