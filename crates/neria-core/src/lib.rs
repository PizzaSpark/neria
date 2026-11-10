//! Neria core: everything that isn't UI.
//!
//! Kept free of any UI toolkit so the desktop (Slint) and a future Android
//! front-end can share it.

pub mod clip;
pub mod compress;
pub mod disk;
pub mod download;
pub mod fingerprint;
pub mod fmt;
pub mod gif;
pub mod media;
pub mod music;
pub mod paths;
pub mod probe;
pub mod proc;
pub mod queue;
pub mod report;
pub mod settings;
pub mod spotify;
pub mod tools;
pub mod update;
pub mod web;
pub mod ytmusic;
