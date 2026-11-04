//! Observação das pastas locais e fila de envio.
//!
//! Os eventos de sistema de arquivos variam entre Windows, Linux e macOS
//! (renomeações, eventos agrupados, tipos genéricos). Por isso a fila guarda
//! apenas *qual caminho mudou*; a ação é decidida na hora de processar,
//! olhando o estado atual do disco: arquivo existente → upload, caminho
//! inexistente → remoção remota, pasta criada/renomeada → envio do conteúdo.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use notify::event::{EventKind, ModifyKind};
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{
    DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache, new_debouncer,
};

use crate::ftp::FtpClient;
use crate::{Config, Error, Result, remote_path};

pub const MAX_PENDING_TASKS: usize = 10_000;
const RETRY_DELAYS: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_millis(500),
    Duration::from_millis(1500),
];
/// Aguarda o arquivo parar de mudar antes de enviar.
const DEBOUNCE: Duration = Duration::from_millis(800);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);
const IGNORED_NAMES: [&str; 4] = ["node_modules", ".git", ".vscode", "desktop.ini"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Success,
    Error,
}

#[derive(Clone, Debug)]
pub enum EngineEvent {
    Log(LogLevel, String),
    Running(bool),
    /// O observador de pastas falhou; a sincronização deve ser interrompida.
    WatchFailed(String),
}

pub type Listener = Arc<dyn Fn(EngineEvent) + Send + Sync>;

/// Destino remoto da sincronização; abstraído para testes.
pub trait Remote: Send {
    fn connect(&mut self, config: &Config) -> Result<()>;
    fn disconnect(&mut self);
    fn keepalive(&mut self);
    fn upload(&mut self, config: &Config, local: &Path, remote: &str) -> Result<()>;
    /// Retorna `true` se algo foi removido.
    fn remove(&mut self, config: &Config, remote: &str) -> Result<bool>;
}

impl Remote for FtpClient {
    fn connect(&mut self, config: &Config) -> Result<()> {
        FtpClient::connect(self, config)
    }
    fn disconnect(&mut self) {
        FtpClient::disconnect(self)
    }
    fn keepalive(&mut self) {
        FtpClient::keepalive(self)
    }
    fn upload(&mut self, config: &Config, local: &Path, remote: &str) -> Result<()> {
        FtpClient::upload(self, config, local, remote)
    }
    fn remove(&mut self, config: &Config, remote: &str) -> Result<bool> {
        FtpClient::remove(self, config, remote)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    /// Se o caminho for uma pasta, envia todo o conteúdo (pasta criada ou renomeada).
    pub scan_dir: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Task {
    pub project: usize,
    pub path: PathBuf,
    pub scan_dir: bool,
}

/// Fila FIFO sem duplicatas: um caminho pendente aparece uma única vez.
#[derive(Default)]
pub struct TaskQueue {
    order: VecDeque<(usize, PathBuf)>,
    pending: HashMap<(usize, PathBuf), bool>,
}

impl TaskQueue {
    /// Retorna `false` se a fila estiver cheia.
    pub fn push(&mut self, project: usize, change: Change) -> bool {
        let key = (project, change.path);
        if let Some(scan_dir) = self.pending.get_mut(&key) {
            *scan_dir |= change.scan_dir;
            return true;
        }
        if self.order.len() >= MAX_PENDING_TASKS {
            return false;
        }
        self.order.push_back(key.clone());
        self.pending.insert(key, change.scan_dir);
        true
    }

    pub fn pop(&mut self) -> Option<Task> {
        let key = self.order.pop_front()?;
        let scan_dir = self.pending.remove(&key).unwrap_or(false);
        Some(Task {
            project: key.0,
            path: key.1,
            scan_dir,
        })
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

enum Message {
    Changes(usize, Vec<Change>),
    Stop,
}

struct Session {
    sender: Sender<Message>,
    stopped: Arc<AtomicBool>,
    _debouncers: Vec<Debouncer<RecommendedWatcher, RecommendedCache>>,
}

pub struct SyncEngine {
    listener: Listener,
    session: Option<Session>,
}

impl SyncEngine {
    pub fn new(listener: Listener) -> Self {
        Self {
            listener,
            session: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.session.is_some()
    }

    /// Conecta ao FTP e começa a monitorar as pastas. Bloqueia durante a conexão.
    pub fn start(&mut self, config: Config) -> Result<()> {
        self.start_with(config, Box::new(FtpClient::default()))
    }

    pub fn start_with(&mut self, config: Config, mut remote: Box<dyn Remote>) -> Result<()> {
        self.stop_silently();
        self.log(LogLevel::Info, "Iniciando serviço...");
        if config.projects.is_empty() {
            self.log(LogLevel::Error, "Nenhuma pasta configurada!");
            return Err(Error::invalid("Nenhuma pasta configurada!"));
        }
        if let Err(error) = remote.connect(&config) {
            self.log(LogLevel::Error, format!("Erro FTP: {error}"));
            return Err(error);
        }
        self.log(LogLevel::Success, "Conexão FTP estabelecida!");

        let (sender, receiver) = mpsc::channel();
        let mut debouncers = Vec::with_capacity(config.projects.len());
        for (index, project) in config.projects.iter().enumerate() {
            match watch(index, &project.local, sender.clone(), self.listener.clone()) {
                Ok(debouncer) => debouncers.push(debouncer),
                Err(error) => {
                    remote.disconnect();
                    self.log(
                        LogLevel::Error,
                        format!("Erro ao monitorar {}: {error}", project.local.display()),
                    );
                    return Err(error);
                }
            }
        }

        let stopped = Arc::new(AtomicBool::new(false));
        let count = config.projects.len();
        let worker = Worker {
            config,
            remote,
            receiver,
            stopped: stopped.clone(),
            listener: self.listener.clone(),
            queue: TaskQueue::default(),
            failed: 0,
        };
        thread::Builder::new()
            .name("sync-worker".into())
            .spawn(move || worker.run())?;

        self.session = Some(Session {
            sender,
            stopped,
            _debouncers: debouncers,
        });
        (self.listener)(EngineEvent::Running(true));
        self.log(LogLevel::Info, format!("Monitorando {count} projeto(s)..."));
        Ok(())
    }

    pub fn stop(&mut self) {
        if self.stop_silently() {
            self.log(LogLevel::Info, "Serviço parado.");
        }
    }

    /// Para imediatamente. Um upload em andamento termina em segundo plano,
    /// mas nenhuma tarefa nova é iniciada.
    fn stop_silently(&mut self) -> bool {
        let Some(session) = self.session.take() else {
            return false;
        };
        session.stopped.store(true, Ordering::SeqCst);
        let _ = session.sender.send(Message::Stop);
        drop(session);
        (self.listener)(EngineEvent::Running(false));
        true
    }

    fn log(&self, level: LogLevel, message: impl Into<String>) {
        (self.listener)(EngineEvent::Log(level, message.into()));
    }
}

impl Drop for SyncEngine {
    fn drop(&mut self) {
        self.stop_silently();
    }
}

fn watch(
    project: usize,
    root: &Path,
    sender: Sender<Message>,
    listener: Listener,
) -> Result<Debouncer<RecommendedWatcher, RecommendedCache>> {
    let root_display = root.display().to_string();
    let mut debouncer =
        new_debouncer(
            DEBOUNCE,
            None,
            move |result: DebounceEventResult| match result {
                Ok(events) => {
                    let changes = events_to_changes(&events);
                    if !changes.is_empty() {
                        let _ = sender.send(Message::Changes(project, changes));
                    }
                }
                Err(errors) => {
                    let message = errors
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; ");
                    listener(EngineEvent::WatchFailed(format!(
                        "Erro ao monitorar {root_display}: {message}"
                    )));
                }
            },
        )?;
    debouncer.watch(root, RecursiveMode::Recursive)?;
    Ok(debouncer)
}

fn events_to_changes(events: &[DebouncedEvent]) -> Vec<Change> {
    let mut changes: Vec<Change> = Vec::new();
    for event in events {
        let kind = event.event.kind;
        if matches!(kind, EventKind::Access(_)) {
            continue;
        }
        let scan_dir = matches!(
            kind,
            EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_))
        );
        for path in &event.event.paths {
            if is_ignored(path) {
                continue;
            }
            match changes.iter_mut().find(|change| &change.path == path) {
                Some(change) => change.scan_dir |= scan_dir,
                None => changes.push(Change {
                    path: path.clone(),
                    scan_dir,
                }),
            }
        }
    }
    changes
}

pub fn is_ignored(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        IGNORED_NAMES
            .iter()
            .any(|ignored| name.eq_ignore_ascii_case(ignored))
    })
}

fn display_relative(relative: &Path) -> String {
    relative.to_string_lossy().replace('\\', "/")
}

/// Arquivos dentro de uma pasta (recursivo), sem seguir links simbólicos.
fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if is_ignored(Path::new(&entry.file_name())) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

struct Worker {
    config: Config,
    remote: Box<dyn Remote>,
    receiver: Receiver<Message>,
    stopped: Arc<AtomicBool>,
    listener: Listener,
    queue: TaskQueue,
    failed: usize,
}

impl Worker {
    fn run(mut self) {
        loop {
            if self.is_stopped() {
                break;
            }
            let message = if self.queue.is_empty() {
                match self.receiver.recv_timeout(KEEPALIVE_INTERVAL) {
                    Ok(message) => Some(message),
                    Err(RecvTimeoutError::Timeout) => {
                        self.remote.keepalive();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match self.receiver.try_recv() {
                    Ok(message) => Some(message),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => break,
                }
            };
            match message {
                Some(Message::Stop) => break,
                Some(Message::Changes(project, changes)) => {
                    for change in changes {
                        self.enqueue(project, change);
                    }
                    continue;
                }
                None => {}
            }

            if let Some(task) = self.queue.pop() {
                if !self.execute_with_retry(&task) {
                    self.failed += 1;
                }
                if self.queue.is_empty() && !self.is_stopped() {
                    match self.failed {
                        0 => self.log(LogLevel::Info, "Sincronismo em dia."),
                        failed => self.log(
                            LogLevel::Error,
                            format!("Fila concluída com {failed} erro(s)."),
                        ),
                    }
                    self.failed = 0;
                }
            }
        }
        self.remote.disconnect();
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn log(&self, level: LogLevel, message: impl Into<String>) {
        (self.listener)(EngineEvent::Log(level, message.into()));
    }

    fn enqueue(&mut self, project: usize, change: Change) {
        let path = change.path.clone();
        if !self.queue.push(project, change) {
            self.log(
                LogLevel::Error,
                format!("Fila cheia; alteração não enfileirada: {}", path.display()),
            );
        }
    }

    /// Retorna `false` se a tarefa falhou definitivamente.
    fn execute_with_retry(&mut self, task: &Task) -> bool {
        let mut last_error = None;
        for (attempt, delay) in RETRY_DELAYS.iter().enumerate() {
            if self.is_stopped() {
                return true;
            }
            if !delay.is_zero() {
                thread::sleep(*delay);
            }
            match self.execute(task) {
                Ok(()) => return true,
                Err(error @ Error::Invalid(_)) => {
                    self.log(LogLevel::Error, error.to_string());
                    return false;
                }
                Err(error) => {
                    self.remote.disconnect();
                    if attempt + 1 < RETRY_DELAYS.len() {
                        self.log(
                            LogLevel::Error,
                            format!("Tentativa {} falhou; reconectando: {error}", attempt + 1),
                        );
                    }
                    last_error = Some(error);
                }
            }
        }
        if !self.is_stopped()
            && let Some(error) = last_error
        {
            self.log(
                LogLevel::Error,
                format!(
                    "Erro em {} após {} tentativas: {error}",
                    task.path.display(),
                    RETRY_DELAYS.len()
                ),
            );
        }
        false
    }

    fn execute(&mut self, task: &Task) -> Result<()> {
        let project = &self.config.projects[task.project];
        let relative = task
            .path
            .strip_prefix(&project.local)
            .map_err(|_| Error::invalid("Arquivo fora da pasta local configurada."))?
            .to_path_buf();
        let shown = display_relative(&relative);
        let remote = remote_path::join(&project.remote, &relative)?;

        match fs::metadata(&task.path) {
            Ok(metadata) if metadata.is_file() => {
                self.log(LogLevel::Info, format!("[Upload] {shown}"));
                self.remote.upload(&self.config, &task.path, &remote)?;
                self.log(LogLevel::Success, format!("Sucesso: {shown}"));
            }
            Ok(metadata) if metadata.is_dir() && task.scan_dir => {
                let mut files = Vec::new();
                collect_files(&task.path, &mut files)?;
                self.log(
                    LogLevel::Info,
                    format!("[Pasta] {shown} ({} arquivo(s))", files.len()),
                );
                for file in files {
                    self.enqueue(
                        task.project,
                        Change {
                            path: file,
                            scan_dir: false,
                        },
                    );
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // Se a própria pasta do projeto sumiu (disco desmontado, rede caiu),
                // não espelha a "exclusão" de tudo no servidor.
                if !project.local.is_dir() {
                    return Err(Error::invalid(format!(
                        "Pasta local do projeto indisponível ({}); exclusões ignoradas.",
                        project.local.display()
                    )));
                }
                if relative.as_os_str().is_empty() {
                    return Ok(());
                }
                self.log(LogLevel::Info, format!("[Remover] {shown}"));
                if self.remote.remove(&self.config, &remote)? {
                    self.log(LogLevel::Success, format!("Removido: {shown}"));
                }
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Project;
    use std::sync::Mutex;
    use std::time::Instant;

    fn change(path: &str, scan_dir: bool) -> Change {
        Change {
            path: PathBuf::from(path),
            scan_dir,
        }
    }

    #[test]
    fn fila_nao_duplica_caminho_pendente() {
        let mut queue = TaskQueue::default();
        queue.push(0, change("/p/a.js", false));
        queue.push(0, change("/p/a.js", true));
        queue.push(1, change("/p/a.js", false));
        assert_eq!(queue.len(), 2);
        assert_eq!(
            queue.pop(),
            Some(Task {
                project: 0,
                path: "/p/a.js".into(),
                scan_dir: true
            })
        );
        assert_eq!(queue.pop().unwrap().project, 1);
    }

    #[test]
    fn alteracao_durante_processamento_volta_para_a_fila() {
        let mut queue = TaskQueue::default();
        queue.push(0, change("/p/a.js", false));
        let _processing = queue.pop();
        queue.push(0, change("/p/a.js", false));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn fila_tem_limite() {
        let mut queue = TaskQueue::default();
        for index in 0..MAX_PENDING_TASKS {
            assert!(queue.push(0, change(&format!("/p/{index}"), false)));
        }
        assert!(!queue.push(0, change("/p/extra", false)));
    }

    #[test]
    fn ignora_pastas_de_ferramentas() {
        assert!(is_ignored(Path::new("/p/node_modules/x/index.js")));
        assert!(is_ignored(Path::new("/p/.git/HEAD")));
        assert!(is_ignored(Path::new("/p/Desktop.ini")));
        assert!(!is_ignored(Path::new("/p/.gitignore")));
        assert!(!is_ignored(Path::new("/p/src/git.js")));
    }

    /// Registra as operações remotas em memória.
    #[derive(Clone, Default)]
    struct FakeRemote {
        operations: Arc<Mutex<Vec<String>>>,
    }

    impl Remote for FakeRemote {
        fn connect(&mut self, _: &Config) -> Result<()> {
            Ok(())
        }
        fn disconnect(&mut self) {}
        fn keepalive(&mut self) {}
        fn upload(&mut self, _: &Config, _: &Path, remote: &str) -> Result<()> {
            self.operations
                .lock()
                .unwrap()
                .push(format!("upload {remote}"));
            Ok(())
        }
        fn remove(&mut self, _: &Config, remote: &str) -> Result<bool> {
            self.operations
                .lock()
                .unwrap()
                .push(format!("remove {remote}"));
            Ok(true)
        }
    }

    fn wait_for(operations: &Arc<Mutex<Vec<String>>>, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if operations
                .lock()
                .unwrap()
                .iter()
                .any(|operation| operation == expected)
            {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "operação {expected:?} não ocorreu; registradas: {:?}",
            operations.lock().unwrap()
        );
    }

    /// Usa o observador real do sistema operacional; roda nos três sistemas na CI.
    #[test]
    fn sincroniza_criacao_alteracao_pasta_e_exclusao() {
        let dir = tempfile::tempdir().unwrap();
        // Em macOS a pasta temporária é um link (/var → /private/var); os eventos vêm no caminho real.
        let root = dir.path().canonicalize().unwrap();
        let remote = FakeRemote::default();
        let operations = remote.operations.clone();
        let config = Config {
            projects: vec![Project {
                local: root.clone(),
                remote: "/site".into(),
            }],
            ..Config::default()
        };
        let mut engine = SyncEngine::new(Arc::new(|_| {}));
        engine.start_with(config, Box::new(remote)).unwrap();
        thread::sleep(Duration::from_millis(300));

        fs::write(root.join("index.html"), "<h1>oi</h1>").unwrap();
        wait_for(&operations, "upload /site/index.html");

        fs::create_dir_all(root.join("staging/img")).unwrap();
        fs::write(root.join("staging/img/logo.png"), [0u8, 1, 2]).unwrap();
        thread::sleep(Duration::from_millis(1200));
        fs::rename(root.join("staging"), root.join("assets")).unwrap();
        wait_for(&operations, "upload /site/assets/img/logo.png");

        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::write(root.join("node_modules/ignored.js"), "x").unwrap();

        fs::remove_file(root.join("index.html")).unwrap();
        wait_for(&operations, "remove /site/index.html");

        engine.stop();
        assert!(
            !operations
                .lock()
                .unwrap()
                .iter()
                .any(|operation| operation.contains("node_modules"))
        );
    }
}
