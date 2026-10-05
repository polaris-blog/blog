//! Polaris — a fast, lightweight, secure and extensible blog engine.
//!
//! ```text
//! HTTP → axum handlers → services → repositories → db (SQLx Any)
//!                                              ↘ SQLite / MySQL / PostgreSQL
//! ```
//!
//! Everything the server needs is in this library; `main.rs` is only the CLI.

pub mod auth;
pub mod backup;
pub mod cache;
pub mod config;
pub mod config_schema;
pub mod config_store;
pub mod db;
pub mod error;
pub mod extension;
pub mod http;
pub mod i18n;
pub mod markdown;
pub mod media;
pub mod models;
pub mod plugins;
pub mod repositories;
pub mod scheduler;
pub mod search;
pub mod services;
pub mod state;
pub mod templates;
pub mod themes;
pub mod utils;
