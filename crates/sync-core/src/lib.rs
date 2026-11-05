//! Motor de sincronização do FTP File Synchronizer.
//!
//! Este crate não depende de interface gráfica: configuração, validação,
//! cliente FTP e o observador de pastas ficam aqui para serem testados
//! isoladamente em Windows, Linux e macOS.

pub mod config;
pub mod engine;
pub mod error;
pub mod ftp;
pub mod ignore_rules;
pub mod message;
pub mod remote_path;
pub mod validate;

pub use config::{Config, ConfigStore, Language, Profile, Project, Settings, Theme};
pub use engine::{EngineEvent, LogLevel, SyncEngine, SyncOptions};
pub use error::{Error, Result};
pub use message::Message;

/// Identificador usado no nome do executável, no `app_id` do Wayland,
/// no `WM_CLASS` do X11 e no nome do arquivo `.desktop`.
pub const APP_ID: &str = "ftp-file-synchronizer";
pub const APP_NAME: &str = "FTP File Synchronizer";
