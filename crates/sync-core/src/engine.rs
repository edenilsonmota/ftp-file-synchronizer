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
use crate::ignore_rules::{DEFAULT_PATTERNS, IgnoreRules};
use crate::{Config, Error, Message, Result, remote_path};

pub const MAX_PENDING_TASKS: usize = 10_000;
const RETRY_DELAYS: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_millis(500),
    Duration::from_millis(1500),
];
/// Aguarda o arquivo parar de mudar antes de enviar.
const DEBOUNCE: Duration = Duration::from_millis(800);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);

/// Preferências gerais que afetam a sincronização de todos os projetos.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncOptions {
    /// Apaga no servidor o que foi apagado localmente.
    pub mirror_deletions: bool,
    /// Padrões ignorados em todos os projetos (formato `.gitignore`).
    pub ignore: Vec<String>,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            mirror_deletions: true,
            ignore: DEFAULT_PATTERNS
                .iter()
                .map(|item| item.to_string())
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Success,
    Error,
}

#[derive(Clone, Debug)]
pub enum EngineEvent {
    Log(LogLevel, Message),
    Running(bool),
    /// O observador de pastas falhou; a sincronização deve ser interrompida.
    WatchFailed(Message),
    /// Emitido quando a fila esvazia: arquivos que ainda não foram enviados por
    /// falha (caminhos relativos ao projeto). Vazio quando tudo foi enviado.
    Pending(Vec<String>),
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

/// Comandos para a thread de envio.
enum Command {
    Changes(usize, Vec<Change>),
    /// Coloca de volta na fila os arquivos que falharam.
    RetryFailed,
    Stop,
}

struct Session {
    sender: Sender<Command>,
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
    pub fn start(&mut self, config: Config, options: SyncOptions) -> Result<()> {
        self.start_with(config, options, Box::new(FtpClient::default()))
    }

    pub fn start_with(
        &mut self,
        config: Config,
        options: SyncOptions,
        mut remote: Box<dyn Remote>,
    ) -> Result<()> {
        self.stop_silently();
        self.log(LogLevel::Info, Message::Starting);
        if config.projects.is_empty() {
            self.log(LogLevel::Error, Message::NoProjects);
            return Err(Error::invalid(Message::NoProjects));
        }
        let rules = config
            .projects
            .iter()
            .map(|project| {
                IgnoreRules::new(&project.local, &options.ignore, &project.ignore).map(Arc::new)
            })
            .collect::<Result<Vec<_>>>()
            .inspect_err(|error| self.log(LogLevel::Error, error.message()))?;
        if let Err(error) = remote.connect(&config) {
            self.log(
                LogLevel::Error,
                Message::FtpError {
                    reason: error.message().boxed(),
                },
            );
            return Err(error);
        }
        self.log(LogLevel::Success, Message::Connected);

        let (sender, receiver) = mpsc::channel();
        let mut debouncers = Vec::with_capacity(config.projects.len());
        for (index, project) in config.projects.iter().enumerate() {
            match watch(
                index,
                &project.local,
                rules[index].clone(),
                sender.clone(),
                self.listener.clone(),
            ) {
                Ok(debouncer) => debouncers.push(debouncer),
                Err(error) => {
                    remote.disconnect();
                    self.log(
                        LogLevel::Error,
                        Message::WatchError {
                            path: project.local.display().to_string(),
                            reason: error.message().boxed(),
                        },
                    );
                    return Err(error);
                }
            }
        }

        let stopped = Arc::new(AtomicBool::new(false));
        let count = config.projects.len();
        let worker = Worker {
            config,
            mirror_deletions: options.mirror_deletions,
            rules,
            remote,
            receiver,
            stopped: stopped.clone(),
            listener: self.listener.clone(),
            queue: TaskQueue::default(),
            failed: Vec::new(),
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
        self.log(LogLevel::Info, Message::Watching { count });
        Ok(())
    }

    /// Tenta de novo os arquivos que falharam. Retorna `false` se não está sincronizando.
    pub fn retry_failed(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.sender.send(Command::RetryFailed).is_ok())
    }

    pub fn stop(&mut self) {
        if self.stop_silently() {
            self.log(LogLevel::Info, Message::Stopped);
        }
    }

    /// Para imediatamente. Um upload em andamento termina em segundo plano,
    /// mas nenhuma tarefa nova é iniciada.
    fn stop_silently(&mut self) -> bool {
        let Some(session) = self.session.take() else {
            return false;
        };
        session.stopped.store(true, Ordering::SeqCst);
        let _ = session.sender.send(Command::Stop);
        drop(session);
        (self.listener)(EngineEvent::Running(false));
        true
    }

    fn log(&self, level: LogLevel, message: Message) {
        (self.listener)(EngineEvent::Log(level, message));
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
    rules: Arc<IgnoreRules>,
    sender: Sender<Command>,
    listener: Listener,
) -> Result<Debouncer<RecommendedWatcher, RecommendedCache>> {
    let root_display = root.display().to_string();
    let root_path = root.to_path_buf();
    let mut debouncer =
        new_debouncer(
            DEBOUNCE,
            None,
            move |result: DebounceEventResult| match result {
                Ok(events) => {
                    let changes = events_to_changes(&events, &root_path, &rules);
                    if !changes.is_empty() {
                        let _ = sender.send(Command::Changes(project, changes));
                    }
                }
                Err(errors) => {
                    let message = errors
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; ");
                    listener(EngineEvent::WatchFailed(Message::WatchError {
                        path: root_display.clone(),
                        reason: Message::Raw(message).boxed(),
                    }));
                }
            },
        )?;
    debouncer.watch(root, RecursiveMode::Recursive)?;
    Ok(debouncer)
}

fn events_to_changes(events: &[DebouncedEvent], root: &Path, rules: &IgnoreRules) -> Vec<Change> {
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
            if rules.is_ignored(root, path) {
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

fn display_relative(relative: &Path) -> String {
    relative.to_string_lossy().replace('\\', "/")
}

/// Arquivos dentro de uma pasta (recursivo), sem seguir links simbólicos.
fn collect_files(
    dir: &Path,
    root: &Path,
    rules: &IgnoreRules,
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if rules.is_ignored(root, &path) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&path, root, rules, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

struct Worker {
    config: Config,
    mirror_deletions: bool,
    /// Regras de cada projeto, na mesma ordem de `config.projects`.
    rules: Vec<Arc<IgnoreRules>>,
    remote: Box<dyn Remote>,
    receiver: Receiver<Command>,
    stopped: Arc<AtomicBool>,
    listener: Listener,
    queue: TaskQueue,
    /// Arquivos que falharam e ainda não foram enviados, na ordem em que falharam.
    /// Saem da lista quando um envio posterior dá certo.
    failed: Vec<(usize, PathBuf)>,
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
                Some(Command::Stop) => break,
                Some(Command::Changes(project, changes)) => {
                    for change in changes {
                        self.enqueue(project, change);
                    }
                    continue;
                }
                Some(Command::RetryFailed) => {
                    self.retry_failed();
                    continue;
                }
                None => {}
            }

            if let Some(task) = self.queue.pop() {
                let key = (task.project, task.path.clone());
                if self.execute_with_retry(&task) {
                    self.failed.retain(|failed| *failed != key);
                } else if !self.failed.contains(&key) {
                    self.failed.push(key);
                }
                if self.queue.is_empty() && !self.is_stopped() {
                    self.report_idle();
                }
            }
        }
        self.remote.disconnect();
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Fila vazia: informa se está tudo enviado ou o que ficou pendente.
    fn report_idle(&self) {
        match self.failed.len() {
            0 => self.log(LogLevel::Info, Message::UpToDate),
            count => self.log(LogLevel::Error, Message::QueueFinishedWithErrors { count }),
        }
        let pending = self
            .failed
            .iter()
            .map(|(project, path)| {
                let root = &self.config.projects[*project].local;
                display_relative(path.strip_prefix(root).unwrap_or(path))
            })
            .collect();
        (self.listener)(EngineEvent::Pending(pending));
    }

    fn retry_failed(&mut self) {
        if self.failed.is_empty() {
            return;
        }
        self.log(
            LogLevel::Info,
            Message::RetryingFailed {
                count: self.failed.len(),
            },
        );
        // Ficam na lista até darem certo; pastas voltam a ter o conteúdo enviado.
        for (project, path) in self.failed.clone() {
            self.enqueue(
                project,
                Change {
                    path,
                    scan_dir: true,
                },
            );
        }
    }

    fn log(&self, level: LogLevel, message: Message) {
        (self.listener)(EngineEvent::Log(level, message));
    }

    fn enqueue(&mut self, project: usize, change: Change) {
        let path = change.path.clone();
        if !self.queue.push(project, change) {
            self.log(
                LogLevel::Error,
                Message::QueueFull {
                    path: path.display().to_string(),
                },
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
                    self.log(LogLevel::Error, error.message());
                    return false;
                }
                Err(error) => {
                    self.remote.disconnect();
                    if attempt + 1 < RETRY_DELAYS.len() {
                        self.log(
                            LogLevel::Error,
                            Message::RetryFailed {
                                attempt: attempt + 1,
                                reason: error.message().boxed(),
                            },
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
                Message::GaveUp {
                    path: task.path.display().to_string(),
                    attempts: RETRY_DELAYS.len(),
                    reason: error.message().boxed(),
                },
            );
        }
        false
    }

    fn execute(&mut self, task: &Task) -> Result<()> {
        let project = &self.config.projects[task.project];
        let rules = &self.rules[task.project];
        if rules.is_ignored(&project.local, &task.path) {
            return Ok(());
        }
        let relative = task
            .path
            .strip_prefix(&project.local)
            .map_err(|_| Error::invalid(Message::FileOutsideProject))?
            .to_path_buf();
        let shown = display_relative(&relative);
        let remote = remote_path::join(&project.remote, &relative)?;

        match fs::metadata(&task.path) {
            Ok(metadata) if metadata.is_file() => {
                self.log(
                    LogLevel::Info,
                    Message::Uploading {
                        path: shown.clone(),
                    },
                );
                self.remote.upload(&self.config, &task.path, &remote)?;
                self.log(LogLevel::Success, Message::Uploaded { path: shown });
            }
            Ok(metadata) if metadata.is_dir() && task.scan_dir => {
                let mut files = Vec::new();
                collect_files(&task.path, &project.local, rules, &mut files)?;
                self.log(
                    LogLevel::Info,
                    Message::FolderQueued {
                        path: shown,
                        files: files.len(),
                    },
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
                    return Err(Error::invalid(Message::ProjectRootMissing {
                        path: project.local.display().to_string(),
                    }));
                }
                if relative.as_os_str().is_empty() {
                    return Ok(());
                }
                if !self.mirror_deletions {
                    self.log(LogLevel::Info, Message::DeletionNotMirrored { path: shown });
                    return Ok(());
                }
                self.log(
                    LogLevel::Info,
                    Message::Removing {
                        path: shown.clone(),
                    },
                );
                if self.remote.remove(&self.config, &remote)? {
                    self.log(LogLevel::Success, Message::Removed { path: shown });
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

    /// Registra as operações remotas em memória.
    #[derive(Clone, Default)]
    struct FakeRemote {
        operations: Arc<Mutex<Vec<String>>>,
        /// Simula o servidor fora do ar: todo envio falha.
        offline: Arc<AtomicBool>,
    }

    impl Remote for FakeRemote {
        fn connect(&mut self, _: &Config) -> Result<()> {
            Ok(())
        }
        fn disconnect(&mut self) {}
        fn keepalive(&mut self) {}
        fn upload(&mut self, _: &Config, _: &Path, remote: &str) -> Result<()> {
            if self.offline.load(Ordering::SeqCst) {
                return Err(io::Error::other("offline").into());
            }
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
                ..Project::default()
            }],
            ..Config::default()
        };
        let mut engine = SyncEngine::new(Arc::new(|_| {}));
        engine
            .start_with(config, SyncOptions::default(), Box::new(remote))
            .unwrap();
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

    /// Padrões do projeto e exclusões desativadas, também com o observador real.
    #[test]
    fn respeita_padroes_do_projeto_e_nao_espelha_exclusoes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("existente.php"), "<?php").unwrap();
        let remote = FakeRemote::default();
        let operations = remote.operations.clone();
        let config = Config {
            projects: vec![Project {
                local: root.clone(),
                remote: "/site".into(),
                ignore: vec!["*.log".into(), "cache/".into()],
                ..Project::default()
            }],
            ..Config::default()
        };
        let options = SyncOptions {
            mirror_deletions: false,
            ..SyncOptions::default()
        };
        let mut engine = SyncEngine::new(Arc::new(|_| {}));
        engine
            .start_with(config, options, Box::new(remote))
            .unwrap();
        thread::sleep(Duration::from_millis(300));

        fs::write(root.join("erro.log"), "x").unwrap();
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::write(root.join("cache/a.php"), "x").unwrap();
        fs::remove_file(root.join("existente.php")).unwrap();
        // Arquivo enviado por último: quando ele chega, os anteriores já foram processados.
        thread::sleep(Duration::from_millis(1200));
        fs::write(root.join("index.php"), "<?php").unwrap();
        wait_for(&operations, "upload /site/index.php");

        engine.stop();
        assert_eq!(
            *operations.lock().unwrap(),
            vec!["upload /site/index.php".to_string()]
        );
    }

    /// Espera até a condição ser verdadeira (no máximo 20 s).
    fn wait_until(description: &str, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !condition() {
            assert!(Instant::now() < deadline, "tempo esgotado: {description}");
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn arquivos_que_falharam_podem_ser_reenviados() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let remote = FakeRemote::default();
        let operations = remote.operations.clone();
        let offline = remote.offline.clone();
        offline.store(true, Ordering::SeqCst);
        let reports: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let listener = {
            let reports = reports.clone();
            Arc::new(move |event| {
                if let EngineEvent::Pending(files) = event {
                    reports.lock().unwrap().push(files);
                }
            })
        };
        let last_report = || {
            let mut files = reports.lock().unwrap().last().cloned().unwrap_or_default();
            files.sort();
            files
        };
        let config = Config {
            projects: vec![Project {
                local: root.clone(),
                remote: "/site".into(),
                ..Project::default()
            }],
            ..Config::default()
        };
        let mut engine = SyncEngine::new(listener);
        engine
            .start_with(config, SyncOptions::default(), Box::new(remote))
            .unwrap();
        thread::sleep(Duration::from_millis(300));

        // Servidor fora do ar: os dois arquivos ficam pendentes.
        fs::write(root.join("a.php"), "a").unwrap();
        fs::write(root.join("b.php"), "b").unwrap();
        wait_until("relatório com os dois pendentes", || {
            last_report() == ["a.php", "b.php"]
        });
        assert!(operations.lock().unwrap().is_empty());

        // Servidor de volta: tentar de novo envia os dois e esvazia a lista.
        offline.store(false, Ordering::SeqCst);
        assert!(engine.retry_failed());
        wait_until("relatório vazio depois de reenviar", || {
            operations.lock().unwrap().len() == 2 && last_report().is_empty()
        });
        let mut uploaded = operations.lock().unwrap().clone();
        uploaded.sort();
        assert_eq!(uploaded, ["upload /site/a.php", "upload /site/b.php"]);

        engine.stop();
        assert!(!engine.retry_failed());
    }
}
