//! Idioma da interface (português, inglês e espanhol).
//!
//! Os textos do `app.slint` são traduzidos pelo próprio Slint (`@tr` e os
//! arquivos em `translations/`). As mensagens geradas em Rust — o log do motor
//! e os avisos do app — são traduzidas aqui. O inglês vem do `Display` de
//! [`Message`]; os `match` são exaustivos, então uma mensagem nova sem tradução
//! não compila.

use std::cell::Cell;

use sync_core::{Language, Message};

/// Ordem das opções no seletor de idioma da interface.
const LANGUAGES: [Language; 3] = [Language::Pt, Language::En, Language::Es];

thread_local! {
    /// Por thread, como a seleção de tradução do Slint: toda tradução acontece
    /// na thread da interface (o log do motor é traduzido ao chegar nela).
    static CURRENT: Cell<Language> = const { Cell::new(Language::En) };
}

pub fn current() -> Language {
    CURRENT.get()
}

/// Troca o idioma do app inteiro. Precisa ser chamado depois de criar a janela.
pub fn apply(language: Language) {
    CURRENT.set(language);
    // O inglês é o idioma original dos textos; não há arquivo de tradução para ele.
    let _ = slint::select_bundled_translation(match language {
        Language::Pt => "pt",
        Language::En => "en",
        Language::Es => "es",
    });
}

pub fn index(language: Language) -> i32 {
    LANGUAGES
        .iter()
        .position(|item| *item == language)
        .unwrap_or(1) as i32
}

pub fn from_index(index: i32) -> Language {
    usize::try_from(index)
        .ok()
        .and_then(|index| LANGUAGES.get(index).copied())
        .unwrap_or(Language::En)
}

/// Idioma do sistema operacional, ou inglês se não for português nem espanhol.
pub fn detect() -> Language {
    sys_locale::get_locale()
        .map(|locale| from_locale(&locale))
        .unwrap_or(Language::En)
}

fn from_locale(locale: &str) -> Language {
    let locale = locale.to_lowercase();
    if locale.starts_with("pt") {
        Language::Pt
    } else if locale.starts_with("es") {
        Language::Es
    } else {
        Language::En
    }
}

/// Escolhe o texto do idioma atual.
fn pick(pt: impl Into<String>, en: impl Into<String>, es: impl Into<String>) -> String {
    match current() {
        Language::Pt => pt.into(),
        Language::En => en.into(),
        Language::Es => es.into(),
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// Mensagem do motor no idioma atual.
pub fn message(message: &Message) -> String {
    match current() {
        Language::En => message.to_string(),
        Language::Pt => portuguese(message),
        Language::Es => spanish(message),
    }
}

fn portuguese(message: &Message) -> String {
    use Message::*;
    match message {
        Raw(text) => text.clone(),
        Starting => "Iniciando serviço...".into(),
        NoProjects => "Nenhuma pasta configurada!".into(),
        FtpError { reason } => format!("Erro FTP: {}", portuguese(reason)),
        Connected => "Conexão FTP estabelecida!".into(),
        WatchError { path, reason } => {
            format!("Erro ao monitorar {path}: {}", portuguese(reason))
        }
        Watching { count } => format!("Monitorando {}...", plural(*count, "projeto", "projetos")),
        Stopped => "Serviço parado.".into(),
        UpToDate => "Sincronismo em dia.".into(),
        QueueFinishedWithErrors { count } => {
            format!("Fila concluída com {}.", plural(*count, "erro", "erros"))
        }
        QueueFull { path } => format!("Fila cheia; alteração não enfileirada: {path}"),
        RetryingFailed { count } => format!(
            "Tentando novamente {}...",
            plural(*count, "arquivo que falhou", "arquivos que falharam")
        ),
        RetryFailed { attempt, reason } => format!(
            "Tentativa {attempt} falhou; reconectando: {}",
            portuguese(reason)
        ),
        GaveUp {
            path,
            attempts,
            reason,
        } => format!(
            "Erro em {path} após {attempts} tentativas: {}",
            portuguese(reason)
        ),
        FileOutsideProject => "Arquivo fora da pasta local configurada.".into(),
        Uploading { path } => format!("[Upload] {path}"),
        Uploaded { path } => format!("Sucesso: {path}"),
        FolderQueued { path, files } => {
            format!("[Pasta] {path} ({})", plural(*files, "arquivo", "arquivos"))
        }
        ProjectRootMissing { path } => {
            format!("Pasta local do projeto indisponível ({path}); exclusões ignoradas.")
        }
        DeletionNotMirrored { path } => format!("[Exclusão não espelhada] {path}"),
        Removing { path } => format!("[Remover] {path}"),
        Removed { path } => format!("Removido: {path}"),
        ConfigDirNotFound => "Pasta de configuração do usuário não encontrada.".into(),
        CorruptConfig { reason } => format!("Configuração corrompida: {reason}"),
        ImportedFromPreviousVersion => "Configuração importada da versão anterior.".into(),
        LegacyPasswordNotImported => "A senha da versão anterior é criptografada e não pôde ser importada; informe-a novamente.".into(),
        KeyringReadFailed { reason } => {
            format!("Não foi possível ler a senha do cofre do sistema: {reason}")
        }
        KeyringUnavailable => {
            "Cofre de senhas do sistema indisponível; a senha foi salva sem criptografia.".into()
        }
        HostAndUserRequired => "Host e usuário são obrigatórios.".into(),
        InvalidPort => "Porta FTP inválida.".into(),
        NoValidProject => "Adicione pelo menos um projeto válido.".into(),
        InvalidMapping => "Mapeamento de pasta inválido.".into(),
        LocalFolderMissing { path } => format!("Pasta local inexistente ou inacessível: {path}"),
        AllProjectsDisabled => "Todos os projetos estão pausados; ative pelo menos um.".into(),
        InvalidIgnorePattern { reason } => format!("Padrão ignorado inválido: {reason}"),
        RemotePathWithParent => "Caminho remoto não pode conter \"..\".".into(),
        InvalidRemotePath => "Caminho remoto inválido.".into(),
        HostNotFound { host } => format!("Host não encontrado: {host}"),
        TlsSetupFailed { reason } => format!("Falha ao preparar TLS: {reason}"),
        WatchSetupFailed { reason } => format!("Falha ao monitorar pasta: {reason}"),
    }
}

fn spanish(message: &Message) -> String {
    use Message::*;
    match message {
        Raw(text) => text.clone(),
        Starting => "Iniciando servicio...".into(),
        NoProjects => "¡No hay carpetas configuradas!".into(),
        FtpError { reason } => format!("Error FTP: {}", spanish(reason)),
        Connected => "¡Conexión FTP establecida!".into(),
        WatchError { path, reason } => format!("Error al vigilar {path}: {}", spanish(reason)),
        Watching { count } => format!("Vigilando {}...", plural(*count, "proyecto", "proyectos")),
        Stopped => "Servicio detenido.".into(),
        UpToDate => "Todo sincronizado.".into(),
        QueueFinishedWithErrors { count } => {
            format!("Cola terminada con {}.", plural(*count, "error", "errores"))
        }
        QueueFull { path } => format!("Cola llena; cambio no agregado: {path}"),
        RetryingFailed { count } => format!(
            "Reintentando {}...",
            plural(*count, "archivo con error", "archivos con error")
        ),
        RetryFailed { attempt, reason } => format!(
            "El intento {attempt} falló; reconectando: {}",
            spanish(reason)
        ),
        GaveUp {
            path,
            attempts,
            reason,
        } => format!(
            "Error en {path} después de {attempts} intentos: {}",
            spanish(reason)
        ),
        FileOutsideProject => "Archivo fuera de la carpeta local del proyecto.".into(),
        Uploading { path } => format!("[Envío] {path}"),
        Uploaded { path } => format!("Enviado: {path}"),
        FolderQueued { path, files } => {
            format!("[Carpeta] {path} ({})", plural(*files, "archivo", "archivos"))
        }
        ProjectRootMissing { path } => format!(
            "Carpeta local del proyecto no disponible ({path}); eliminaciones ignoradas."
        ),
        DeletionNotMirrored { path } => format!("[Eliminación no replicada] {path}"),
        Removing { path } => format!("[Eliminar] {path}"),
        Removed { path } => format!("Eliminado: {path}"),
        ConfigDirNotFound => "No se encontró la carpeta de configuración del usuario.".into(),
        CorruptConfig { reason } => format!("Configuración dañada: {reason}"),
        ImportedFromPreviousVersion => "Configuración importada de la versión anterior.".into(),
        LegacyPasswordNotImported => "La contraseña de la versión anterior está cifrada y no se pudo importar; ingrésala de nuevo.".into(),
        KeyringReadFailed { reason } => format!(
            "No se pudo leer la contraseña del almacén de claves del sistema: {reason}"
        ),
        KeyringUnavailable => "Almacén de claves del sistema no disponible; la contraseña se guardó sin cifrar.".into(),
        HostAndUserRequired => "El host y el usuario son obligatorios.".into(),
        InvalidPort => "Puerto FTP no válido.".into(),
        NoValidProject => "Agrega al menos un proyecto válido.".into(),
        InvalidMapping => "Asignación de carpetas no válida.".into(),
        LocalFolderMissing { path } => {
            format!("La carpeta local no existe o no es accesible: {path}")
        }
        AllProjectsDisabled => "Todos los proyectos están en pausa; activa al menos uno.".into(),
        InvalidIgnorePattern { reason } => format!("Patrón ignorado no válido: {reason}"),
        RemotePathWithParent => "La ruta remota no puede contener \"..\".".into(),
        InvalidRemotePath => "Ruta remota no válida.".into(),
        HostNotFound { host } => format!("Host no encontrado: {host}"),
        TlsSetupFailed { reason } => format!("Error al preparar TLS: {reason}"),
        WatchSetupFailed { reason } => format!("Error al vigilar la carpeta: {reason}"),
    }
}

/// Textos gerados pelo próprio app (fora do `app.slint`).
pub enum Text {
    LoadSettingsFailed(Message),
    OpenSettingsFailed(Message),
    SaveSettingsFailed(Message),
    OpenFailed {
        target: String,
        reason: String,
    },
    TestingCredentials,
    CredentialsValid,
    ConnectionSucceeded,
    TestFailed(Message),
    ConnectFailed(Message),
    StartFailed(Message),
    ListFailed(Message),
    FillHostBeforeBrowsing,
    ConfirmDeleteProfile {
        name: String,
    },
    CopyOf {
        name: String,
    },
    ChooseLocalFolder,
    ProjectFallback,
    Untitled,
    NewProfile,
    DefaultProfile,
    /// Título do aviso de arquivos que falharam.
    PendingTitle {
        count: usize,
    },
    /// Os primeiros nomes e quantos faltam.
    PendingFiles {
        files: Vec<String>,
    },
}

/// Quantos nomes de arquivo aparecem no aviso antes de "e mais N".
const PENDING_NAMES: usize = 3;

pub fn text(text: Text) -> String {
    use Text::*;
    match text {
        LoadSettingsFailed(reason) => {
            let reason = message(&reason);
            pick(
                format!("Falha ao carregar configurações: {reason}"),
                format!("Failed to load settings: {reason}"),
                format!("Error al cargar la configuración: {reason}"),
            )
        }
        OpenSettingsFailed(reason) => {
            let reason = message(&reason);
            pick(
                format!("Falha ao abrir configurações: {reason}"),
                format!("Failed to open settings: {reason}"),
                format!("Error al abrir la configuración: {reason}"),
            )
        }
        SaveSettingsFailed(reason) => {
            let reason = message(&reason);
            pick(
                format!("Falha ao salvar configurações: {reason}"),
                format!("Failed to save settings: {reason}"),
                format!("Error al guardar la configuración: {reason}"),
            )
        }
        OpenFailed { target, reason } => pick(
            format!("Não foi possível abrir {target}: {reason}"),
            format!("Could not open {target}: {reason}"),
            format!("No se pudo abrir {target}: {reason}"),
        ),
        TestingCredentials => pick(
            "Testando credenciais FTP...",
            "Testing FTP credentials...",
            "Probando credenciales FTP...",
        ),
        CredentialsValid => pick(
            "Credenciais FTP válidas.",
            "FTP credentials are valid.",
            "Credenciales FTP válidas.",
        ),
        ConnectionSucceeded => pick(
            "Conexão FTP estabelecida com sucesso!",
            "FTP connection established successfully!",
            "¡Conexión FTP establecida con éxito!",
        ),
        TestFailed(reason) => {
            let reason = message(&reason);
            pick(
                format!("Falha no teste FTP: {reason}"),
                format!("FTP test failed: {reason}"),
                format!("Falló la prueba FTP: {reason}"),
            )
        }
        ConnectFailed(reason) => {
            let reason = message(&reason);
            pick(
                format!("Falha ao conectar: {reason}"),
                format!("Failed to connect: {reason}"),
                format!("Error al conectar: {reason}"),
            )
        }
        StartFailed(reason) => {
            let reason = message(&reason);
            pick(
                format!("Não foi possível iniciar a sincronização: {reason}"),
                format!("Could not start syncing: {reason}"),
                format!("No se pudo iniciar la sincronización: {reason}"),
            )
        }
        ListFailed(reason) => {
            let reason = message(&reason);
            pick(
                format!("Falha ao listar diretórios remotos: {reason}"),
                format!("Failed to list remote folders: {reason}"),
                format!("Error al listar las carpetas remotas: {reason}"),
            )
        }
        FillHostBeforeBrowsing => pick(
            "Preencha host e usuário antes de navegar no FTP remoto.",
            "Fill in host and user before browsing the remote FTP.",
            "Completa el host y el usuario antes de explorar el FTP remoto.",
        ),
        ConfirmDeleteProfile { name } => pick(
            format!(
                "Excluir o perfil \"{name}\"? A credencial e a lista de projetos dele serão apagadas."
            ),
            format!(
                "Delete the profile \"{name}\"? Its credential and project list will be removed."
            ),
            format!(
                "¿Eliminar el perfil \"{name}\"? Se borrarán su credencial y su lista de proyectos."
            ),
        ),
        CopyOf { name } => pick(
            format!("{name} (cópia)"),
            format!("{name} (copy)"),
            format!("{name} (copia)"),
        ),
        ChooseLocalFolder => pick(
            "Selecionar pasta local",
            "Choose local folder",
            "Elegir carpeta local",
        ),
        ProjectFallback => pick("projeto", "project", "proyecto"),
        Untitled => pick("Sem nome", "Untitled", "Sin nombre"),
        NewProfile => pick("Novo perfil", "New profile", "Nuevo perfil"),
        DefaultProfile => pick("Padrão", "Default", "Predeterminado"),
        PendingTitle { count } => match count {
            1 => pick(
                "1 arquivo não foi enviado",
                "1 file was not uploaded",
                "1 archivo no se envió",
            ),
            _ => pick(
                format!("{count} arquivos não foram enviados"),
                format!("{count} files were not uploaded"),
                format!("{count} archivos no se enviaron"),
            ),
        },
        PendingFiles { files } => {
            let names = files
                .iter()
                .take(PENDING_NAMES)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            match files.len().saturating_sub(PENDING_NAMES) {
                0 => names,
                more => pick(
                    format!("{names} e mais {more}"),
                    format!("{names} and {more} more"),
                    format!("{names} y {more} más"),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idioma_do_sistema() {
        assert_eq!(from_locale("pt-BR"), Language::Pt);
        assert_eq!(from_locale("pt_PT.UTF-8"), Language::Pt);
        assert_eq!(from_locale("es-AR"), Language::Es);
        assert_eq!(from_locale("en-US"), Language::En);
        assert_eq!(from_locale("de-DE"), Language::En);
    }

    #[test]
    fn indice_do_seletor() {
        for language in LANGUAGES {
            assert_eq!(from_index(index(language)), language);
        }
        assert_eq!(from_index(-1), Language::En);
        assert_eq!(from_index(9), Language::En);
    }

    #[test]
    fn aviso_de_pendentes_em_portugues_e_espanhol() {
        let files: Vec<String> = (1..=4).map(|n| format!("{n}.php")).collect();
        apply(Language::Pt);
        assert_eq!(
            text(Text::PendingTitle { count: 4 }),
            "4 arquivos não foram enviados"
        );
        assert_eq!(
            text(Text::PendingFiles {
                files: files.clone()
            }),
            "1.php, 2.php, 3.php e mais 1"
        );
        apply(Language::Es);
        assert_eq!(
            text(Text::PendingTitle { count: 1 }),
            "1 archivo no se envió"
        );
        assert_eq!(
            text(Text::PendingFiles { files }),
            "1.php, 2.php, 3.php y 1 más"
        );
    }

    #[test]
    fn traduz_mensagens_aninhadas() {
        let error = Message::GaveUp {
            path: "/p/a.php".into(),
            attempts: 3,
            reason: Message::Raw("Connection timed out".into()).boxed(),
        };
        assert_eq!(
            portuguese(&error),
            "Erro em /p/a.php após 3 tentativas: Connection timed out"
        );
        assert_eq!(
            spanish(&Message::FtpError {
                reason: Message::HostAndUserRequired.boxed()
            }),
            "Error FTP: El host y el usuario son obligatorios."
        );
        assert_eq!(
            Message::Watching { count: 1 }.to_string(),
            "Watching 1 project..."
        );
        assert_eq!(
            portuguese(&Message::Watching { count: 2 }),
            "Monitorando 2 projetos..."
        );
    }
}
