// Reaching a device: starting and finding its daemon, adb itself, warm
// sessions, and the host-side state mirror.

pub(crate) mod adb;
pub(crate) mod daemon;
pub(crate) mod session;
pub(crate) mod state_cache;
