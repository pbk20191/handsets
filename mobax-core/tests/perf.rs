// Host-side cost of one screen: parse the daemon's dump, build the @N table,
// render it. Run with
//   cargo test --release -p mobax-core --test perf -- --ignored --nocapture

use std::time::{Duration, Instant};

use mobax_core::json;
use mobax_core::screen::Screen;
use mobax_core::ui_dump;

/// A list screen: `rows` clickable rows, each a container holding a title,
/// a summary and a switch, under a few layers of layout.
fn list_dump(rows: usize, shift: i64) -> String {
    let row = |i: usize| {
        let y = 200 + i as i64 * 160 + shift;
        format!(
            r#"{{"cls":"android.widget.LinearLayout","flags":"c","bounds":[0,{y},1080,{b}],"children":[
                {{"cls":"android.widget.TextView","id":"android:id/title","text":"Setting number {i}","bounds":[48,{y},800,{t}]}},
                {{"cls":"android.widget.TextView","id":"android:id/summary","text":"What setting {i} does, in a sentence","bounds":[48,{t},800,{b}]}},
                {{"cls":"android.widget.Switch","id":"android:id/switch_widget","flags":"cK","bounds":[900,{y},1032,{b}]}}]}}"#,
            b = y + 150,
            t = y + 80,
        )
    };
    let kids: Vec<String> = (0..rows).map(row).collect();
    format!(
        r#"{{"ts":1,"windows":[{{"id":7,"type":1,"layer":0,"root":
            {{"cls":"android.widget.FrameLayout","pkg":"com.android.settings","bounds":[0,0,1080,2400],"children":[
            {{"cls":"android.widget.FrameLayout","bounds":[0,0,1080,2400],"children":[
            {{"cls":"androidx.recyclerview.widget.RecyclerView","id":"com.android.settings:id/recycler_view","flags":"s","bounds":[0,180,1080,2400],"children":[{}]}}]}}]}}}}]}}"#,
        kids.join(",")
    )
}

fn time<R>(n: u32, mut f: impl FnMut() -> R) -> Duration {
    let start = Instant::now();
    for _ in 0..n {
        std::hint::black_box(f());
    }
    start.elapsed() / n
}

#[test]
#[ignore]
fn host_cost_of_one_screen() {
    for rows in [15, 60, 250] {
        let text = list_dump(rows, 0);
        let moved = list_dump(rows, -40);
        let dump = json::parse(&text).unwrap();
        let prev = Screen::from_dump(&json::parse(&moved).unwrap(), None);
        let roots = json::app_roots(&dump);
        let n = 2000;
        let parse = time(n, || json::parse(&text).unwrap());
        let first = time(n, || Screen::from_dump(&dump, None));
        let again = time(n, || Screen::from_dump(&dump, Some(&prev)));
        let change = {
            let s = Screen::from_dump(&dump, Some(&prev));
            time(n, || s.change_from(Some(&prev)))
        };
        let table = {
            let s = Screen::from_dump(&dump, None);
            time(n, || s.table())
        };
        let interactive = time(n, || ui_dump::render_interactive(&roots));
        let table_len = Screen::from_dump(&dump, None).table().len();
        println!(
            "{rows:>4} rows  dump {:>6} B -> table {:>5} B | parse {:>7.1?}  screen {:>7.1?}  vs prev {:>7.1?}  change {:>7.1?}  table {:>6.1?}  ui -i {:>7.1?}",
            text.len(), table_len, parse, first, again, change, table, interactive
        );
    }
}
