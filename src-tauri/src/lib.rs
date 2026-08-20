#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod app;
pub mod commands;
mod core_factory;
mod embedded_server;
mod session_ops;
mod state_ops;
pub mod view;
pub mod webview_host;
pub mod workspace_tabs;

pub use app::{TiangongApp, ToolInjection};
