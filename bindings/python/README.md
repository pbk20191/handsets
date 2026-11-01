# mobileax — Python bindings

A small, Pythonic wrapper around the [mobileAX](https://github.com/pbk20191/mobileax)
CLI (`mobax`). Drives Android devices from Python without reimplementing the
subprocess + JSON-parse + exit-code boilerplate every caller used to write
by hand.

## Install

```bash
pip install ./bindings/python   # from a checkout; not on PyPI yet
```

You also need the `mobax` binary on `$PATH`. See the project
[install instructions](https://github.com/pbk20191/mobileax#install).

## Usage

```python
from mobileax import Session

with Session() as d:                   # `mobax use` on enter, `mobax drop` on exit
    for node in d.ui():
        print(node.cls, node.text, node.coords)

    d.tap("Continue")                  # text lookup
    d.tap(540, 860)                    # raw coords
    d.fill("EditText", "you@x.com")    # atomic ACTION_SET_TEXT against the selector
    d.type("hello")                    # keystrokes to the focused field
    d.submit()
    d.wait(text="Welcome", timeout="15s")
```

Errors map to typed exceptions:

```python
from mobileax import Session, NotFound, Timeout, Ambiguous

try:
    d.tap("Submit", unique=True, timeout="5s")
except NotFound:
    ...  # exit code 2 — selector matched nothing
except Timeout:
    ...  # exit code 3 — wait budget exhausted
except Ambiguous:
    ...  # exit code 4 — --unique saw multiple matches
```

Everything else (daemon errors, bad arguments, secure-window blocks)
raises a generic `MobileAXError` whose `.code` attribute carries the
structured `ErrCode` enum value from the CLI's JSON output.

## Batching

For tight loops, `Session.batch()` opens a warm-socket context that
keeps one `mobax run -` subprocess alive across calls. Per-call process
startup (~5–10 ms each) collapses into one startup for the whole batch:

```python
with Session() as d:
    with d.batch(timeout="5s", retries=2) as b:
        for label in labels_to_press:
            b.tap(label, visible=True)
        b.wait(text="Done")
```

`batch()` covers the action verbs (`tap`, `type`, `fill`, `submit`,
`paste`, `wait`, `go`, `swipe`). Query verbs (`find`, `ui`) still spawn
per-call — they emit multiple JSON lines which would desync the batch
read loop.

## Talking to a specific device

```python
Session(serial="PIXEL6_SERIAL")
```

Multiple sessions can run side-by-side; each one shells out independently.

## Why a thin wrapper?

The CLI already does the hard work: warm daemon, push-mirrored state,
millisecond round-trips. The Python layer's job is to make that ergonomic
— context managers, typed exceptions, no manual `subprocess.run`. Future
versions may keep an `mobax run` subprocess warm and stream commands over its
stdin to amortise per-call process overhead.
