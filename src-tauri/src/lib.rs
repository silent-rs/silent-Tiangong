#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod app;
pub mod commands;
mod embedded_server;
pub mod scheduler;
pub mod view;
pub mod workspace_tabs;

pub use app::{TiangongApp, ToolInjection};
