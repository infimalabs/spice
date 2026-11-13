//! Loopback operator surface, atomic app generations, the canvas archive, derived reads, and
//! causal thread walks.

#![forbid(unsafe_code)]

mod asset;
#[cfg(not(target_arch = "wasm32"))]
pub mod axum;
#[cfg(not(target_arch = "wasm32"))]
pub mod canvas;
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
mod roles;
pub mod speech;
#[cfg(not(target_arch = "wasm32"))]
pub mod stream;
#[cfg(not(target_arch = "wasm32"))]
pub mod thread;
pub mod view;

pub use asset::{
    APP, APP_GENERATION_ROUTE, APP_STAGE, APP_STAGE_CURRENT, APP_STAGE_GENERATIONS, App, AppAsset,
    AppGeneration, AppSource, AppSourceError, Discovery, PublishError, ServedAsset, content_type,
};
pub use config::{Config, ConfigError};
#[cfg(not(target_arch = "wasm32"))]
pub use roles::{RoleBook, RoleBookUnavailable};
pub use speech::{SpeechBackend, SpeechConfig, SpeechConfigError};
