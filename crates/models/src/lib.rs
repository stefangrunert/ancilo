//! The model manager: from a model address to a running, well-configured
//! model – and everything in between (hardware, planning, downloads,
//! discovery of existing files, llama.cpp processes, RAM budget).

pub mod address;
pub mod catalog;
pub mod discovery;
pub mod download;
pub mod gguf;
pub mod hardware;
pub mod health;
pub mod hf;
pub mod llama;
pub mod manager;
pub mod ops;
pub mod planner;
pub mod quant;
pub mod resources;
pub mod routing;

pub use manager::{ManagerOptions, ModelManager, ModelStatus, ModelView};
