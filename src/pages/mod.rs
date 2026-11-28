//! 应用页面。
//!
//! 目录结构与窗口一一对应：`mainwindow` 放主窗口的页面，
//! 每个页面模块再由 `main.rs` 依据侧边栏当前选中项分发。

#[path = "mainwindow/lib.rs"]
pub mod mainwindow;

pub mod lyricseditor;
