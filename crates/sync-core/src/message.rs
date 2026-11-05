//! Mensagens para o usuário, independentes de idioma.
//!
//! O motor descreve o que aconteceu com um [`Message`]; a interface traduz.
//! O `Display` produz o texto em inglês, usado em logs de depuração e como
//! idioma padrão da interface.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// Texto que não é traduzido (erros do sistema operacional ou do servidor).
    Raw(String),

    // Sincronização
    Starting,
    NoProjects,
    FtpError {
        reason: Box<Message>,
    },
    Connected,
    WatchError {
        path: String,
        reason: Box<Message>,
    },
    Watching {
        count: usize,
    },
    Stopped,
    UpToDate,
    QueueFinishedWithErrors {
        count: usize,
    },
    QueueFull {
        path: String,
    },
    RetryingFailed {
        count: usize,
    },
    RetryFailed {
        attempt: usize,
        reason: Box<Message>,
    },
    GaveUp {
        path: String,
        attempts: usize,
        reason: Box<Message>,
    },
    FileOutsideProject,
    Uploading {
        path: String,
    },
    Uploaded {
        path: String,
    },
    FolderQueued {
        path: String,
        files: usize,
    },
    ProjectRootMissing {
        path: String,
    },
    DeletionNotMirrored {
        path: String,
    },
    Removing {
        path: String,
    },
    Removed {
        path: String,
    },

    // Configuração
    ConfigDirNotFound,
    CorruptConfig {
        reason: String,
    },
    ImportedFromPreviousVersion,
    LegacyPasswordNotImported,
    KeyringReadFailed {
        reason: String,
    },
    KeyringUnavailable,

    // Validação
    HostAndUserRequired,
    InvalidPort,
    NoValidProject,
    InvalidMapping,
    LocalFolderMissing {
        path: String,
    },
    AllProjectsDisabled,
    InvalidIgnorePattern {
        reason: String,
    },
    RemotePathWithParent,
    InvalidRemotePath,

    // Conexão
    HostNotFound {
        host: String,
    },
    TlsSetupFailed {
        reason: String,
    },
    WatchSetupFailed {
        reason: String,
    },
}

impl Message {
    pub fn boxed(self) -> Box<Self> {
        Box::new(self)
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw(text) => write!(f, "{text}"),
            Self::Starting => write!(f, "Starting service..."),
            Self::NoProjects => write!(f, "No folders configured!"),
            Self::FtpError { reason } => write!(f, "FTP error: {reason}"),
            Self::Connected => write!(f, "FTP connection established!"),
            Self::WatchError { path, reason } => write!(f, "Error watching {path}: {reason}"),
            Self::Watching { count } => {
                write!(f, "Watching {}...", plural(*count, "project", "projects"))
            }
            Self::Stopped => write!(f, "Service stopped."),
            Self::UpToDate => write!(f, "Everything is up to date."),
            Self::QueueFinishedWithErrors { count } => {
                write!(
                    f,
                    "Queue finished with {}.",
                    plural(*count, "error", "errors")
                )
            }
            Self::QueueFull { path } => write!(f, "Queue is full; change not queued: {path}"),
            Self::RetryingFailed { count } => write!(
                f,
                "Retrying {}...",
                plural(*count, "failed file", "failed files")
            ),
            Self::RetryFailed { attempt, reason } => {
                write!(f, "Attempt {attempt} failed; reconnecting: {reason}")
            }
            Self::GaveUp {
                path,
                attempts,
                reason,
            } => write!(f, "Error on {path} after {attempts} attempts: {reason}"),
            Self::FileOutsideProject => write!(f, "File is outside the project's local folder."),
            Self::Uploading { path } => write!(f, "[Upload] {path}"),
            Self::Uploaded { path } => write!(f, "Uploaded: {path}"),
            Self::FolderQueued { path, files } => {
                write!(f, "[Folder] {path} ({})", plural(*files, "file", "files"))
            }
            Self::ProjectRootMissing { path } => write!(
                f,
                "Project's local folder is unavailable ({path}); deletions ignored."
            ),
            Self::DeletionNotMirrored { path } => write!(f, "[Deletion not mirrored] {path}"),
            Self::Removing { path } => write!(f, "[Remove] {path}"),
            Self::Removed { path } => write!(f, "Removed: {path}"),
            Self::ConfigDirNotFound => write!(f, "User configuration folder not found."),
            Self::CorruptConfig { reason } => write!(f, "Corrupted configuration: {reason}"),
            Self::ImportedFromPreviousVersion => {
                write!(f, "Configuration imported from the previous version.")
            }
            Self::LegacyPasswordNotImported => write!(
                f,
                "The previous version's password is encrypted and could not be imported; please enter it again."
            ),
            Self::KeyringReadFailed { reason } => {
                write!(
                    f,
                    "Could not read the password from the system keyring: {reason}"
                )
            }
            Self::KeyringUnavailable => write!(
                f,
                "System password keyring unavailable; the password was saved without encryption."
            ),
            Self::HostAndUserRequired => write!(f, "Host and user are required."),
            Self::InvalidPort => write!(f, "Invalid FTP port."),
            Self::NoValidProject => write!(f, "Add at least one valid project."),
            Self::InvalidMapping => write!(f, "Invalid folder mapping."),
            Self::LocalFolderMissing { path } => {
                write!(f, "Local folder missing or inaccessible: {path}")
            }
            Self::AllProjectsDisabled => {
                write!(f, "All projects are paused; enable at least one.")
            }
            Self::InvalidIgnorePattern { reason } => write!(f, "Invalid ignore pattern: {reason}"),
            Self::RemotePathWithParent => write!(f, "Remote path cannot contain \"..\"."),
            Self::InvalidRemotePath => write!(f, "Invalid remote path."),
            Self::HostNotFound { host } => write!(f, "Host not found: {host}"),
            Self::TlsSetupFailed { reason } => write!(f, "Failed to set up TLS: {reason}"),
            Self::WatchSetupFailed { reason } => write!(f, "Failed to watch folder: {reason}"),
        }
    }
}
