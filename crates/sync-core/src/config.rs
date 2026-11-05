//! Persistência da configuração.
//!
//! A configuração fica em `<config_dir>/ftp-file-synchronizer/config.json` e as
//! senhas no cofre do sistema (Windows Credential Manager, Keychain do macOS ou
//! Secret Service no Linux), uma por perfil. Quando o cofre não está disponível
//! — comum em distribuições Linux sem chaveiro ativo — a senha é salva no
//! próprio arquivo, sem criptografia, para não se perder entre execuções.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::SyncOptions;
use crate::ignore_rules::DEFAULT_PATTERNS;
use crate::{APP_ID, Error, Result};

const KEYRING_SERVICE: &str = "com.edenilson.ftpfilesynchronizer";
/// Conta usada até a v3.0, quando havia uma única credencial.
const LEGACY_KEYRING_ACCOUNT: &str = "ftp-password";
/// Pasta de configuração da versão Electron (nome definido por `app.setName`).
const LEGACY_DIR_NAME: &str = "FTP File Synchronizer";
const LEGACY_PLAIN_PREFIX: &str = "plain-storage:";
const LEGACY_ENCRYPTED_PREFIX: &str = "safe-storage:";
const SETTINGS_VERSION: u32 = 2;

pub const DEFAULT_PROFILE_ID: &str = "default";
pub const DEFAULT_PROFILE_NAME: &str = "Padrão";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub local: PathBuf,
    pub remote: String,
    /// Projeto desativado continua salvo, mas não é monitorado.
    pub enabled: bool,
    /// Padrões no formato `.gitignore`, relativos à pasta local.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<String>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            local: PathBuf::new(),
            remote: String::new(),
            enabled: true,
            ignore: Vec::new(),
        }
    }
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

/// Um servidor com suas credenciais e seus projetos.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub config: Config,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// Barra lateral mostrando só os ícones.
    pub sidebar_collapsed: bool,
    /// Inicia a sincronização do perfil ativo ao abrir o app.
    pub auto_start: bool,
    /// Apaga no servidor o que foi apagado localmente.
    pub mirror_deletions: bool,
    /// Padrões ignorados em todos os projetos (formato `.gitignore`).
    pub ignore: Vec<String>,
    pub active_profile: String,
    pub profiles: Vec<Profile>,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_config(Config::default())
    }
}

impl Settings {
    /// Configurações padrão com um único perfil.
    pub fn from_config(config: Config) -> Self {
        Self {
            theme: Theme::default(),
            sidebar_collapsed: false,
            auto_start: false,
            mirror_deletions: true,
            ignore: DEFAULT_PATTERNS
                .iter()
                .map(|item| item.to_string())
                .collect(),
            active_profile: DEFAULT_PROFILE_ID.to_string(),
            profiles: vec![Profile {
                id: DEFAULT_PROFILE_ID.to_string(),
                name: DEFAULT_PROFILE_NAME.to_string(),
                config,
            }],
        }
    }

    /// Garante ao menos um perfil e um perfil ativo existente.
    pub fn normalized(mut self) -> Self {
        if self.profiles.is_empty() {
            self.profiles = Self::default().profiles;
        }
        if self.profile_index(&self.active_profile).is_none() {
            self.active_profile = self.profiles[0].id.clone();
        }
        self
    }

    pub fn profile_index(&self, id: &str) -> Option<usize> {
        self.profiles.iter().position(|profile| profile.id == id)
    }

    pub fn active_index(&self) -> usize {
        self.profile_index(&self.active_profile).unwrap_or(0)
    }

    pub fn active(&self) -> &Profile {
        &self.profiles[self.active_index()]
    }

    pub fn active_mut(&mut self) -> &mut Profile {
        let index = self.active_index();
        &mut self.profiles[index]
    }

    /// Identificador ainda não usado por nenhum perfil.
    pub fn new_profile_id(&self) -> String {
        (1..)
            .map(|number| format!("perfil-{number}"))
            .find(|id| self.profile_index(id).is_none())
            .expect("sempre há um identificador livre")
    }

    pub fn sync_options(&self) -> SyncOptions {
        SyncOptions {
            mirror_deletions: self.mirror_deletions,
            ignore: self.ignore.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredSettings {
    version: u32,
    #[serde(flatten)]
    settings: Settings,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    plain_passwords: BTreeMap<String, String>,
}

/// Formato da v3.0: uma única conexão no topo do arquivo.
#[derive(Deserialize)]
struct SingleProfileConfig {
    #[serde(flatten)]
    config: Config,
    #[serde(default)]
    plain_password: Option<String>,
}

/// Onde as senhas são guardadas; abstraído para permitir testes sem cofre real.
pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> std::result::Result<Option<String>, String>;
    fn set(&self, account: &str, secret: &str) -> std::result::Result<(), String>;
    fn delete(&self, account: &str) -> std::result::Result<(), String>;
}

pub struct KeyringStore;

impl KeyringStore {
    fn entry(account: &str) -> std::result::Result<keyring::Entry, String> {
        keyring::Entry::new(KEYRING_SERVICE, account).map_err(|error| error.to_string())
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, account: &str) -> std::result::Result<Option<String>, String> {
        match Self::entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn set(&self, account: &str, secret: &str) -> std::result::Result<(), String> {
        Self::entry(account)?
            .set_password(secret)
            .map_err(|error| error.to_string())
    }

    fn delete(&self, account: &str) -> std::result::Result<(), String> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

fn secret_account(profile_id: &str) -> String {
    format!("{LEGACY_KEYRING_ACCOUNT}:{profile_id}")
}

pub struct LoadedSettings {
    pub settings: Settings,
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

    pub fn load(&self) -> Result<LoadedSettings> {
        if self.path.exists() {
            let value: Value = serde_json::from_str(&fs::read_to_string(&self.path)?)?;
            if value.get("profiles").is_some() {
                return self.load_settings(value);
            }
            return self.migrate_single_profile(value);
        }

        if let Some(legacy) = self.legacy_path.as_deref().filter(|path| path.exists())
            && let Some((config, password_imported)) = import_electron(legacy)
        {
            let mut notices = vec!["Configuração importada da versão anterior.".to_string()];
            if !password_imported {
                notices.push("A senha da versão anterior é criptografada e não pôde ser importada; informe-a novamente.".to_string());
            }
            let settings = Settings::from_config(config);
            notices.extend(self.save(&settings)?);
            return Ok(LoadedSettings { settings, notices });
        }

        Ok(LoadedSettings {
            settings: Settings::default(),
            notices: Vec::new(),
        })
    }

    fn load_settings(&self, value: Value) -> Result<LoadedSettings> {
        let stored: StoredSettings = serde_json::from_value(value)?;
        let mut settings = stored.settings.normalized();
        let mut notices = Vec::new();
        for profile in &mut settings.profiles {
            profile.config.password = match stored.plain_passwords.get(&profile.id) {
                Some(password) => password.clone(),
                None => self.read_secret(&secret_account(&profile.id), &mut notices),
            };
        }
        Ok(LoadedSettings { settings, notices })
    }

    /// Converte o arquivo da v3.0 em um perfil "Padrão" e copia a senha para a conta do perfil.
    /// A conta antiga é mantida para a v3.0 continuar funcionando caso o usuário volte a ela.
    fn migrate_single_profile(&self, value: Value) -> Result<LoadedSettings> {
        let stored: SingleProfileConfig = serde_json::from_value(value)?;
        let mut notices = Vec::new();
        let mut config = stored.config;
        config.password = match stored.plain_password {
            Some(password) => password,
            None => self.read_secret(LEGACY_KEYRING_ACCOUNT, &mut notices),
        };
        let settings = Settings::from_config(config);
        notices.extend(self.save(&settings)?);
        Ok(LoadedSettings { settings, notices })
    }

    fn read_secret(&self, account: &str, notices: &mut Vec<String>) -> String {
        self.secrets
            .get(account)
            .unwrap_or_else(|error| {
                let notice = format!("Não foi possível ler a senha do cofre do sistema: {error}");
                if !notices.contains(&notice) {
                    notices.push(notice);
                }
                None
            })
            .unwrap_or_default()
    }

    /// Salva a configuração; retorna avisos para o usuário, se houver.
    pub fn save(&self, settings: &Settings) -> Result<Vec<String>> {
        let mut notices = Vec::new();
        let mut plain_passwords = BTreeMap::new();
        let mut keyring_available = true;
        for profile in &settings.profiles {
            let password = &profile.config.password;
            if keyring_available {
                let account = secret_account(&profile.id);
                let result = if password.is_empty() {
                    self.secrets.delete(&account)
                } else {
                    self.secrets.set(&account, password)
                };
                if let Err(error) = result {
                    log::warn!("Cofre do sistema indisponível: {error}");
                    // Não insiste nos demais perfis: cada tentativa pode demorar.
                    keyring_available = false;
                }
            }
            if !keyring_available && !password.is_empty() {
                plain_passwords.insert(profile.id.clone(), password.clone());
            }
        }
        if !plain_passwords.is_empty() {
            notices.push(
                "Cofre de senhas do sistema indisponível; a senha foi salva sem criptografia."
                    .to_string(),
            );
        }

        let stored = StoredSettings {
            version: SETTINGS_VERSION,
            settings: settings.clone(),
            plain_passwords,
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

    /// Apaga a senha de um perfil excluído.
    pub fn delete_secret(&self, profile_id: &str) {
        let _ = self.secrets.delete(&secret_account(profile_id));
    }
}

/// Lê o `config.json` do electron-store. Retorna a configuração e se a senha veio junto.
fn import_electron(path: &Path) -> Option<(Config, bool)> {
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
                        ..Project::default()
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
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemorySecrets {
        values: Mutex<HashMap<String, String>>,
        unavailable: bool,
    }

    impl MemorySecrets {
        fn with(account: &str, secret: &str) -> Self {
            let secrets = Self::default();
            secrets
                .values
                .lock()
                .unwrap()
                .insert(account.into(), secret.into());
            secrets
        }
    }

    impl SecretStore for MemorySecrets {
        fn get(&self, account: &str) -> std::result::Result<Option<String>, String> {
            if self.unavailable {
                return Err("sem cofre".into());
            }
            Ok(self.values.lock().unwrap().get(account).cloned())
        }
        fn set(&self, account: &str, secret: &str) -> std::result::Result<(), String> {
            if self.unavailable {
                return Err("sem cofre".into());
            }
            self.values
                .lock()
                .unwrap()
                .insert(account.into(), secret.into());
            Ok(())
        }
        fn delete(&self, account: &str) -> std::result::Result<(), String> {
            if self.unavailable {
                return Err("sem cofre".into());
            }
            self.values.lock().unwrap().remove(account);
            Ok(())
        }
    }

    fn sample() -> Settings {
        let mut settings = Settings::from_config(Config {
            host: "ftp.exemplo.com".into(),
            user: "user".into(),
            password: "segredo".into(),
            port: 2121,
            secure: true,
            projects: vec![Project {
                local: PathBuf::from("/projeto"),
                remote: "/web".into(),
                enabled: false,
                ignore: vec!["*.log".into()],
            }],
        });
        settings.profiles.push(Profile {
            id: "perfil-1".into(),
            name: "Homologação".into(),
            config: Config {
                host: "ftp.homolog".into(),
                user: "outro".into(),
                password: "outra-senha".into(),
                ..Config::default()
            },
        });
        settings.active_profile = "perfil-1".into();
        settings.theme = Theme::Light;
        settings
    }

    #[test]
    fn senhas_vao_para_o_cofre_e_nao_para_o_arquivo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let store = ConfigStore::new(path.clone(), None, Box::new(MemorySecrets::default()));
        assert!(store.save(&sample()).unwrap().is_empty());
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("segredo") && !content.contains("outra-senha"));
        assert_eq!(store.load().unwrap().settings, sample());
    }

    #[test]
    fn sem_cofre_as_senhas_continuam_persistidas() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = MemorySecrets {
            unavailable: true,
            ..Default::default()
        };
        let store = ConfigStore::new(dir.path().join("config.json"), None, Box::new(secrets));
        assert_eq!(store.save(&sample()).unwrap().len(), 1);
        assert_eq!(store.load().unwrap().settings, sample());
    }

    #[test]
    fn excluir_senha_de_perfil() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(
            dir.path().join("config.json"),
            None,
            Box::new(MemorySecrets::with("ftp-password:perfil-1", "x")),
        );
        store.delete_secret("perfil-1");
        assert_eq!(store.secrets.get("ftp-password:perfil-1").unwrap(), None);
    }

    #[test]
    fn migra_configuracao_de_perfil_unico() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{"host":"ftp.x","user":"u","port":21,"secure":false,
            "projects":[{"local":"/p","remote":"/r"}]}"#,
        )
        .unwrap();
        let store = ConfigStore::new(
            path,
            None,
            Box::new(MemorySecrets::with(LEGACY_KEYRING_ACCOUNT, "abc")),
        );
        let settings = store.load().unwrap().settings;
        let profile = settings.active();
        assert_eq!(profile.name, DEFAULT_PROFILE_NAME);
        assert_eq!(profile.config.password, "abc");
        assert!(profile.config.projects[0].enabled);
        assert_eq!(
            store
                .secrets
                .get("ftp-password:default")
                .unwrap()
                .as_deref(),
            Some("abc")
        );
        assert_eq!(
            store
                .secrets
                .get(LEGACY_KEYRING_ACCOUNT)
                .unwrap()
                .as_deref(),
            Some("abc")
        );
        // A segunda leitura já usa o formato novo.
        assert_eq!(store.load().unwrap().settings, settings);
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
        let config = &loaded.settings.active().config;
        assert_eq!(config.host, "ftp.x");
        assert_eq!(config.password, "abc");
        assert_eq!(
            config.projects,
            vec![Project {
                local: "/p".into(),
                remote: "/r".into(),
                ..Project::default()
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
        assert_eq!(loaded.settings.active().config.password, "");
        assert_eq!(loaded.notices.len(), 2);
    }

    #[test]
    fn normaliza_perfil_ativo_inexistente() {
        let settings = Settings {
            active_profile: "sumiu".into(),
            ..sample()
        }
        .normalized();
        assert_eq!(settings.active_profile, DEFAULT_PROFILE_ID);
        let empty = Settings {
            profiles: Vec::new(),
            ..sample()
        }
        .normalized();
        assert_eq!(empty.profiles.len(), 1);
    }

    #[test]
    fn novo_identificador_de_perfil_e_unico() {
        let settings = sample();
        assert_eq!(settings.new_profile_id(), "perfil-2");
    }
}
