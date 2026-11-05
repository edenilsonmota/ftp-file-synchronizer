use std::io;

use crate::Message;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(Message),
    #[error("{0}")]
    Ftp(#[from] suppaftp::FtpError),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("Corrupted configuration: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Failed to set up TLS: {0}")]
    Tls(#[from] rustls::Error),
    #[error("Failed to watch folder: {0}")]
    Watch(#[from] notify::Error),
}

impl Error {
    pub fn invalid(message: Message) -> Self {
        Self::Invalid(message)
    }

    /// O erro como mensagem traduzível. Detalhes vindos do sistema ou do
    /// servidor (ex.: "Connection timed out") ficam como estão.
    pub fn message(&self) -> Message {
        match self {
            Self::Invalid(message) => message.clone(),
            Self::Ftp(error) => Message::Raw(error.to_string()),
            Self::Io(error) => Message::Raw(error.to_string()),
            Self::Json(error) => Message::CorruptConfig {
                reason: error.to_string(),
            },
            Self::Tls(error) => Message::TlsSetupFailed {
                reason: error.to_string(),
            },
            Self::Watch(error) => Message::WatchSetupFailed {
                reason: error.to_string(),
            },
        }
    }

    /// Indica a resposta FTP 550 (arquivo ou pasta inexistente/indisponível).
    pub fn is_ftp_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Ftp(suppaftp::FtpError::UnexpectedResponse(response))
                if response.status == suppaftp::Status::FileUnavailable
        )
    }
}

pub type Result<T> = std::result::Result<T, Error>;
