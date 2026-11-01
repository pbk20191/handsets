//! mobileAX core: what the `mobax` CLI, its MCP server and the TUI share,
//! with no dependencies beyond std.
//!
//! - [`wire`]: a connection to the on-device daemon and its framing.
//! - [`adb_server`]: the adb server's protocol, and streams to a device's
//!   daemon with no host port.
//! - [`device`]: which device a call talks to.
//! - [`json`], [`json_out`]: the dump parser and a small JSON writer.
//! - [`selector`]: CSS-like selectors over the accessibility tree.
//! - [`ui_dump`], [`xml_dump`]: the `ui` table and the XML view of a dump.
//! - [`screen`]: a screen as rows with stable `@N` refs, and what changed.
//! - [`errors`]: error codes and [`errors::Error`].

pub mod adb_server;
pub mod device;
pub mod errors;
pub mod json;
pub mod json_out;
pub mod screen;
pub mod selector;
pub mod ui_dump;
pub mod wire;
pub mod xml_dump;

pub use errors::{ErrCode, Error};
pub use wire::Conn;
