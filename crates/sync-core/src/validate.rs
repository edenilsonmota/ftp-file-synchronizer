//! Validação da configuração antes de conectar ou salvar.

use std::path::{Path, PathBuf};

use crate::{Config, Error, Message, Project, Result};

/// Valida e limpa os dados de conexão.
pub fn connection(config: &Config) -> Result<Config> {
    let host = config.host.trim().to_string();
    let user = config.user.trim().to_string();
    if host.is_empty() || user.is_empty() {
        return Err(Error::invalid(Message::HostAndUserRequired));
    }
    if config.port == 0 {
        return Err(Error::invalid(Message::InvalidPort));
    }
    Ok(Config {
        host,
        user,
        ..config.clone()
    })
}

/// Valida a configuração usada pela sincronização.
/// O resultado contém apenas os projetos ativos.
pub fn config(config: &Config) -> Result<Config> {
    let mut validated = connection(config)?;
    if config.projects.is_empty() {
        return Err(Error::invalid(Message::NoValidProject));
    }
    validated.projects = config
        .projects
        .iter()
        .filter(|project| project.enabled)
        .map(|project| {
            let remote = project.remote.trim().to_string();
            if project.local.as_os_str().is_empty() || remote.is_empty() {
                return Err(Error::invalid(Message::InvalidMapping));
            }
            let local = absolute(&project.local);
            if !local.is_dir() {
                return Err(Error::invalid(Message::LocalFolderMissing {
                    path: local.display().to_string(),
                }));
            }
            Ok(Project {
                local,
                remote,
                ..project.clone()
            })
        })
        .collect::<Result<_>>()?;
    if validated.projects.is_empty() {
        return Err(Error::invalid(Message::AllProjectsDisabled));
    }
    Ok(validated)
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        Config {
            host: " ftp.x ".into(),
            user: " u ".into(),
            ..Config::default()
        }
    }

    #[test]
    fn exige_host_usuario_e_porta() {
        assert!(connection(&Config::default()).is_err());
        assert!(connection(&Config { port: 0, ..base() }).is_err());
        let valid = connection(&base()).unwrap();
        assert_eq!((valid.host.as_str(), valid.user.as_str()), ("ftp.x", "u"));
    }

    #[test]
    fn exige_projetos_com_pasta_local_existente() {
        assert!(config(&base()).is_err());
        let dir = tempfile::tempdir().unwrap();
        let missing = Project {
            local: dir.path().join("nao-existe"),
            remote: "/r".into(),
            ..Project::default()
        };
        assert!(
            config(&Config {
                projects: vec![missing],
                ..base()
            })
            .is_err()
        );
        let existing = Project {
            local: dir.path().to_path_buf(),
            remote: " /r ".into(),
            ..Project::default()
        };
        let valid = config(&Config {
            projects: vec![existing],
            ..base()
        })
        .unwrap();
        assert_eq!(valid.projects[0].remote, "/r");
    }

    #[test]
    fn ignora_projetos_desativados() {
        let dir = tempfile::tempdir().unwrap();
        let active = Project {
            local: dir.path().to_path_buf(),
            remote: "/ativo".into(),
            ..Project::default()
        };
        // Desativado e com pasta inexistente: não deve impedir a sincronização.
        let disabled = Project {
            local: dir.path().join("nao-existe"),
            remote: "/desativado".into(),
            enabled: false,
            ..Project::default()
        };
        let valid = config(&Config {
            projects: vec![disabled.clone(), active],
            ..base()
        })
        .unwrap();
        assert_eq!(valid.projects.len(), 1);
        assert_eq!(valid.projects[0].remote, "/ativo");
        assert!(
            config(&Config {
                projects: vec![disabled],
                ..base()
            })
            .is_err()
        );
    }
}
