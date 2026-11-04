//! Cliente FTP/FTPS.

use std::collections::HashSet;
use std::fs::File;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use rustls::ClientConfig;
use rustls_platform_verifier::ConfigVerifierExt;
use suppaftp::types::FileType;
use suppaftp::{RustlsConnector, RustlsFtpStream};

use crate::{Config, Error, Result, remote_path};

const TIMEOUT: Duration = Duration::from_secs(30);

pub struct RemoteListing {
    pub current_path: String,
    pub directories: Vec<String>,
}

/// Abre uma sessão autenticada, em modo binário.
pub fn connect(config: &Config) -> Result<RustlsFtpStream> {
    let addresses: Vec<SocketAddr> = (config.host.as_str(), config.port)
        .to_socket_addrs()?
        .collect();
    let mut last_error = None;
    let mut stream = None;
    for address in addresses {
        match RustlsFtpStream::connect_timeout(address, TIMEOUT) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let mut stream = match (stream, last_error) {
        (Some(stream), _) => stream,
        (None, Some(error)) => return Err(error.into()),
        (None, None) => {
            return Err(Error::invalid(format!(
                "Host não encontrado: {}",
                config.host
            )));
        }
    };
    stream.get_ref().set_read_timeout(Some(TIMEOUT))?;
    stream.get_ref().set_write_timeout(Some(TIMEOUT))?;

    if config.secure {
        // Usa os certificados confiáveis do sistema operacional, incluindo CAs corporativas.
        let tls = ClientConfig::with_platform_verifier()?;
        stream = stream.into_secure(RustlsConnector::from(Arc::new(tls)), &config.host)?;
    }
    stream.login(&config.user, &config.password)?;
    // O padrão da biblioteca é ASCII, que converte quebras de linha e corrompe binários.
    stream.transfer_type(FileType::Binary)?;
    Ok(stream)
}

pub fn test_connection(config: &Config) -> Result<()> {
    let mut stream = connect(config)?;
    let _ = stream.quit();
    Ok(())
}

pub fn list_directories(config: &Config, requested: &str) -> Result<RemoteListing> {
    let mut stream = connect(config)?;
    if stream.cwd(remote_path::normalize(requested)).is_err() {
        stream.cwd("/")?;
    }
    let current_path = remote_path::normalize(&stream.pwd()?);
    let mut directories: Vec<String> = stream
        .list(None)?
        .iter()
        .filter_map(|line| suppaftp::list::File::from_str(line).ok())
        .filter(|entry| entry.is_directory() && entry.name() != "." && entry.name() != "..")
        .map(|entry| entry.name().to_string())
        .collect();
    directories.sort_by_key(|name| name.to_lowercase());
    let _ = stream.quit();
    Ok(RemoteListing {
        current_path,
        directories,
    })
}

/// Conexão persistente usada pela sincronização.
#[derive(Default)]
pub struct FtpClient {
    stream: Option<RustlsFtpStream>,
    /// Pastas remotas já criadas/confirmadas nesta conexão.
    known_dirs: HashSet<String>,
}

impl FtpClient {
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    pub fn connect(&mut self, config: &Config) -> Result<()> {
        self.disconnect();
        self.stream = Some(connect(config)?);
        Ok(())
    }

    pub fn disconnect(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            let _ = stream.quit();
        }
        self.known_dirs.clear();
    }

    /// Mantém a conexão viva durante períodos sem alterações.
    /// Se o servidor já a derrubou, apenas descarta; a próxima tarefa reconecta.
    pub fn keepalive(&mut self) {
        if let Some(stream) = self.stream.as_mut()
            && stream.noop().is_err()
        {
            self.stream = None;
            self.known_dirs.clear();
        }
    }

    fn stream(&mut self, config: &Config) -> Result<&mut RustlsFtpStream> {
        if self.stream.is_none() {
            self.connect(config)?;
        }
        Ok(self.stream.as_mut().expect("conexão recém-estabelecida"))
    }

    pub fn upload(&mut self, config: &Config, local: &Path, remote: &str) -> Result<()> {
        let parent = remote_path::parent(remote);
        self.ensure_dir(config, &parent)?;
        let stream = self.stream(config)?;
        stream.cwd(&parent)?;
        let mut file = File::open(local)?;
        stream.put_file(remote_path::file_name(remote), &mut file)?;
        Ok(())
    }

    fn ensure_dir(&mut self, config: &Config, remote_dir: &str) -> Result<()> {
        let mut current = String::new();
        for part in remote_dir.split('/').filter(|part| !part.is_empty()) {
            current.push('/');
            current.push_str(part);
            if self.known_dirs.contains(&current) {
                continue;
            }
            // Falha ao criar normalmente significa que a pasta já existe;
            // se não existir, o `cwd` do upload reporta o erro real.
            let _ = self.stream(config)?.mkdir(&current);
            self.known_dirs.insert(current.clone());
        }
        Ok(())
    }

    /// Remove um caminho remoto cujo tipo local já não é conhecido
    /// (o arquivo/pasta foi apagado). Ignora caminhos inexistentes.
    /// Retorna `true` se algo foi removido.
    pub fn remove(&mut self, config: &Config, remote: &str) -> Result<bool> {
        match self.stream(config)?.rm(remote) {
            Ok(()) => return Ok(true),
            Err(error) => {
                let error = Error::from(error);
                if !error.is_ftp_unavailable() {
                    return Err(error);
                }
            }
        }
        match self.remove_dir_recursive(config, remote) {
            Ok(()) => {
                self.known_dirs
                    .retain(|dir| dir != remote && !dir.starts_with(&format!("{remote}/")));
                Ok(true)
            }
            Err(error) if error.is_ftp_unavailable() => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn remove_dir_recursive(&mut self, config: &Config, remote: &str) -> Result<()> {
        let stream = self.stream(config)?;
        stream.cwd(remote)?;
        let entries: Vec<suppaftp::list::File> = stream
            .list(None)?
            .iter()
            .filter_map(|line| suppaftp::list::File::from_str(line).ok())
            .filter(|entry| entry.name() != "." && entry.name() != "..")
            .collect();
        for entry in entries {
            let child = remote_path::child(remote, entry.name());
            if entry.is_directory() {
                self.remove_dir_recursive(config, &child)?;
            } else {
                self.stream(config)?.rm(&child)?;
            }
        }
        let stream = self.stream(config)?;
        stream.cwd(remote_path::parent(remote))?;
        stream.rmdir(remote)?;
        Ok(())
    }
}
