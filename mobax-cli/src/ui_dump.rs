// `mobax ui` — human-readable UI tree, walked client-side from the daemon's
// compact JSON dump. Each node prints on one indented line with the most
// useful fields (class short-name, id short-name, text, content-desc,
// flags). Cluttery container nodes still appear so layout is preserved,
// but only the bits a tester needs to spot a selector show up.

use crate::json::Value;
use crate::selector;

struct Row {
    verb:   String,   // "tap" | "fill" | "-"
    class:  String,   // short class name — EditText / Button / TextView / View / ...
                      // useful for selector construction even though the verb
                      // already encodes input-vs-clickable-vs-label.
    label:  String,   // always double-quoted; pulls from text, falls back to content-desc
    id:     String,   // "#name" if the node has a resource-id, else empty
    coords: String,   // bare "cx,cy" — no @, no parens
    flags:  String,   // space-joined metadata: long / scroll / check / checked / password,
                      // then role= / state= / … (the action flag `click` is implicit in `verb`)
}

/// Flat list of "interactive or content-bearing" nodes — skips pure
/// layout containers (FrameLayout, LinearLayout, ConstraintLayout, …)
/// when they carry no text and no affordance. Each line reads almost
/// like the CLI call an agent would issue next:
///
///   tap   Button    "Continue"  #continue   540,860
///   fill  EditText  "Email"     #email      540,540
///   fill  EditText  "Password"  #password   540,640   [password]
///
/// The verb column collapses to `-` for nodes that are informational
/// only (TextView labels, headings) so the layout still aligns and the
/// agent sees the context without picking a non-actionable target.
pub(crate) fn render_interactive(roots: &[&Value]) -> String {
    let mut rows: Vec<Row> = Vec::new();
    for root in roots {
        collect_interactive(root, None, &mut rows);
    }
    // Pad only the structural columns (verb, class). They have bounded
    // vocabulary and help the eye scan down the table. Label / id /
    // coords are variable-width and render tight — padding them to the
    // widest row causes a single outlier (a 200-char label, a verbose
    // resource-id) to blow whitespace into every other line.
    let verb_w  = rows.iter().map(|r| r.verb.len()).max().unwrap_or(0);
    let class_w = rows.iter().map(|r| r.class.len()).max().unwrap_or(0);

    let mut out = String::with_capacity(8 * 1024);
    for r in rows {
        out.push_str(&format!(
            "{verb:<verb_w$}  {class:<class_w$}  {label}",
            verb  = r.verb,
            class = r.class,
            label = r.label,
        ));
        if !r.id.is_empty() {
            out.push_str("  ");
            out.push_str(&r.id);
        }
        out.push_str("  ");
        out.push_str(&r.coords);
        if !r.flags.is_empty() {
            out.push_str("  [");
            out.push_str(&r.flags);
            out.push(']');
        }
        out.push('\n');
    }
    out
}

/// `-i` shows only nodes a human can identify by reading the screen:
/// non-empty text OR content-desc. Empty interactive widgets stay in too
/// (an unfilled `EditText` / `SeekBar` etc. still matters for automation).
/// Nameless clickable / scrollable containers are dropped when a child
/// carries a label — tapping the label coords still triggers the parent's
/// onClick via bubbling. Image-only ones (grid cells) have no such child
/// and are kept.
fn is_interactive(node: &Value) -> bool {
    let text = selector::get_str(node, "text").unwrap_or("");
    let desc = selector::get_str(node, "desc").unwrap_or("");
    if !text.is_empty() || !desc.is_empty() { return true; }

    // Inherent input/widget classes that matter even when empty.
    let cls_full = selector::get_str(node, "cls").unwrap_or("");
    let simple = cls_full.rsplit('.').next().unwrap_or(cls_full);
    if matches!(simple,
        "EditText" | "Button" | "ImageButton" | "Switch" | "CheckBox"
        | "RadioButton" | "ToggleButton" | "Spinner" | "SeekBar"
        | "RatingBar" | "WebView"
        | "AutoCompleteTextView" | "MultiAutoCompleteTextView"
        | "DatePicker" | "TimePicker" | "NumberPicker") {
        return true;
    }

    // Anonymous clickable nodes (ImageView icons, View shims, clickable
    // containers) with a resource-id are still automatable — surface them
    // with the id standing in for the missing text/desc label.
    let rid = selector::get_str(node, "rid").unwrap_or("");
    let flags = selector::get_str(node, "flags").unwrap_or("");
    flags.contains('c') && (!rid.is_empty() || !has_labelled_descendant(node))
}

fn has_labelled_descendant(node: &Value) -> bool {
    selector::children(node).is_some_and(|kids| kids.iter().any(|c| {
        !selector::get_str(c, "text").unwrap_or("").is_empty()
            || !selector::get_str(c, "desc").unwrap_or("").is_empty()
            || has_labelled_descendant(c)
    }))
}

/// `slot` is the nearest ancestor with a resource-id and the index of the
/// child subtree we are in; it names anonymous cells as `#rv_cards[3]`.
fn collect_interactive(node: &Value, slot: Option<(&str, usize)>, rows: &mut Vec<Row>) {
    let cls_full  = selector::get_str(node, "cls").unwrap_or("");
    let cls_short = cls_full.rsplit('.').next().unwrap_or(cls_full);
    let id_full   = selector::get_str(node, "rid").unwrap_or("");
    let id_short  = id_full.rsplit('/').next().unwrap_or(id_full);

    // Skip nodes that aren't actually on-screen. Off-screen / negative
    // bounds usually mean the view has been scrolled out, recycled, or
    // never laid out — its centroid is unreachable (the daemon rejects
    // off-screen taps with OFF_SCREEN) so listing it is just noise. We
    // still walk children: a parent with degenerate bounds can wrap a
    // child whose own bounds are valid.
    if is_interactive(node) && has_visible_bounds(node) {
        let coords = match selector::bounds(node) {
            Some((x1, y1, x2, y2)) => format!("{},{}", (x1+x2)/2, (y1+y2)/2),
            None => "?,?".to_string(),
        };
        let flags = selector::get_str(node, "flags").unwrap_or("");
        let text  = selector::get_str(node, "text").unwrap_or("");
        let desc  = selector::get_str(node, "desc").unwrap_or("");

        // Label: prefer text, fall back to content-desc; when both are
        // empty (anonymous clickable widgets) fall through to `#id` so
        // the row still carries an identifier the user can read and
        // grep for. The caller doesn't care which attribute carried it
        // — only that it identifies the node.
        let label_src = if !text.is_empty() { text } else { desc };
        let label = if !label_src.is_empty() {
            truncate_quoted(label_src, 60)
        } else if !id_short.is_empty() {
            format!("#{id_short}")
        } else if let Some((parent, i)) = slot {
            format!("#{parent}[{i}]")
        } else {
            String::new()
        };

        // Verb the agent would call next. Input widgets get `fill`
        // (atomic ACTION_SET_TEXT against the selector); any other
        // clickable node gets `tap`; informational nodes (TextView
        // labels, headers) get `-` so the column still aligns.
        let verb = if is_input_widget(cls_short) {
            "fill"
        } else if flags.contains('c') {
            "tap"
        } else {
            "-"
        };

        // Metadata flags only. The `click` affordance is implied by the
        // verb column, so dropping it removes redundant noise.
        let mut tags = String::new();
        for (c, tag) in [
            ('L', "long"),
            ('s', "scroll"),
            ('k', "check"),
            ('K', "checked"),
            ('p', "password"),
        ] {
            if flags.contains(c) {
                if !tags.is_empty() { tags.push(' '); }
                tags.push_str(tag);
            }
        }
        push_semantics(node, ' ', &mut tags);

        // The id column is suppressed when the label already carries
        // `#id` (anonymous clickable widgets) — printing it twice in
        // the same row reads like a data-entry mistake.
        let id_field = if id_short.is_empty() || label.starts_with('#') {
            String::new()
        } else {
            format!("#{id_short}")
        };

        rows.push(Row {
            verb:   verb.into(),
            class:  cls_short.into(),
            label,
            id:     id_field,
            coords,
            flags:  tags,
        });
    }

    if let Some(kids) = selector::children(node) {
        for (i, c) in kids.iter().enumerate() {
            let slot = if id_short.is_empty() { slot } else { Some((id_short, i)) };
            collect_interactive(c, slot, rows);
        }
    }
}

/// What a toolkit says beyond the class (Compose sets most of these): the
/// role and state in the device's language, headings, errors, ranges and
/// list shape. Values with spaces are quoted so a row stays one token each.
fn push_semantics(node: &Value, sep: char, tags: &mut String) {
    let flags = selector::get_str(node, "flags").unwrap_or("");
    let mut push = |t: String| {
        if !tags.is_empty() { tags.push(sep); }
        tags.push_str(&t);
    };
    if flags.contains('H') { push("heading".into()); }
    for key in ["role", "state", "error", "range", "collection", "item"] {
        if let Some(v) = selector::get_str(node, key) {
            let v = if v.contains(' ') || v.contains(sep) { truncate_quoted(v, 40) } else { v.to_string() };
            push(format!("{key}={v}"));
        }
    }
}

/// Drop nodes whose bounds put them off-screen (any negative coordinate)
/// or collapsed to a zero-area rectangle. `is_interactive` already filters
/// on text/desc/widget-class; this is the second-half check that the row
/// the user sees is actually somewhere the screen can be tapped.
fn has_visible_bounds(node: &Value) -> bool {
    match selector::bounds(node) {
        Some((x1, y1, x2, y2)) => x1 >= 0 && y1 >= 0 && x2 > x1 && y2 > y1,
        None => false,
    }
}

/// True for the input widgets whose canonical CLI verb is `type` rather
/// than `tap` — these accept ACTION_SET_TEXT and the agent should know to
/// reach for `mobax type` rather than `mobax tap`.
fn is_input_widget(simple: &str) -> bool {
    matches!(simple,
        "EditText"
        | "AutoCompleteTextView"
        | "MultiAutoCompleteTextView")
}

pub(crate) fn render_human(roots: &[&Value]) -> String {
    let mut out = String::with_capacity(8 * 1024);
    for root in roots {
        walk(root, 0, &mut out);
    }
    out
}

fn walk(node: &Value, depth: usize, out: &mut String) {
    let cls_full = selector::get_str(node, "cls").unwrap_or("");
    let cls_short = cls_full.rsplit('.').next().unwrap_or(cls_full);
    let id_full = selector::get_str(node, "rid").unwrap_or("");
    let id_short = id_full.rsplit('/').next().unwrap_or(id_full);
    let text = selector::get_str(node, "text").unwrap_or("");
    let desc = selector::get_str(node, "desc").unwrap_or("");
    let flags = selector::get_str(node, "flags").unwrap_or("");

    let indent = "  ".repeat(depth);
    let mut line = format!("{indent}{cls_short}");
    if !id_short.is_empty() {
        line.push_str(" #");
        line.push_str(id_short);
    }
    if !text.is_empty() {
        line.push_str("  ");
        line.push_str(&truncate_quoted(text, 80));
    } else if !desc.is_empty() {
        line.push_str("  desc=");
        line.push_str(&truncate_quoted(desc, 80));
    }

    // Single-letter flag tags: c=clickable, s=scrollable, f=focusable,
    // F=focused, e=enabled (omitted, it's the default), L=long-clickable,
    // k=checkable, K=checked, p=password, S=selected, v=visible,
    // E=editable, H=heading.
    let mut tags = String::new();
    for (c, tag) in [
        ('c', "click"),
        ('L', "long"),
        ('s', "scroll"),
        ('f', "focusable"),
        ('F', "focused"),
        ('k', "check"),
        ('K', "checked"),
        ('p', "password"),
        ('S', "selected"),
        ('E', "editable"),
    ] {
        if flags.contains(c) {
            if !tags.is_empty() { tags.push(','); }
            tags.push_str(tag);
        }
    }
    push_semantics(node, ',', &mut tags);
    if let Some(a) = selector::get_str(node, "actions") {
        if !tags.is_empty() { tags.push(','); }
        tags.push_str("actions=");
        tags.push_str(&a.replace(',', "+"));
    }
    if !tags.is_empty() {
        line.push_str("  [");
        line.push_str(&tags);
        line.push(']');
    }

    if let Some((x1, y1, x2, y2)) = selector::bounds(node) {
        let cx = (x1 + x2) / 2;
        let cy = (y1 + y2) / 2;
        line.push_str(&format!("  @({cx},{cy})"));
    }

    out.push_str(&line);
    out.push('\n');

    if let Some(kids) = selector::children(node) {
        for c in kids { walk(c, depth + 1, out); }
    }
}

fn truncate_quoted(s: &str, max: usize) -> String {
    let escaped: String = s.chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| match c {
            '"'  => "\\\"".to_string(),
            '\n' => "↵".to_string(),
            '\t' => " ".to_string(),
            c    => c.to_string(),
        }).collect();
    if escaped.chars().count() > max {
        let t: String = escaped.chars().take(max).collect();
        format!("\"{t}…\"")
    } else {
        format!("\"{escaped}\"")
    }
}

/// Zero-width / formatting characters that the accessibility tree
/// sometimes emits between glyphs (notably ZWSP between CJK characters
/// on some apps). They look like padding in a monospace render and
/// carry no semantic value to a tap-by-label loop. Stripped from
/// rendered labels so `"返回按钮"` doesn't display as `"返​回​按​钮​"`.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}'              |  // soft hyphen
        '\u{200B}'..='\u{200F}' |  // zero-width space/joiner/non-joiner + LTR/RTL marks
        '\u{2028}'..='\u{202E}' |  // line/para separator, bidi controls
        '\u{2060}'..='\u{206F}' |  // word joiner, invisible math/comma/plus
        '\u{FEFF}'                 // ZWNBSP / BOM
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    /// Snapshot-style assertion that the verb-led layout for the canonical
    /// login-screen example renders the way the README and cookbook quote
    /// it. Catches regressions in column widths and the per-node verb logic.
    #[test]
    fn renders_verb_led_layout_for_login_screen() {
        let dump = json::parse(r#"{
            "root": {
                "cls": "android.widget.LinearLayout",
                "rid": "",
                "text": "",
                "desc": "",
                "flags": "",
                "bounds": [0, 0, 1080, 1920],
                "children": [
                    {
                        "cls": "android.widget.EditText",
                        "rid": "com.foo:id/email",
                        "text": "",
                        "desc": "Email",
                        "flags": "ce",
                        "bounds": [0, 500, 1080, 580],
                        "children": []
                    },
                    {
                        "cls": "android.widget.EditText",
                        "rid": "com.foo:id/password",
                        "text": "",
                        "desc": "Password",
                        "flags": "cep",
                        "bounds": [0, 600, 1080, 680],
                        "children": []
                    },
                    {
                        "cls": "android.widget.Button",
                        "rid": "com.foo:id/continue",
                        "text": "Continue",
                        "desc": "",
                        "flags": "ce",
                        "bounds": [0, 820, 1080, 900],
                        "children": []
                    }
                ]
            }
        }"#).unwrap();
        let out = render_interactive(&json::all_roots(&dump));
        let lines: Vec<&str> = out.trim_end().lines().collect();
        assert_eq!(lines.len(), 3, "expected 3 actionable rows, got:\n{out}");

        // Each line starts with the verb the agent would issue next, and
        // the password node carries the `[password]` metadata tag.
        assert!(lines[0].starts_with("fill"), "got: {}", lines[0]);
        assert!(lines[1].starts_with("fill"), "got: {}", lines[1]);
        assert!(lines[2].starts_with("tap"),  "got: {}", lines[2]);
        assert!(lines[1].ends_with("[password]"), "expected trailing [password] tag, got: {}", lines[1]);

        // Labels render as quoted strings whether they came from text or
        // content-desc; coords are bare digits with no @/parens.
        assert!(lines[0].contains("\"Email\""),    "got: {}", lines[0]);
        assert!(lines[1].contains("\"Password\""), "got: {}", lines[1]);
        assert!(lines[2].contains("\"Continue\""), "got: {}", lines[2]);
        for ln in &lines {
            assert!(!ln.contains('@'), "coords should be bare, no @: {ln}");
            assert!(!ln.contains("(540"), "coords should be bare, no parens: {ln}");
        }

        // Resource IDs surface as `#shortname` (Reitz/CSS convention).
        assert!(lines[0].contains("#email"));
        assert!(lines[2].contains("#continue"));
    }

    /// Clickable ImageView icons (and other anonymous tappable widgets)
    /// have no text/desc but still need to surface in the agent's table —
    /// otherwise SSO buttons and the like silently disappear from `mobax ui`.
    /// They render with `#id` standing in for the label, and the id column
    /// is suppressed so the id doesn't print twice on the same row.
    #[test]
    fn anonymous_clickable_image_view_renders_with_id_as_label() {
        let dump = json::parse(r#"{
            "root": {
                "cls": "android.widget.LinearLayout",
                "rid": "",
                "text": "",
                "desc": "",
                "flags": "",
                "bounds": [0, 0, 1080, 1920],
                "children": [
                    {
                        "cls": "android.widget.ImageView",
                        "rid": "com.foo:id/back_btn",
                        "text": "",
                        "desc": "",
                        "flags": "ce",
                        "bounds": [40, 200, 160, 320],
                        "children": []
                    },
                    {
                        "cls": "android.widget.ImageView",
                        "rid": "com.foo:id/decorative",
                        "text": "",
                        "desc": "",
                        "flags": "e",
                        "bounds": [0, 400, 100, 500],
                        "children": []
                    }
                ]
            }
        }"#).unwrap();
        let out = render_interactive(&json::all_roots(&dump));
        let lines: Vec<&str> = out.trim_end().lines().collect();

        // Only the clickable icon surfaces — non-clickable anonymous
        // ImageViews stay dropped (they were never automatable).
        assert_eq!(lines.len(), 1, "expected exactly 1 row, got:\n{out}");
        assert!(lines[0].starts_with("tap"), "got: {}", lines[0]);
        assert!(lines[0].contains("ImageView"), "got: {}", lines[0]);
        assert!(lines[0].contains("#back_btn"), "got: {}", lines[0]);
        // `#back_btn` should appear exactly once — no duplicate id column.
        assert_eq!(lines[0].matches("#back_btn").count(), 1, "got: {}", lines[0]);
    }

    /// Image-only grid cells (clickable, no text/desc/id) used to vanish,
    /// so a card picker showed nothing to tap. They are listed under the
    /// nearest id'd ancestor and their index, which `mobax tap` accepts.
    /// A nameless clickable whose child carries a label stays out: the
    /// label row already covers it.
    #[test]
    fn anonymous_cells_are_listed_by_ancestor_id_and_index() {
        let dump = json::parse(r#"{
            "root": {
                "cls": "android.widget.FrameLayout", "bounds": [0, 0, 1080, 2400],
                "children": [
                    {"cls":"android.widget.GridView","rid":"com.foo:id/rv_cards","flags":"s","bounds":[0,600,1080,1800],"children":[
                        {"cls":"android.view.ViewGroup","flags":"ce","bounds":[0,600,360,1000],"children":[
                            {"cls":"android.widget.ImageView","flags":"","bounds":[20,620,340,980],"children":[]}]},
                        {"cls":"android.view.ViewGroup","flags":"ce","bounds":[360,600,720,1000],"children":[
                            {"cls":"android.widget.ImageView","flags":"","bounds":[380,620,700,980],"children":[]}]}
                    ]},
                    {"cls":"android.widget.LinearLayout","flags":"ce","bounds":[0,1900,1080,2000],"children":[
                        {"cls":"android.widget.TextView","text":"Settings","flags":"","bounds":[40,1910,400,1990],"children":[]}]}
                ]
            }
        }"#).unwrap();
        let out = render_interactive(&json::all_roots(&dump));
        let lines: Vec<&str> = out.trim_end().lines().collect();
        assert_eq!(lines.len(), 3, "got:\n{out}");
        assert!(lines[0].starts_with("tap") && lines[0].contains("ViewGroup")
            && lines[0].contains("#rv_cards[0]") && lines[0].contains("180,800"), "got: {}", lines[0]);
        assert!(lines[1].contains("#rv_cards[1]") && lines[1].contains("540,800"), "got: {}", lines[1]);
        assert!(lines[2].contains("\"Settings\""), "got: {}", lines[2]);
    }

    #[test]
    fn compose_role_and_state_show_in_the_table_and_tree() {
        let dump = json::parse(r#"{
            "root": {
                "cls": "android.view.View", "bounds": [0, 0, 1080, 2400],
                "children": [
                    {"cls":"android.view.View","flags":"ckKfev","role":"스위치","state":"사용","bounds":[184,323,321,449],"children":[]},
                    {"cls":"android.widget.TextView","text":"설정","flags":"evH","bounds":[40,40,400,100],"children":[]},
                    {"cls":"android.widget.EditText","flags":"cLfevE","actions":"set_text,ime_enter","bounds":[42,134,1038,302],"children":[]}
                ]
            }
        }"#).unwrap();
        let table = render_interactive(&json::all_roots(&dump));
        assert!(table.contains("check checked role=스위치 state=사용"), "got:\n{table}");
        assert!(table.contains("\"설정\"") && table.contains("heading"), "got:\n{table}");
        let tree = render_human(&json::all_roots(&dump));
        assert!(tree.contains("[click,long,focusable,editable,actions=set_text+ime_enter]"), "got:\n{tree}");
        assert!(tree.contains("role=스위치,state=사용"), "got:\n{tree}");
    }

    /// Off-screen rows (negative bounds, zero-area, scrolled-out recycler
    /// cells) used to show up alongside the visible ones, with coords like
    /// "-540,-200" that no tap could ever reach. Regression: those nodes
    /// are now dropped before they hit the render.
    #[test]
    fn drops_offscreen_and_zero_area_nodes() {
        let dump = json::parse(r#"{
            "root": {
                "cls": "android.widget.FrameLayout",
                "bounds": [0, 0, 1080, 2400],
                "children": [
                    {"cls":"android.widget.Button","rid":"com.foo:id/ok","text":"OK","flags":"ce","bounds":[100,200,300,400],"children":[]},
                    {"cls":"android.widget.Button","rid":"com.foo:id/scrolled","text":"Off the top","flags":"ce","bounds":[100,-300,300,-100],"children":[]},
                    {"cls":"android.widget.Button","rid":"com.foo:id/zero","text":"Zero size","flags":"ce","bounds":[0,0,0,0],"children":[]}
                ]
            }
        }"#).unwrap();
        let out = render_interactive(&json::all_roots(&dump));
        let lines: Vec<&str> = out.trim_end().lines().collect();
        assert_eq!(lines.len(), 1, "only the OK button has visible bounds, got:\n{out}");
        assert!(lines[0].contains("\"OK\""));
        assert!(!out.contains("Off the top"));
        assert!(!out.contains("Zero size"));
    }

    /// Visual inspection helper — run with `cargo test ui_dump::tests::shows_canonical_render -- --nocapture`
    /// to see exactly what the agent gets back. Always passes; the value
    /// is in the printed output, not the assertion.
    #[test]
    fn shows_canonical_render() {
        let dump = json::parse(r#"{
            "root": {
                "cls": "android.widget.LinearLayout",
                "children": [
                    {"cls":"android.widget.EditText","rid":"com.foo:id/email","desc":"Email","flags":"ce","bounds":[0,500,1080,580],"children":[]},
                    {"cls":"android.widget.EditText","rid":"com.foo:id/password","desc":"Password","flags":"cep","bounds":[0,600,1080,680],"children":[]},
                    {"cls":"android.widget.Button","rid":"com.foo:id/continue","text":"Continue","flags":"ce","bounds":[0,820,1080,900],"children":[]}
                ]
            }
        }"#).unwrap();
        print!("\n--- mobax ui (verb-led) ---\n{}", render_interactive(&json::all_roots(&dump)));
    }
}

