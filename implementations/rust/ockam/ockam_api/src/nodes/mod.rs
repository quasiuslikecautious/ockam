pub mod config;
pub mod registry;

pub mod service;

pub(crate) mod connection;
pub mod models;

/// A const address to bind and send messages to
pub const NODEMANAGER_ADDR: &str = "_internal.nodemanager";

/// The main node-manager service running on remote nodes
pub use service::{IdentityOverride, NodeManager, NodeManagerWorker};
