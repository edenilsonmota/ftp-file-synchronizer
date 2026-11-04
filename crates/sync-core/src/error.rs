use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Ftp(#[from] suppaftp::FtpError),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("Configuração corrompida: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Falha ao preparar TLS: {0}")]
    Tls(#[from] rustls::Error),
    #[error("Falha ao monitorar pasta: {0}")]
    Watch(#[from] notify::Error),
}

impl Error {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
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
