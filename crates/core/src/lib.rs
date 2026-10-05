//! Core building blocks shared by every Ancilo crate.
//!
//! - [`error`]: the single error type that crosses crate and API boundaries
//! - [`event`]: the event bus every long-running activity reports through
//! - [`op`]: the operation registry – every user-visible function is defined once
//!   here and exposed by REST, CLI, MCP and the assistant alike
//! - [`paths`] / [`config`]: where Ancilo keeps its data and how it is configured

pub mod config;
pub mod error;
pub mod event;
pub mod messages;
pub mod op;
pub mod paths;
pub mod secrets;
pub mod stats;
pub mod version;

pub use config::Config;
pub use error::{Error, Result};
pub use event::{Event, EventBus, EventSink};
pub use messages::msg;
pub use op::{
    BoxFuture, NoInput, OpBuilder, OpCtx, OpSpec, Operation, Permission, Registry, Surface,
    schema_of,
};
pub use paths::Paths;

/// Product name as shown to users.
pub const PRODUCT: &str = "Ancilo";
/// Version of this build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
