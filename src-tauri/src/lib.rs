#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod app;
pub mod browser;
pub mod commands;
pub mod scheduler;
pub mod view;

pub use app::TiangongApp;
