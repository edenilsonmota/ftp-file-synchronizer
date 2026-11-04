//! Persistência da configuração.
//!
//! A configuração fica em `<config_dir>/ftp-file-synchronizer/config.json` e a
//! senha no cofre do sistema (Windows Credential Manager, Keychain do macOS ou
//! Secret Service no Linux). Quando o cofre não está disponível — comum em
//! distribuições Linux sem chaveiro ativo — a senha é salva no próprio arquivo,
//! sem criptografia, para não se perder entre execuções.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{APP_ID, Error, Result};

const KEYRING_SERVICE: &str = "com.edenilson.ftpfilesynchronizer";
const KEYRING_ACCOUNT: &str = "ftp-password";
/// Pasta de configuração da versão Electron (nome definido por `app.setName`).
const LEGACY_DIR_NAME: &str = "FTP File Synchronizer";
const LEGACY_PLAIN_PREFIX: &str = "plain-storage:";
const LEGACY_ENCRYPTED_PREFIX: &str = "safe-storage:";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub local: PathBuf,
    pub remote: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub host: String,
    pub user: String,
    #[serde(skip)]
    pub password: String,
    pub port: u16,
    /// FTPS explícito (`AUTH TLS`).
    pub secure: bool,
    pub projects: Vec<Project>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: String::new(),
            user: String::new(),
            password: String::new(),
            port: 21,
            secure: false,
            projects: Vec::new(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StoredConfig {
    #[serde(flatten)]
    config: Config,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plain_password: Option<String>,
}

/// Onde a senha é guardada; abstraído para permitir testes sem cofre real.
pub trait SecretStore: Send + Sync {
    fn get(&self) -> std::result::Result<Option<String>, String>;
    fn set(&self, secret: &str) -> std::result::Result<(), String>;
    fn delete(&self) -> std::result::Result<(), String>;
}

pub struct KeyringStore;

impl KeyringStore {
    fn entry() -> std::result::Result<keyring::Entry, String> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(|error| error.to_string())
    }
}

impl SecretStore for KeyringStore {
    fn get(&self) -> std::result::Result<Option<String>, String> {
        match Self::entry()?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn set(&self, secret: &str) -> std::result::Result<(), String> {
        Self::entry()?
            .set_password(secret)
            .map_err(|error| error.to_string())
    }

    fn delete(&self) -> std::result::Result<(), String> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

pub struct LoadedConfig {
    pub config: Config,
    /// Mensagens para o usuário (importação, senha sem criptografia etc.).
    pub notices: Vec<String>,
}

pub struct ConfigStore {
    path: PathBuf,
    legacy_path: Option<PathBuf>,
    secrets: Box<dyn SecretStore>,
}

impl ConfigStore {
    pub fn new(path: PathBuf, legacy_path: Option<PathBuf>, secrets: Box<dyn SecretStore>) -> Self {
        Self {
            path,
            legacy_path,
            secrets,
        }
    }

    pub fn open_default() -> Result<Self> {
        let base = dirs::config_dir()
            .ok_or_else(|| Error::invalid("Pasta de configuração do usuário não encontrada."))?;
        Ok(Self::new(
            base.join(APP_ID).join("config.json"),
            Some(base.join(LEGACY_DIR_NAME).join("config.json")),
            Box::new(KeyringStore),
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<LoadedConfig> {
        if self.path.exists() {
            let stored: StoredConfig = serde_json::from_str(&fs::read_to_string(&self.path)?)?;
            let mut config = stored.config;
            let mut notices = Vec::new();
            config.password = match stored.plain_password {
                Some(password) => password,
                None => self
                    .secrets
                    .get()
                    .unwrap_or_else(|error| {
                        notices.push(format!(
                            "Não foi possível ler a senha do cofre do sistema: {error}"
                        ));
                        None
                    })
                    .unwrap_or_default(),
            };
            return Ok(LoadedConfig { config, notices });
        }

        if let Some(legacy) = self.legacy_path.as_deref().filter(|path| path.exists())
            && let Some((config, password_imported)) = import_legacy(legacy)
        {
            let mut notices = vec!["Configuração importada da versão anterior.".to_string()];
            if !password_imported {
                notices.push("A senha da versão anterior é criptografada e não pôde ser importada; informe-a novamente.".to_string());
            }
            notices.extend(self.save(&config)?);
            return Ok(LoadedConfig { config, notices });
        }

        Ok(LoadedConfig {
            config: Config::default(),
            notices: Vec::new(),
        })
    }

    /// Salva a configuração; retorna avisos para o usuário, se houver.
    pub fn save(&self, config: &Config) -> Result<Vec<String>> {
        let mut notices = Vec::new();
        let secret_result = if config.password.is_empty() {
            self.secrets.delete()
        } else {
            self.secrets.set(&config.password)
        };
        let plain_password = match secret_result {
            Ok(()) => None,
            Err(error) => {
                log::warn!("Cofre do sistema indisponível: {error}");
                notices.push(
                    "Cofre de senhas do sistema indisponível; a senha foi salva sem criptografia."
                        .to_string(),
                );
                Some(config.password.clone())
            }
        };

        let stored = StoredConfig {
            config: config.clone(),
            plain_password,
        };
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Escrita atômica: um arquivo parcial nunca substitui a configuração válida.
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_string_pretty(&stored)?)?;
        fs::rename(&temporary, &self.path)?;
        Ok(notices)
    }
}

/// Lê o `config.json` do electron-store. Retorna a configuração e se a senha veio junto.
fn import_legacy(path: &Path) -> Option<(Config, bool)> {
    let root: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    let legacy = root.get("config")?;
    let text = |key: &str| {
        legacy
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let port = match legacy.get("port") {
        Some(Value::Number(number)) => number.as_u64().and_then(|port| u16::try_from(port).ok()),
        Some(Value::String(port)) => port.trim().parse().ok(),
        _ => None,
    };
    let projects = legacy
        .get("projects")
        .and_then(Value::as_array)
        .map(|projects| {
            projects
                .iter()
                .filter_map(|project| {
                    Some(Project {
                        local: PathBuf::from(project.get("local")?.as_str()?),
                        remote: project.get("remote")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let raw_password = text("password");
    let password = if let Some(plain) = raw_password.strip_prefix(LEGACY_PLAIN_PREFIX) {
        Some(plain.to_string())
    } else if raw_password.starts_with(LEGACY_ENCRYPTED_PREFIX) {
        None
    } else {
        Some(raw_password)
    };

    let config = Config {
        host: text("host"),
        user: text("user"),
        password: password.clone().unwrap_or_default(),
        port: port.unwrap_or(21),
        secure: false,
        projects,
    };
    Some((config, password.is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemorySecrets {
        value: Mutex<Option<String>>,
        unavailable: bool,
    }

    impl SecretStore for MemorySecrets {
        fn get(&self) -> std::result::Result<Option<String>, String> {
            if self.unavailable {
                return Err("sem cofre".into());
            }
            Ok(self.value.lock().unwrap().clone())
        }
        fn set(&self, secret: &str) -> std::result::Result<(), String> {
            if self.unavailable {
                return Err("sem cofre".into());
            }
            *self.value.lock().unwrap() = Some(secret.to_string());
            Ok(())
        }
        fn delete(&self) -> std::result::Result<(), String> {
            if self.unavailable {
                return Err("sem cofre".into());
            }
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }

    fn sample() -> Config {
        Config {
            host: "ftp.exemplo.com".into(),
            user: "user".into(),
            password: "segredo".into(),
            port: 2121,
            secure: true,
            projects: vec![Project {
                local: PathBuf::from("/projeto"),
                remote: "/web".into(),
            }],
        }
    }

    #[test]
    fn senha_vai_para_o_cofre_e_nao_para_o_arquivo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let store = ConfigStore::new(path.clone(), None, Box::new(MemorySecrets::default()));
        assert!(store.save(&sample()).unwrap().is_empty());
        assert!(!fs::read_to_string(&path).unwrap().contains("segredo"));
        assert_eq!(store.load().unwrap().config, sample());
    }

    #[test]
    fn sem_cofre_a_senha_continua_persistida() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = MemorySecrets {
            unavailable: true,
            ..Default::default()
        };
        let store = ConfigStore::new(dir.path().join("config.json"), None, Box::new(secrets));
        assert_eq!(store.save(&sample()).unwrap().len(), 1);
        assert_eq!(store.load().unwrap().config.password, "segredo");
    }

    #[test]
    fn importa_configuracao_da_versao_electron() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("legacy.json");
        fs::write(
            &legacy,
            r#"{"config":{"host":"ftp.x","user":"u","port":"21","password":"plain-storage:abc",
            "projects":[{"local":"/p","remote":"/r"}]}}"#,
        )
        .unwrap();
        let store = ConfigStore::new(
            dir.path().join("new/config.json"),
            Some(legacy),
            Box::new(MemorySecrets::default()),
        );
        let loaded = store.load().unwrap();
        assert_eq!(loaded.config.host, "ftp.x");
        assert_eq!(loaded.config.password, "abc");
        assert_eq!(
            loaded.config.projects,
            vec![Project {
                local: "/p".into(),
                remote: "/r".into()
            }]
        );
        assert!(store.path().exists());
    }

    #[test]
    fn senha_criptografada_da_versao_electron_nao_e_importada() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("legacy.json");
        fs::write(&legacy, r#"{"config":{"host":"ftp.x","user":"u","port":21,"password":"safe-storage:AAAA","projects":[]}}"#).unwrap();
        let store = ConfigStore::new(
            dir.path().join("config.json"),
            Some(legacy),
            Box::new(MemorySecrets::default()),
        );
        let loaded = store.load().unwrap();
        assert_eq!(loaded.config.password, "");
        assert_eq!(loaded.notices.len(), 2);
    }
}
