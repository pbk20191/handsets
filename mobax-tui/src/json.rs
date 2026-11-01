// The dump parser and node accessors, from mobax-core.

pub use mobax_core::json::{parse, Value};
pub use mobax_core::selector::{bounds, children, get_str};

/// A node's integer field (`type`, `layer`, …).
pub fn get_num(node: &Value, key: &str) -> Option<i64> {
    mobax_core::json::obj_get(node, key).and_then(mobax_core::json::as_num)
}
