// What the MCP server remembers of a device's screen between calls: the
// `ui` rows with stable `@N` refs, so an agent can act on a row by its
// number and an action's reply can say what changed instead of resending
// the whole table.
//
// A ref stays with its row while the screen stays the same (same app
// windows): a row that moves or changes state keeps its number, a new row
// gets the next unused one. A new window set (another activity, a dialog)
// is a new screen and numbering restarts at 1.

use std::collections::HashMap;

use crate::json::{self, Value};
use crate::ui_dump;

pub(crate) struct Screen {
    /// App windows, bottom first, as `id:package`; a different set is a new screen.
    key: String,
    /// Package of the topmost app window.
    pub pkg: String,
    /// The soft keyboard's window is up.
    pub keyboard: bool,
    rows: Vec<Ref>,
    next: u32,
}

pub(crate) struct Ref {
    pub n: u32,
    ident: String,
    pub line: String,
    pub x: i64,
    pub y: i64,
    /// Daemon-grammar selector for the node.
    pub sel: String,
}

/// How a screen differs from the one before it.
pub(crate) enum Change {
    /// Another screen, or so much changed that the full table is clearer.
    New,
    /// Nothing visible changed.
    Same,
    /// `+` added, `~` changed, `-` removed rows.
    Rows(Vec<String>),
}

impl Screen {
    /// The screen in `dump` (a `dump` or `dump_active` payload). Rows keep
    /// their refs from `prev` when it is the same screen.
    pub(crate) fn from_dump(dump: &Value, prev: Option<&Screen>) -> Screen {
        let (key, pkg) = identity(dump);
        let rows = ui_dump::interactive_rows(&json::app_roots(dump));
        let same = prev.filter(|p| p.key == key);
        let old: HashMap<&str, u32> = same
            .map(|p| p.rows.iter().map(|r| (r.ident.as_str(), r.n)).collect())
            .unwrap_or_default();
        let mut next = same.map_or(1, |p| p.next);
        let mut seen: HashMap<String, u32> = HashMap::new();
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            // Identical rows (three "Delete" buttons) are told apart by order.
            let k = seen.entry(r.ident.clone()).or_insert(0);
            *k += 1;
            let ident = if *k == 1 { r.ident } else { format!("{}#{k}", r.ident) };
            let n = match old.get(ident.as_str()) {
                Some(&n) => n,
                None => {
                    next += 1;
                    next - 1
                }
            };
            out.push(Ref { n, ident, line: r.line, x: r.x, y: r.y, sel: r.sel });
        }
        let keyboard = json::obj_get(dump, "windows").and_then(json::as_arr).unwrap_or(&[]).iter()
            .any(|w| json::obj_get(w, "type").and_then(json::as_num) == Some(TYPE_INPUT_METHOD));
        Screen { key, pkg, keyboard, rows: out, next }
    }

    /// A launching app's starting window: an app window whose root names
    /// no package and holds no rows yet.
    pub(crate) fn is_starting(dump: &Value) -> bool {
        let (_, pkg) = identity(dump);
        pkg.is_empty() && ui_dump::interactive_rows(&json::app_roots(dump)).is_empty()
    }

    /// Rows `ui` would list for `dump`.
    pub(crate) fn row_count(dump: &Value) -> usize {
        ui_dump::interactive_rows(&json::app_roots(dump)).len()
    }

    pub(crate) fn get(&self, n: u32) -> Option<&Ref> {
        self.rows.iter().find(|r| r.n == n)
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// The package, and whether the keyboard is up.
    pub(crate) fn title(&self) -> String {
        if self.keyboard { format!("{} · keyboard open", self.pkg) } else { self.pkg.clone() }
    }

    /// Some row shows `text` (case-insensitive).
    pub(crate) fn shows(&self, text: &str) -> bool {
        let t = text.to_lowercase();
        self.rows.iter().any(|r| r.line.to_lowercase().contains(&t))
    }

    /// What an agent reads after an action or a watch: `note`, the package,
    /// then the rows that changed, or the whole new screen.
    pub(crate) fn describe(&self, note: &str, change: Change) -> String {
        let mut out = format!("{note} · {}", self.title());
        match change {
            Change::Same => out.push_str(" · no visible change"),
            Change::Rows(lines) => {
                out.push('\n');
                out.push_str(&lines.join("\n"));
            }
            Change::New => {
                out.push_str(&format!(" · new screen, {} rows\n", self.len()));
                out.push_str(self.table().trim_end());
            }
        }
        out
    }

    /// The rows as a JSON array of `{"ref","line","x","y"}`.
    pub(crate) fn rows_json(&self) -> String {
        let mut out = String::from("[");
        for (i, r) in self.rows.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&crate::json_out::Obj::new().n("ref", r.n as i64).s("line", &r.line).n("x", r.x).n("y", r.y).finish());
        }
        out.push(']');
        out
    }

    /// The whole table, one `@N  row` per line.
    pub(crate) fn table(&self) -> String {
        let w = self.ref_width();
        let mut out = String::new();
        for r in &self.rows {
            out.push_str(&format!("{:<w$}  {}\n", format!("@{}", r.n), r.line));
        }
        out
    }

    /// What changed since `prev`.
    pub(crate) fn change_from(&self, prev: Option<&Screen>) -> Change {
        let Some(prev) = prev.filter(|p| p.key == self.key) else { return Change::New };
        let before: HashMap<&str, &Ref> = prev.rows.iter().map(|r| (r.ident.as_str(), r)).collect();
        let now: HashMap<&str, &Ref> = self.rows.iter().map(|r| (r.ident.as_str(), r)).collect();
        let w = self.ref_width().max(prev.ref_width());
        let tag = |n: u32| format!("{:<w$}", format!("@{n}"));
        let mut lines = Vec::new();
        for r in &self.rows {
            match before.get(r.ident.as_str()) {
                None => lines.push(format!("+ {}  {}", tag(r.n), r.line)),
                Some(p) if p.line != r.line => lines.push(format!("~ {}  {}", tag(r.n), r.line)),
                _ => {}
            }
        }
        for p in &prev.rows {
            if !now.contains_key(p.ident.as_str()) {
                lines.push(format!("- {}  {}", tag(p.n), p.line));
            }
        }
        if lines.is_empty() {
            Change::Same
        } else if lines.len() > (self.rows.len() / 2).max(8) {
            Change::New
        } else {
            Change::Rows(lines)
        }
    }

    fn ref_width(&self) -> usize {
        self.rows.iter().map(|r| r.n).max().unwrap_or(1).to_string().len() + 1
    }
}

/// AccessibilityWindowInfo.TYPE_INPUT_METHOD.
const TYPE_INPUT_METHOD: i64 = 2;

/// `(key, package)` of a dump: its app windows, and the topmost one's package.
fn identity(dump: &Value) -> (String, String) {
    let pkg_of = |root: &Value| json::obj_get(root, "pkg").and_then(json::as_str).unwrap_or("").to_string();
    if let Some(root) = json::obj_get(dump, "root") {
        let p = pkg_of(root);
        return (p.clone(), p);
    }
    let mut apps: Vec<&Value> = json::obj_get(dump, "windows")
        .and_then(json::as_arr)
        .unwrap_or(&[])
        .iter()
        .filter(|w| json::obj_get(w, "type").and_then(json::as_num) == Some(1))
        .collect();
    apps.sort_by_key(|w| json::obj_get(w, "layer").and_then(json::as_num).unwrap_or(0));
    let mut key = String::new();
    let mut top = String::new();
    for w in apps {
        let id = json::obj_get(w, "id").and_then(json::as_num).unwrap_or(-1);
        let pkg = json::obj_get(w, "root").map(pkg_of).unwrap_or_default();
        key.push_str(&format!("{id}:{pkg};"));
        // A closing or starting window may name no package; keep the last that does.
        if !pkg.is_empty() {
            top = pkg;
        }
    }
    (key, top)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(window_id: i64, buttons: &[(&str, i64)]) -> Value {
        let kids: Vec<String> = buttons
            .iter()
            .map(|(t, y)| format!(
                r#"{{"cls":"android.widget.Button","text":"{t}","flags":"c","bounds":[0,{y},100,{}]}}"#,
                y + 50
            ))
            .collect();
        json::parse(&format!(
            r#"{{"windows":[{{"id":{window_id},"type":1,"layer":0,"root":{{"cls":"android.widget.FrameLayout","pkg":"com.example","bounds":[0,0,1080,2400],"children":[{}]}}}}]}}"#,
            kids.join(",")
        ))
        .unwrap()
    }

    #[test]
    fn refs_number_rows_and_resolve_to_their_centre() {
        let s = Screen::from_dump(&dump(1, &[("OK", 100), ("Cancel", 200)]), None);
        assert_eq!(s.pkg, "com.example");
        assert!(s.table().starts_with("@1  tap"), "{}", s.table());
        let r = s.get(2).unwrap();
        assert!(r.line.contains("Cancel"));
        assert_eq!((r.x, r.y), (50, 225));
    }

    #[test]
    fn a_row_keeps_its_ref_when_it_moves_and_new_rows_get_new_ones() {
        let a = Screen::from_dump(&dump(1, &[("OK", 100), ("Cancel", 200)]), None);
        let b = Screen::from_dump(&dump(1, &[("Cancel", 300), ("Retry", 400)]), Some(&a));
        assert!(b.get(2).unwrap().line.contains("Cancel"), "{}", b.table());
        assert!(b.get(3).unwrap().line.contains("Retry"), "{}", b.table());
        let Change::Rows(lines) = b.change_from(Some(&a)) else { panic!("expected row changes") };
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("+ @3") && l.contains("Retry")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("~ @2") && l.contains("Cancel")), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("- @1") && l.contains("OK")), "{lines:?}");
    }

    #[test]
    fn same_rows_are_no_change_and_another_window_is_a_new_screen() {
        let a = Screen::from_dump(&dump(1, &[("OK", 100)]), None);
        let same = Screen::from_dump(&dump(1, &[("OK", 100)]), Some(&a));
        assert!(matches!(same.change_from(Some(&a)), Change::Same));
        let other = Screen::from_dump(&dump(2, &[("OK", 100)]), Some(&a));
        assert!(matches!(other.change_from(Some(&a)), Change::New));
        assert!(other.get(1).is_some(), "numbering restarts on a new screen");
    }

    #[test]
    fn a_field_keeps_its_ref_as_its_text_changes() {
        let field = |text: &str| json::parse(&format!(
            r#"{{"windows":[{{"id":1,"type":1,"layer":0,"root":{{"cls":"android.widget.FrameLayout","pkg":"com.example","bounds":[0,0,1080,2400],"children":[{{"cls":"android.widget.EditText","rid":"com.example:id/query","text":"{text}","flags":"cf","bounds":[0,100,1080,200]}}]}}}}]}}"#
        )).unwrap();
        let a = Screen::from_dump(&field("Search"), None);
        let b = Screen::from_dump(&field("battery"), Some(&a));
        let Change::Rows(lines) = b.change_from(Some(&a)) else { panic!("expected a changed row") };
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("~ @1") && lines[0].contains("battery"), "{lines:?}");
    }

    #[test]
    fn identical_rows_get_separate_refs() {
        let s = Screen::from_dump(&dump(1, &[("Delete", 100), ("Delete", 200)]), None);
        assert_eq!(s.len(), 2);
        assert_ne!(s.get(1).unwrap().y, s.get(2).unwrap().y);
    }
}
