// `--help` text for the `hs` binary.
//
// Lives in its own module so main.rs can stay focused on argument parsing
// and dispatch. The 100-line USAGE constant used to dominate the top of
// main.rs and made it hard to skim the verb table next to the dispatcher
// that consumes it.

pub const USAGE: &str = "\
hs — drive Android from the shell. Built for LLM agents and humans.

Usage: hs [--device SERIAL] [--json] <verb> [args]

The agent loop:
  hs use [SERIAL]              connect a device, start the daemon
  hs ui                        flat table of tappable nodes (the LLM input format)
  hs tap \"Continue\"             find by text, tap centre
  hs tap #back_btn             find by resource-id, tap centre
  hs type TEXT                 type into the focused field
  hs wait \"Welcome\"             wait for that text (or a selector: #id)
  hs drop                      tear the daemon down

Selectors (CSS-like, Playwright-inspired):
  hs find 'Button[text=\"OK\"]'             attribute predicates
  hs find 'Button:has-text(\"Sign in\")'    Playwright sugar for [text~=]
  hs find '*EditText:below(TextView[text=Email])'
  hs find 'Button:near(ImageView[desc~=cart], 200)'
  hs find '[role=Switch][state=On]'       Compose role / state (device language)
  Pseudos: :visible :clickable :enabled :focused :checkable :checked
           :editable :heading :has-text(\"x\") :text-is(\"x\")
           :in(SEL) :below(SEL) :right-of(SEL) :near(SEL, PX)

Shared action flags (tap, type, find, wait, submit, paste, act):
  --timeout MS         per-call wait budget (default 10 s)
  --retries N          retry on TIMEOUT / NOT_FOUND  (with --retry-delay MS)
  --visible            require isVisibleToUser
  --clickable          require framework-clickable
  --enabled            require enabled
  --unique             fail with AMBIGUOUS if >1 match
  --nth I              pick the I-th match (1-indexed)
  --active             look in the focused window only (default: every
                       app window, so dialogs and popups are found too)
  --json               emit {\"verb\":…, \"ok\":…, \"result\"|\"error\":…} per line

More verbs:
  Capture     hs see [PATH.jpg|.webp|.png|.xml|.json] [--size N|--native]
              (.xml/.json: focused window; --all: every window)
              hs ui [-i|--tree|--json|--xml] [--active|--all]
                                       (app windows incl. popups; --active:
                                       focused window; --all: every window)
              hs tui [--active]        keyboard-driven interactive inspector
                                       (--active: focused window only)
              hs info | hs show [top|PKG]
  Input       hs tap X Y | hs swipe DIR | hs go back|home|recents|…
              hs type TEXT                  keystrokes to focused field
              hs fill SELECTOR TEXT         atomic ACTION_SET_TEXT
                                            SELECTOR is CSS-like (`#email`,
                                            `EditText[hint~=Mail]`), daemon
                                            grammar (`id=…`), or a free-text
                                            query that picks the best EditText
              hs submit | hs paste
  Lifecycle   hs open PKG[/.Class] | hs open \"App Name\" | hs close PKG
              hs install APK… | hs uninstall PKG
  Files       hs cp device:src dst | src device:dst
  Apps        hs apps [--3rd] | hs links PKG
  Data        hs sms | hs calls | hs contacts | hs calendar | hs notif | hs clip
              hs location          last-known GPS fix per provider [--json]
  System      hs prop [KEY [VAL]] | hs settings [NS [KEY [VAL]]]
  Diagnostics hs logs [--tail N | --follow] [--pkg PKG | --pid N] [--level E]
                      [--tag T[:L]]… [--grep RE]… [--exclude TEXT]… [-i]
                      [--since 5m] [--format brief] [--crash | --buffer B]
                      [--clear] [-- LOGCAT ARGS…]  | hs events
  Scripting   hs shell                 interactive REPL (also batch via stdin)
              hs run [SCRIPT|-]        batch CLI verbs over one warm socket
              hs init [PATH]           scaffold a starter script
              hs act --tap … --until … one-shot tap-then-verify composite
              hs fan SERIAL,SERIAL -- VERB    parallel per-device
              hs mcp [--device SERIAL]  MCP server on stdio (tools for each verb)

Exit codes:
  0  ok
  1  failure (everything below not broken-out — see error.code in --json output)
  2  NOT_FOUND      no node matched
  3  TIMEOUT        wait budget exhausted
  4  AMBIGUOUS      --unique saw multiple matches

Global options:
  --host HOST        default 127.0.0.1
  --port PORT        default 9008
  --device, -s SERIAL  route to a daemon, or select the device for `hs use`
  --json             default output to JSON (or set HS_FORMAT=json)

Low-level / debugging:
  hs dev <sub>       ping | snapshot | screen | bench [-n N] [--json] | quit | state-daemon
  hs do <wire>       fire one raw wire command (see docs/wire.md)
";
