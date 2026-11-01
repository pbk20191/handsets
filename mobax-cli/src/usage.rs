// `--help` text for the `mobax` binary.
//
// Lives in its own module so main.rs can stay focused on argument parsing
// and dispatch. The 100-line USAGE constant used to dominate the top of
// main.rs and made it hard to skim the verb table next to the dispatcher
// that consumes it.

pub const USAGE: &str = "\
mobax — drive Android from the shell. Built for LLM agents and humans.

Usage: mobax [--device SERIAL] [--json] <verb> [args]

The agent loop:
  mobax use [SERIAL]              connect a device, start the daemon
  mobax ui                        flat table of tappable nodes (the LLM input format)
  mobax tap \"Continue\"             find by text, tap centre
  mobax tap #back_btn             find by resource-id, tap centre
  mobax type TEXT                 type into the focused field
  mobax wait \"Welcome\"             wait for that text (or a selector: #id)
  mobax drop                      tear the daemon down

Selectors (CSS-like, Playwright-inspired):
  mobax find 'Button[text=\"OK\"]'             attribute predicates
  mobax find 'Button:has-text(\"Sign in\")'    Playwright sugar for [text~=]
  mobax find '*EditText:below(TextView[text=Email])'
  mobax find 'Button:near(ImageView[desc~=cart], 200)'
  mobax find '[role=Switch][state=On]'       Compose role / state (device language)
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
  Capture     mobax see [PATH.jpg|.webp|.png|.xml|.json] [--size N|--native]
              (.xml/.json: focused window; --all: every window)
              mobax ui [-i|--tree|--json|--xml] [--active|--all]
                                       (app windows incl. popups; --active:
                                       focused window; --all: every window)
              mobax tui [--active]        keyboard-driven interactive inspector
                                       (--active: focused window only)
              mobax info | mobax show [top|PKG]
  Input       mobax tap X Y | mobax swipe DIR | mobax go back|home|recents|…
              mobax type TEXT                  keystrokes to focused field
              mobax fill SELECTOR TEXT         atomic ACTION_SET_TEXT
                                            SELECTOR is CSS-like (`#email`,
                                            `EditText[hint~=Mail]`), daemon
                                            grammar (`id=…`), or a free-text
                                            query that picks the best EditText
              mobax submit | mobax paste
  Lifecycle   mobax open PKG[/.Class] | mobax open \"App Name\" | mobax close PKG
              mobax install APK… | mobax uninstall PKG
  Files       mobax cp device:src dst | src device:dst
  Apps        mobax apps [--3rd] | mobax links PKG
  Data        mobax sms | mobax calls | mobax contacts | mobax calendar | mobax notif | mobax clip
              mobax location          last-known GPS fix per provider [--json]
  System      mobax prop [KEY [VAL]] | mobax settings [NS [KEY [VAL]]]
  Diagnostics mobax logs [--tail N | --follow] [--pkg PKG | --pid N] [--level E]
                      [--tag T[:L]]… [--grep RE]… [--exclude TEXT]… [-i]
                      [--since 5m] [--format brief] [--crash | --buffer B]
                      [--clear] [-- LOGCAT ARGS…]  | mobax events
  Scripting   mobax shell                 interactive REPL (also batch via stdin)
              mobax run [SCRIPT|-]        batch CLI verbs over one warm socket
              mobax init [PATH]           scaffold a starter script
              mobax act --tap … --until … one-shot tap-then-verify composite
              mobax fan SERIAL,SERIAL -- VERB    parallel per-device
              mobax mcp [--device SERIAL]  MCP server on stdio (tools for each verb)

Exit codes:
  0  ok
  1  failure (everything below not broken-out — see error.code in --json output)
  2  NOT_FOUND      no node matched
  3  TIMEOUT        wait budget exhausted
  4  AMBIGUOUS      --unique saw multiple matches

Global options:
  --host HOST        default 127.0.0.1
  --port PORT        default 9008
  --device, -s SERIAL  route to a daemon, or select the device for `mobax use`
  --json             default output to JSON (or set HS_FORMAT=json)

Low-level / debugging:
  mobax dev <sub>       ping | snapshot | screen | bench [-n N] [--json] | quit | state-daemon
  mobax do <wire>       fire one raw wire command (see docs/wire.md)
";
