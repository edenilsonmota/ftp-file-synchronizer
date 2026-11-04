// Em release no Windows, não abre um console junto com a janela.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod single_instance;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};
use sync_core::engine::{EngineEvent, LogLevel as CoreLogLevel};
use sync_core::{Config, ConfigStore, Project, SyncEngine, ftp, remote_path, validate};

slint::include_modules!();

const MAX_LOG_ENTRIES: usize = 500;

type SharedEngine = Arc<Mutex<SyncEngine>>;

fn main() -> Result<(), slint::PlatformError> {
    if single_instance::notify_existing() {
        return Ok(());
    }
    // O app_id no Wayland (e o WM_CLASS no X11) precisa ser igual ao nome do
    // arquivo .desktop instalado; é assim que o ambiente encontra o ícone.
    // Precisa ser definido depois de escolher a plataforma e antes de criar a janela.
    slint::BackendSelector::new().select()?;
    slint::set_xdg_app_id(sync_core::APP_ID)?;

    let ui = AppWindow::new()?;
    let projects = Rc::new(VecModel::<ProjectRow>::default());
    ui.set_projects(projects.clone().into());
    ui.set_logs(Rc::new(VecModel::<LogEntry>::default()).into());

    let store = load_settings(&ui, &projects);
    let engine = create_engine(&ui);

    single_instance::listen({
        let weak = ui.as_weak();
        move || {
            let _ = weak.upgrade_in_event_loop(|ui| {
                ui.window().set_minimized(false);
                let _ = ui.show();
            });
        }
    });

    register_connection_callbacks(&ui);
    register_sync_callbacks(&ui, &engine, store.clone());
    register_project_callbacks(&ui, &projects);
    register_remote_picker(&ui, &projects);
    ui.on_dismiss_notice({
        let weak = ui.as_weak();
        move || weak.unwrap().set_notice(SharedString::new())
    });

    ui.run()?;

    // Fechar a janela encerra o app; a configuração editada não se perde.
    if let Some(store) = store {
        let _ = store.save(&collect_config(&ui));
    }
    lock(&engine).stop();
    Ok(())
}

fn lock(engine: &SharedEngine) -> MutexGuard<'_, SyncEngine> {
    engine
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn load_settings(ui: &AppWindow, projects: &VecModel<ProjectRow>) -> Option<Arc<ConfigStore>> {
    let store = match ConfigStore::open_default() {
        Ok(store) => store,
        Err(error) => {
            push_log(
                ui,
                CoreLogLevel::Error,
                format!("Falha ao abrir configurações: {error}"),
            );
            projects.push(ProjectRow::default());
            return None;
        }
    };
    match store.load() {
        Ok(loaded) => {
            let config = loaded.config;
            ui.set_host(config.host.into());
            ui.set_user(config.user.into());
            ui.set_password(config.password.into());
            ui.set_port(config.port.to_string().into());
            ui.set_secure(config.secure);
            for project in config.projects {
                projects.push(ProjectRow {
                    local: project.local.to_string_lossy().as_ref().into(),
                    remote: project.remote.into(),
                });
            }
            for notice in loaded.notices {
                push_log(ui, CoreLogLevel::Info, notice);
            }
        }
        Err(error) => push_log(
            ui,
            CoreLogLevel::Error,
            format!("Falha ao carregar configurações: {error}"),
        ),
    }
    if projects.row_count() == 0 {
        projects.push(ProjectRow::default());
    }
    Some(Arc::new(store))
}

fn create_engine(ui: &AppWindow) -> SharedEngine {
    let cell: Arc<OnceLock<SharedEngine>> = Arc::new(OnceLock::new());
    let weak = ui.as_weak();
    let engine_ref = cell.clone();
    let engine = Arc::new(Mutex::new(SyncEngine::new(Arc::new(
        move |event| match event {
            EngineEvent::Log(level, message) => {
                let _ = weak.upgrade_in_event_loop(move |ui| push_log(&ui, level, message));
            }
            EngineEvent::Running(running) => {
                let _ = weak.upgrade_in_event_loop(move |ui| ui.set_running(running));
            }
            EngineEvent::WatchFailed(message) => {
                let _ = weak
                    .upgrade_in_event_loop(move |ui| push_log(&ui, CoreLogLevel::Error, message));
                if let Some(engine) = engine_ref.get().cloned() {
                    thread::spawn(move || lock(&engine).stop());
                }
            }
        },
    ))));
    let _ = cell.set(engine.clone());
    engine
}

fn push_log(ui: &AppWindow, level: CoreLogLevel, message: String) {
    let time = chrono::Local::now().format("%H:%M:%S").to_string();
    eprintln!("[{time}] [{level:?}] {message}");
    let logs = ui.get_logs();
    let Some(logs) = logs.as_any().downcast_ref::<VecModel<LogEntry>>() else {
        return;
    };
    let level = match level {
        CoreLogLevel::Info => LogLevel::Info,
        CoreLogLevel::Success => LogLevel::Success,
        CoreLogLevel::Error => LogLevel::Error,
    };
    logs.insert(
        0,
        LogEntry {
            time: time.into(),
            message: message.into(),
            level,
        },
    );
    while logs.row_count() > MAX_LOG_ENTRIES {
        logs.remove(logs.row_count() - 1);
    }
}

fn collect_connection(ui: &AppWindow) -> Config {
    Config {
        host: ui.get_host().trim().to_string(),
        user: ui.get_user().trim().to_string(),
        password: ui.get_password().to_string(),
        port: ui.get_port().trim().parse().unwrap_or(0),
        secure: ui.get_secure(),
        projects: Vec::new(),
    }
}

fn collect_config(ui: &AppWindow) -> Config {
    let projects = ui
        .get_projects()
        .iter()
        .filter(|row| !row.local.trim().is_empty() && !row.remote.trim().is_empty())
        .map(|row| Project {
            local: PathBuf::from(row.local.trim()),
            remote: row.remote.trim().to_string(),
        })
        .collect();
    Config {
        projects,
        ..collect_connection(ui)
    }
}

fn register_connection_callbacks(ui: &AppWindow) {
    let weak = ui.as_weak();
    ui.on_test_connection(move || {
        let ui = weak.unwrap();
        let config = match validate::connection(&collect_connection(&ui)) {
            Ok(config) => config,
            Err(error) => return ui.set_notice(error.to_string().into()),
        };
        ui.set_testing(true);
        push_log(
            &ui,
            CoreLogLevel::Info,
            "Testando credenciais FTP...".into(),
        );
        let weak = weak.clone();
        thread::spawn(move || {
            let result = ftp::test_connection(&config);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_testing(false);
                match result {
                    Ok(()) => {
                        push_log(
                            &ui,
                            CoreLogLevel::Success,
                            "Credenciais FTP válidas.".into(),
                        );
                        ui.set_notice("Conexão FTP estabelecida com sucesso!".into());
                    }
                    Err(error) => {
                        push_log(
                            &ui,
                            CoreLogLevel::Error,
                            format!("Falha no teste FTP: {error}"),
                        );
                        ui.set_notice(format!("Falha ao conectar: {error}").into());
                    }
                }
            });
        });
    });
}

fn register_sync_callbacks(ui: &AppWindow, engine: &SharedEngine, store: Option<Arc<ConfigStore>>) {
    ui.on_start_sync({
        let weak = ui.as_weak();
        let engine = engine.clone();
        move || {
            let ui = weak.unwrap();
            let config = match validate::config(&collect_config(&ui)) {
                Ok(config) => config,
                Err(error) => {
                    push_log(&ui, CoreLogLevel::Error, error.to_string());
                    return ui.set_notice(error.to_string().into());
                }
            };
            if let Some(store) = &store {
                match store.save(&config) {
                    Ok(notices) => notices
                        .into_iter()
                        .for_each(|notice| push_log(&ui, CoreLogLevel::Info, notice)),
                    Err(error) => push_log(
                        &ui,
                        CoreLogLevel::Error,
                        format!("Falha ao salvar configurações: {error}"),
                    ),
                }
            }
            ui.set_busy(true);
            let weak = weak.clone();
            let engine = engine.clone();
            thread::spawn(move || {
                let result = lock(&engine).start(config);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_busy(false);
                    if let Err(error) = result {
                        ui.set_notice(
                            format!("Não foi possível iniciar a sincronização: {error}").into(),
                        );
                    }
                });
            });
        }
    });

    ui.on_stop_sync({
        let weak = ui.as_weak();
        let engine = engine.clone();
        move || {
            weak.unwrap().set_busy(true);
            let weak = weak.clone();
            let engine = engine.clone();
            thread::spawn(move || {
                lock(&engine).stop();
                let _ = weak.upgrade_in_event_loop(|ui| ui.set_busy(false));
            });
        }
    });
}

fn update_row(projects: &VecModel<ProjectRow>, index: usize, update: impl FnOnce(&mut ProjectRow)) {
    if let Some(mut row) = projects.row_data(index) {
        update(&mut row);
        projects.set_row_data(index, row);
    }
}

fn register_project_callbacks(ui: &AppWindow, projects: &Rc<VecModel<ProjectRow>>) {
    ui.on_add_project({
        let projects = projects.clone();
        move || projects.push(ProjectRow::default())
    });
    ui.on_remove_project({
        let projects = projects.clone();
        move |index| {
            let index = index as usize;
            if index < projects.row_count() {
                projects.remove(index);
            }
        }
    });
    ui.on_remote_edited({
        let projects = projects.clone();
        move |index, text| update_row(&projects, index as usize, |row| row.remote = text)
    });
    ui.on_select_local({
        let projects = projects.clone();
        move |index| {
            let projects = projects.clone();
            let _ = slint::spawn_local(async move {
                let picked = rfd::AsyncFileDialog::new()
                    .set_title("Selecionar pasta local")
                    .pick_folder()
                    .await;
                if let Some(folder) = picked {
                    let local: SharedString = folder.path().to_string_lossy().as_ref().into();
                    update_row(&projects, index as usize, |row| row.local = local);
                }
            });
        }
    });
}

struct RemotePicker {
    weak: Weak<AppWindow>,
    target: Cell<usize>,
    config: RefCell<Option<Config>>,
    /// Descarta respostas de listagens antigas (cliques rápidos ou cancelamento).
    request: Arc<AtomicU64>,
}

impl RemotePicker {
    fn load(&self, path: String) {
        let Some(config) = self.config.borrow().clone() else {
            return;
        };
        let ui = self.weak.unwrap();
        let id = self.request.fetch_add(1, Ordering::SeqCst) + 1;
        ui.set_picker_loading(true);
        let weak = self.weak.clone();
        let request = self.request.clone();
        thread::spawn(move || {
            let result = ftp::list_directories(&config, &path);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if request.load(Ordering::SeqCst) != id {
                    return;
                }
                ui.set_picker_loading(false);
                match result {
                    Ok(listing) => {
                        let dirs: Vec<SharedString> =
                            listing.directories.into_iter().map(Into::into).collect();
                        ui.set_picker_path(listing.current_path.into());
                        ui.set_picker_dirs(ModelRc::new(VecModel::from(dirs)));
                    }
                    Err(error) => {
                        ui.set_picker_open(false);
                        ui.set_notice(
                            format!("Falha ao listar diretórios remotos: {error}").into(),
                        );
                    }
                }
            });
        });
    }

    fn close(&self) {
        self.request.fetch_add(1, Ordering::SeqCst);
        self.config.replace(None);
        self.weak.unwrap().set_picker_open(false);
    }
}

fn register_remote_picker(ui: &AppWindow, projects: &Rc<VecModel<ProjectRow>>) {
    let picker = Rc::new(RemotePicker {
        weak: ui.as_weak(),
        target: Cell::new(0),
        config: RefCell::new(None),
        request: Arc::new(AtomicU64::new(0)),
    });

    ui.on_open_remote_picker({
        let picker = picker.clone();
        let projects = projects.clone();
        move |index| {
            let ui = picker.weak.unwrap();
            let Ok(config) = validate::connection(&collect_connection(&ui)) else {
                return ui
                    .set_notice("Preencha host e usuário antes de navegar no FTP remoto.".into());
            };
            let current = projects
                .row_data(index as usize)
                .map(|row| row.remote.to_string())
                .unwrap_or_default();
            let start = remote_path::normalize(&current);
            picker.target.set(index as usize);
            picker.config.replace(Some(config));
            ui.set_picker_path(start.clone().into());
            ui.set_picker_dirs(ModelRc::default());
            ui.set_picker_open(true);
            picker.load(start);
        }
    });
    ui.on_picker_enter({
        let picker = picker.clone();
        move |name| {
            let current = picker.weak.unwrap().get_picker_path();
            picker.load(remote_path::child(&current, &name));
        }
    });
    ui.on_picker_up({
        let picker = picker.clone();
        move || {
            let current = picker.weak.unwrap().get_picker_path();
            picker.load(remote_path::parent(&current));
        }
    });
    ui.on_picker_confirm({
        let picker = picker.clone();
        let projects = projects.clone();
        move || {
            let path = picker.weak.unwrap().get_picker_path();
            update_row(&projects, picker.target.get(), |row| row.remote = path);
            picker.close();
        }
    });
    ui.on_picker_cancel(move || picker.close());
}
