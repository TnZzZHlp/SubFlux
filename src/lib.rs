//! Core library for `subtitle-translator`.
//!
//! The terminal UI is deliberately a thin client over this library: user input
//! becomes an [`action::Action`], the application turns it into a
//! [`app::Command`], and long-running work publishes [`event::TaskEvent`]s.

pub mod action;
pub mod app;
pub mod config;
pub mod error;
pub mod event;
pub mod media;
pub mod output;
pub mod pipeline;
pub mod services;
pub mod stt;
pub mod subtitle;
pub mod translator;
pub mod tui;
