//! The desktop's notifications as a Yoke Plugin: the Plugin is the session's notification server.
//!
//! `store` keeps the notifications and knows nothing of D-Bus or Yoke, `backend` is the server on the
//! session's bus, and `plugin` is the Plugin around them.

pub mod backend;
pub mod plugin;
pub mod store;
