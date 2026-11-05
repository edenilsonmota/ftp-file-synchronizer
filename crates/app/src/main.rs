// Em release no Windows, não abre um console junto com a janela.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod i18n;
mod single_instance;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;

use i18n::Text;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};
use sync_core::config::{DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME};
use sync_core::engine::{EngineEvent, LogLevel as CoreLogLevel};
use sync_core::ignore_rules::{IgnoreRules, parse_patterns};
use sync_core::{
    Config, ConfigStore, Message, Profile, Project, Settings, SyncEngine, Theme, ftp, remote_path,
    validate,
};

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
    ui.set_logs(Rc::new(VecModel::<LogEntry>::default()).into());
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());

    let app = App::load(&ui, ConfigStore::open_default());
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
    register_sync_callbacks(&ui, &app, &engine);
    register_profile_callbacks(&ui, &app);
    register_project_callbacks(&ui, &app);
    register_remote_picker(&ui, &app);
    register_settings_callbacks(&ui, &app);
    ui.on_dismiss_notice({
        let weak = ui.as_weak();
        move || weak.unwrap().set_notice(SharedString::new())
    });

    if app.settings.borrow().auto_start {
        ui.invoke_start_sync();
    }

    ui.run()?;

    // Fechar a janela encerra o app; a configuração editada não se perde.
    app.persist();
    lock(&engine).stop();
    Ok(())
}

fn lock(engine: &SharedEngine) -> MutexGuard<'_, SyncEngine> {
    engine
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Estado da janela: as configurações em memória e os modelos exibidos.
/// Os campos da tela mostram o perfil ativo; `capture` copia as edições de volta.
struct App {
    weak: Weak<AppWindow>,
    settings: RefCell<Settings>,
    store: Option<ConfigStore>,
    projects: Rc<VecModel<ProjectRow>>,
    profiles: Rc<VecModel<ProfileEntry>>,
}

impl App {
    fn load(ui: &AppWindow, store: sync_core::Result<ConfigStore>) -> Rc<Self> {
        let mut notices = Vec::new();
        let mut failures = Vec::new();
        let (store, settings) = match store {
            Ok(store) => {
                let settings = match store.load() {
                    Ok(loaded) => {
                        notices = loaded.notices;
                        loaded.settings
                    }
                    Err(error) => {
                        failures.push(Text::LoadSettingsFailed(error.message()));
                        Settings::default()
                    }
                };
                (Some(store), settings)
            }
            Err(error) => {
                failures.push(Text::OpenSettingsFailed(error.message()));
                (None, Settings::default())
            }
        };

        // Sem idioma salvo é a primeira abertura: usa o do sistema e pergunta.
        let language = settings.language.unwrap_or_else(i18n::detect);
        i18n::apply(language);
        ui.set_language_index(i18n::index(language));
        ui.set_welcome_open(settings.language.is_none());
        for notice in &notices {
            log_message(ui, CoreLogLevel::Info, notice);
        }
        for failure in failures {
            push_log(ui, CoreLogLevel::Error, i18n::text(failure));
        }

        let app = Rc::new(Self {
            weak: ui.as_weak(),
            settings: RefCell::new(settings),
            store,
            projects: Rc::new(VecModel::default()),
            profiles: Rc::new(VecModel::default()),
        });
        ui.set_projects(app.projects.clone().into());
        ui.set_profiles(app.profiles.clone().into());
        app.show_preferences();
        app.show_profiles();
        app
    }

    fn ui(&self) -> AppWindow {
        self.weak.unwrap()
    }

    fn show_preferences(&self) {
        let ui = self.ui();
        let settings = self.settings.borrow();
        ui.set_theme_index(match settings.theme {
            Theme::System => 0,
            Theme::Dark => 1,
            Theme::Light => 2,
        });
        ui.set_sidebar_collapsed(settings.sidebar_collapsed);
        ui.set_auto_start(settings.auto_start);
        ui.set_mirror_deletions(settings.mirror_deletions);
        ui.set_global_ignore(settings.ignore.join("\n").into());
    }

    /// Atualiza a lista de perfis e mostra o perfil ativo nos campos.
    fn show_profiles(&self) {
        let ui = self.ui();
        let settings = self.settings.borrow();
        self.profiles.set_vec(
            settings
                .profiles
                .iter()
                .map(profile_entry)
                .collect::<Vec<_>>(),
        );
        ui.set_profile_index(settings.active_index() as i32);

        let profile = settings.active();
        let config = &profile.config;
        ui.set_profile_name(profile.name.as_str().into());
        ui.set_host(config.host.as_str().into());
        ui.set_user(config.user.as_str().into());
        ui.set_password(config.password.as_str().into());
        ui.set_port(config.port.to_string().into());
        ui.set_secure(config.secure);
        let mut rows: Vec<ProjectRow> = config.projects.iter().map(project_row).collect();
        if rows.is_empty() {
            rows.push(project_row(&Project::default()));
        }
        self.projects.set_vec(rows);
    }

    /// Copia as edições da tela para as configurações em memória.
    fn capture(&self) {
        let ui = self.ui();
        let mut settings = self.settings.borrow_mut();
        // Enquanto as boas-vindas estão abertas, o idioma ainda não foi escolhido.
        if !ui.get_welcome_open() {
            settings.language = Some(i18n::from_index(ui.get_language_index()));
        }
        settings.theme = match ui.get_theme_index() {
            1 => Theme::Dark,
            2 => Theme::Light,
            _ => Theme::System,
        };
        settings.sidebar_collapsed = ui.get_sidebar_collapsed();
        settings.auto_start = ui.get_auto_start();
        settings.mirror_deletions = ui.get_mirror_deletions();
        settings.ignore = parse_patterns(&ui.get_global_ignore());
        let profile = settings.active_mut();
        profile.name = profile_name(&ui.get_profile_name());
        profile.config = collect_config(&ui);
    }

    /// Captura a tela e salva em disco.
    fn persist(&self) {
        self.capture();
        let Some(store) = &self.store else {
            return;
        };
        let ui = self.ui();
        match store.save(&self.settings.borrow()) {
            Ok(notices) => notices
                .iter()
                .for_each(|notice| log_message(&ui, CoreLogLevel::Info, notice)),
            Err(error) => push_log(
                &ui,
                CoreLogLevel::Error,
                i18n::text(Text::SaveSettingsFailed(error.message())),
            ),
        }
    }

    /// Sai da tela de configurações, se aberta. Retorna `false` (e avisa)
    /// se os padrões ignorados globais forem inválidos.
    fn leave_settings(&self) -> bool {
        let ui = self.ui();
        if !ui.get_settings_open() {
            return true;
        }
        if let Err(error) = check_patterns(&parse_patterns(&ui.get_global_ignore())) {
            ui.set_notice(i18n::message(&error.message()).into());
            return false;
        }
        ui.set_settings_open(false);
        true
    }

    /// Adiciona um perfil, torna-o ativo e salva.
    fn add_profile(&self, name: String, config: Config) {
        if !self.leave_settings() {
            return;
        }
        self.capture();
        {
            let mut settings = self.settings.borrow_mut();
            let id = settings.new_profile_id();
            settings.profiles.push(Profile {
                id: id.clone(),
                name,
                config,
            });
            settings.active_profile = id;
        }
        self.show_profiles();
        self.persist();
    }
}

fn profile_name(text: &str) -> String {
    match text.trim() {
        "" => i18n::text(Text::Untitled),
        name => name.to_string(),
    }
}

fn profile_entry(profile: &Profile) -> ProfileEntry {
    ProfileEntry {
        name: profile.name.as_str().into(),
        initial: initial(&profile.name).into(),
        detail: profile.config.host.as_str().into(),
    }
}

/// Primeira letra ou dígito do nome, em maiúscula.
fn initial(name: &str) -> String {
    name.chars()
        .find(|character| character.is_alphanumeric())
        .map(|character| character.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_string())
}

fn project_row(project: &Project) -> ProjectRow {
    ProjectRow {
        local: project.local.to_string_lossy().as_ref().into(),
        remote: project.remote.as_str().into(),
        enabled: project.enabled,
        ignore: project.ignore.join("\n").into(),
        ignore_count: project.ignore.len() as i32,
    }
}

fn create_engine(ui: &AppWindow) -> SharedEngine {
    let cell: Arc<OnceLock<SharedEngine>> = Arc::new(OnceLock::new());
    let weak = ui.as_weak();
    let engine_ref = cell.clone();
    let engine = Arc::new(Mutex::new(SyncEngine::new(Arc::new(
        move |event| match event {
            EngineEvent::Log(level, message) => {
                let _ = weak.upgrade_in_event_loop(move |ui| log_message(&ui, level, &message));
            }
            EngineEvent::Running(running) => {
                let _ = weak.upgrade_in_event_loop(move |ui| ui.set_running(running));
            }
            EngineEvent::WatchFailed(message) => {
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    log_message(&ui, CoreLogLevel::Error, &message)
                });
                if let Some(engine) = engine_ref.get().cloned() {
                    thread::spawn(move || lock(&engine).stop());
                }
            }
        },
    ))));
    let _ = cell.set(engine.clone());
    engine
}

/// Mensagem do motor no log, no idioma atual.
fn log_message(ui: &AppWindow, level: CoreLogLevel, message: &Message) {
    push_log(ui, level, i18n::message(message));
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
            enabled: row.enabled,
            ignore: parse_patterns(&row.ignore),
        })
        .collect();
    Config {
        projects,
        ..collect_connection(ui)
    }
}

/// Abre uma pasta ou um link no aplicativo padrão do sistema.
fn open_external(weak: &Weak<AppWindow>, target: &std::ffi::OsStr) {
    if let Err(error) = open::that_detached(target) {
        weak.unwrap().set_notice(
            i18n::text(Text::OpenFailed {
                target: target.to_string_lossy().into_owned(),
                reason: error.to_string(),
            })
            .into(),
        );
    }
}

/// Confere a sintaxe dos padrões antes de aceitá-los.
fn check_patterns(patterns: &[String]) -> sync_core::Result<()> {
    IgnoreRules::new(Path::new("/"), &[], patterns).map(|_| ())
}

fn register_connection_callbacks(ui: &AppWindow) {
    let weak = ui.as_weak();
    ui.on_test_connection(move || {
        let ui = weak.unwrap();
        let config = match validate::connection(&collect_connection(&ui)) {
            Ok(config) => config,
            Err(error) => return ui.set_notice(i18n::message(&error.message()).into()),
        };
        ui.set_testing(true);
        push_log(
            &ui,
            CoreLogLevel::Info,
            i18n::text(Text::TestingCredentials),
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
                            i18n::text(Text::CredentialsValid),
                        );
                        ui.set_notice(i18n::text(Text::ConnectionSucceeded).into());
                    }
                    Err(error) => {
                        push_log(
                            &ui,
                            CoreLogLevel::Error,
                            i18n::text(Text::TestFailed(error.message())),
                        );
                        ui.set_notice(i18n::text(Text::ConnectFailed(error.message())).into());
                    }
                }
            });
        });
    });
}

fn register_sync_callbacks(ui: &AppWindow, app: &Rc<App>, engine: &SharedEngine) {
    ui.on_start_sync({
        let app = app.clone();
        let engine = engine.clone();
        move || {
            let ui = app.ui();
            app.persist();
            let (config, options) = {
                let settings = app.settings.borrow();
                (
                    validate::config(&settings.active().config),
                    settings.sync_options(),
                )
            };
            let config = match config {
                Ok(config) => config,
                Err(error) => {
                    let message = i18n::message(&error.message());
                    push_log(&ui, CoreLogLevel::Error, message.clone());
                    return ui.set_notice(message.into());
                }
            };
            ui.set_busy(true);
            let weak = ui.as_weak();
            let engine = engine.clone();
            thread::spawn(move || {
                let result = lock(&engine).start(config, options);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_busy(false);
                    if let Err(error) = result {
                        ui.set_notice(i18n::text(Text::StartFailed(error.message())).into());
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

fn register_profile_callbacks(ui: &AppWindow, app: &Rc<App>) {
    ui.on_profile_selected({
        let app = app.clone();
        move |index| {
            if !app.leave_settings() {
                return;
            }
            // Guarda as edições no perfil que estava ativo antes de trocar.
            app.capture();
            {
                let mut settings = app.settings.borrow_mut();
                let Some(profile) = settings.profiles.get(index as usize) else {
                    return;
                };
                settings.active_profile = profile.id.clone();
            }
            app.show_profiles();
            app.persist();
        }
    });
    ui.on_profile_renamed({
        let app = app.clone();
        move |text| {
            let index = app.settings.borrow().active_index();
            if let Some(mut entry) = app.profiles.row_data(index) {
                let name = profile_name(&text);
                entry.initial = initial(&name).into();
                entry.name = name.into();
                app.profiles.set_row_data(index, entry);
            }
        }
    });
    ui.on_add_profile({
        let app = app.clone();
        move || app.add_profile(i18n::text(Text::NewProfile), Config::default())
    });
    ui.on_duplicate_profile({
        let app = app.clone();
        move || {
            app.capture();
            let active = app.settings.borrow().active().clone();
            app.add_profile(
                i18n::text(Text::CopyOf { name: active.name }),
                active.config,
            );
        }
    });
    ui.on_delete_profile({
        let app = app.clone();
        move || {
            let name = profile_name(&app.ui().get_profile_name());
            app.ui()
                .set_confirm_text(i18n::text(Text::ConfirmDeleteProfile { name }).into());
        }
    });
    ui.on_confirm_accepted({
        let app = app.clone();
        move || {
            app.ui().set_confirm_text(SharedString::new());
            let removed = {
                let mut settings = app.settings.borrow_mut();
                if settings.profiles.len() <= 1 {
                    return;
                }
                let index = settings.active_index();
                let removed = settings.profiles.remove(index);
                let next = index.min(settings.profiles.len() - 1);
                settings.active_profile = settings.profiles[next].id.clone();
                removed
            };
            if let Some(store) = &app.store {
                store.delete_secret(&removed.id);
            }
            app.show_profiles();
            app.persist();
        }
    });
    ui.on_confirm_cancelled({
        let weak = ui.as_weak();
        move || weak.unwrap().set_confirm_text(SharedString::new())
    });
}

fn update_row(projects: &VecModel<ProjectRow>, index: usize, update: impl FnOnce(&mut ProjectRow)) {
    if let Some(mut row) = projects.row_data(index) {
        update(&mut row);
        projects.set_row_data(index, row);
    }
}

fn register_project_callbacks(ui: &AppWindow, app: &Rc<App>) {
    let projects = &app.projects;
    ui.on_add_project({
        let projects = projects.clone();
        move || projects.push(project_row(&Project::default()))
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
    ui.on_project_toggled({
        let projects = projects.clone();
        move |index, enabled| update_row(&projects, index as usize, |row| row.enabled = enabled)
    });
    ui.on_select_local({
        let projects = projects.clone();
        move |index| {
            let projects = projects.clone();
            let _ = slint::spawn_local(async move {
                let picked = rfd::AsyncFileDialog::new()
                    .set_title(i18n::text(Text::ChooseLocalFolder))
                    .pick_folder()
                    .await;
                if let Some(folder) = picked {
                    let local: SharedString = folder.path().to_string_lossy().as_ref().into();
                    update_row(&projects, index as usize, |row| row.local = local);
                }
            });
        }
    });

    // Editor dos padrões ignorados de um projeto.
    let target = Rc::new(Cell::new(0usize));
    ui.on_open_ignore({
        let weak = ui.as_weak();
        let projects = projects.clone();
        let target = target.clone();
        move |index| {
            let ui = weak.unwrap();
            let Some(row) = projects.row_data(index as usize) else {
                return;
            };
            let title = Path::new(row.local.as_str())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| i18n::text(Text::ProjectFallback));
            target.set(index as usize);
            ui.set_ignore_title(title.into());
            ui.set_ignore_text(row.ignore);
            ui.set_ignore_open(true);
        }
    });
    ui.on_save_ignore({
        let weak = ui.as_weak();
        let projects = projects.clone();
        move || {
            let ui = weak.unwrap();
            let patterns = parse_patterns(&ui.get_ignore_text());
            if let Err(error) = check_patterns(&patterns) {
                return ui.set_notice(i18n::message(&error.message()).into());
            }
            update_row(&projects, target.get(), |row| {
                row.ignore = patterns.join("\n").into();
                row.ignore_count = patterns.len() as i32;
            });
            ui.set_ignore_open(false);
        }
    });
}

fn register_settings_callbacks(ui: &AppWindow, app: &Rc<App>) {
    ui.on_language_selected(|index| i18n::apply(i18n::from_index(index)));
    ui.on_welcome_confirmed({
        let app = app.clone();
        move || {
            app.ui().set_welcome_open(false);
            app.capture();
            // O perfil criado automaticamente ganha o nome no idioma escolhido.
            {
                let mut settings = app.settings.borrow_mut();
                let name = i18n::text(Text::DefaultProfile);
                for profile in &mut settings.profiles {
                    if profile.id == DEFAULT_PROFILE_ID && profile.name == DEFAULT_PROFILE_NAME {
                        profile.name = name.clone();
                    }
                }
            }
            app.show_profiles();
            app.persist();
        }
    });
    ui.on_open_config_folder({
        let weak = ui.as_weak();
        move || {
            let Some(folder) = ConfigStore::open_default()
                .ok()
                .and_then(|store| store.path().parent().map(Path::to_path_buf))
            else {
                return;
            };
            // A pasta só existe depois do primeiro salvamento.
            let _ = std::fs::create_dir_all(&folder);
            open_external(&weak, folder.as_os_str());
        }
    });
    ui.on_open_repository({
        let weak = ui.as_weak();
        move || open_external(&weak, env!("CARGO_PKG_REPOSITORY").as_ref())
    });
    ui.on_clear_logs({
        let weak = ui.as_weak();
        move || {
            let logs = weak.unwrap().get_logs();
            if let Some(logs) = logs.as_any().downcast_ref::<VecModel<LogEntry>>() {
                logs.set_vec(Vec::new());
            }
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
                        ui.set_notice(i18n::text(Text::ListFailed(error.message())).into());
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

fn register_remote_picker(ui: &AppWindow, app: &Rc<App>) {
    let projects = &app.projects;
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
                return ui.set_notice(i18n::text(Text::FillHostBeforeBrowsing).into());
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

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_backend_testing::{AccessibleRole, ElementHandle};
    use std::collections::HashMap;
    use sync_core::Language;
    use sync_core::config::SecretStore;

    /// Cofre em memória, compartilhado entre os stores de um mesmo teste.
    /// Os testes nunca tocam no cofre real do sistema.
    #[derive(Clone, Default)]
    struct MemorySecrets(Arc<Mutex<HashMap<String, String>>>);

    impl SecretStore for MemorySecrets {
        fn get(&self, account: &str) -> std::result::Result<Option<String>, String> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }
        fn set(&self, account: &str, secret: &str) -> std::result::Result<(), String> {
            self.0.lock().unwrap().insert(account.into(), secret.into());
            Ok(())
        }
        fn delete(&self, account: &str) -> std::result::Result<(), String> {
            self.0.lock().unwrap().remove(account);
            Ok(())
        }
    }

    struct Fixture {
        ui: AppWindow,
        app: Rc<App>,
        dir: tempfile::TempDir,
        secrets: MemorySecrets,
    }

    impl Fixture {
        /// Abre a janela (sem exibir) em inglês, o idioma original dos textos,
        /// com um projeto existente no perfil padrão.
        fn new() -> Self {
            Self::with_language(Some(Language::En))
        }

        /// `None` simula a primeira abertura do app.
        fn with_language(language: Option<Language>) -> Self {
            i_slint_backend_testing::init_no_event_loop();
            let dir = tempfile::tempdir().unwrap();
            let secrets = MemorySecrets::default();
            let project_dir = dir.path().join("site");
            std::fs::create_dir_all(&project_dir).unwrap();
            let mut settings = Settings {
                language,
                ..Settings::default()
            };
            settings.active_mut().config = Config {
                host: "ftp.producao".into(),
                user: "deploy".into(),
                password: "senha-producao".into(),
                projects: vec![Project {
                    local: project_dir,
                    remote: "/web/site".into(),
                    ..Project::default()
                }],
                ..Config::default()
            };
            let store = store(dir.path(), &secrets);
            store.save(&settings).unwrap();

            let ui = AppWindow::new().unwrap();
            // As listas só criam as linhas que cabem na janela.
            ui.window().set_size(slint::LogicalSize::new(1120.0, 780.0));
            ui.set_logs(Rc::new(VecModel::<LogEntry>::default()).into());
            let app = App::load(&ui, Ok(store));
            register_profile_callbacks(&ui, &app);
            register_project_callbacks(&ui, &app);
            register_settings_callbacks(&ui, &app);
            Self {
                ui,
                app,
                dir,
                secrets,
            }
        }

        /// O que está salvo em disco, lido por um store novo.
        fn saved(&self) -> Settings {
            store(self.dir.path(), &self.secrets)
                .load()
                .unwrap()
                .settings
        }

        /// Aciona o botão ou interruptor com esse rótulo (textos com o mesmo
        /// conteúdo, como o balão de dica, também têm o rótulo e são ignorados).
        fn click(&self, label: &str) {
            let control = ElementHandle::find_by_accessible_label(&self.ui, label)
                .find(|element| {
                    matches!(
                        element.accessible_role(),
                        Some(AccessibleRole::Button | AccessibleRole::Checkbox)
                    )
                })
                .unwrap_or_else(|| panic!("nenhum controle com rótulo {label:?}"));
            control.invoke_accessible_default_action();
        }
    }

    fn store(dir: &Path, secrets: &MemorySecrets) -> ConfigStore {
        ConfigStore::new(dir.join("config.json"), None, Box::new(secrets.clone()))
    }

    #[test]
    fn perfis_guardam_credenciais_e_projetos_separados() {
        let fixture = Fixture::new();
        let ui = &fixture.ui;
        assert_eq!(ui.get_host(), "ftp.producao");

        fixture.click("New profile");
        assert_eq!(fixture.app.profiles.row_count(), 2);
        assert_eq!(ui.get_profile_index(), 1);
        assert_eq!(ui.get_host(), "", "perfil novo começa vazio");
        ui.set_profile_name("Homologação".into());
        ui.invoke_profile_renamed("Homologação".into());
        let entry = fixture.app.profiles.row_data(1).unwrap();
        assert_eq!(
            (entry.name.as_str(), entry.initial.as_str()),
            ("Homologação", "H")
        );
        ui.set_host("ftp.homolog".into());
        ui.set_password("senha-homolog".into());

        // Voltar ao primeiro perfil (pela barra lateral) guarda as edições do segundo.
        fixture.click("Padrão");
        assert_eq!(ui.get_host(), "ftp.producao");
        assert_eq!(ui.get_password(), "senha-producao");
        assert_eq!(fixture.app.projects.row_count(), 1);

        let saved = fixture.saved();
        assert_eq!(saved.profiles.len(), 2);
        assert_eq!(saved.active_profile, saved.profiles[0].id);
        let homolog = &saved.profiles[1];
        assert_eq!(homolog.name, "Homologação");
        assert_eq!(homolog.config.host, "ftp.homolog");
        assert_eq!(homolog.config.password, "senha-homolog");
        let config = std::fs::read_to_string(fixture.dir.path().join("config.json")).unwrap();
        assert!(!config.contains("senha-"), "senhas ficam no cofre");
    }

    #[test]
    fn duplicar_e_excluir_perfil() {
        let fixture = Fixture::new();
        let ui = &fixture.ui;
        fixture.click("Duplicate profile");
        assert_eq!(ui.get_profile_name(), "Padrão (copy)");
        assert_eq!(ui.get_host(), "ftp.producao");
        assert_eq!(fixture.app.projects.row_count(), 1);

        // Exclusão pede confirmação; cancelar não apaga nada.
        fixture.click("Delete profile");
        assert!(ui.get_confirm_text().contains("Padrão (copy)"));
        ui.invoke_confirm_cancelled();
        assert_eq!(fixture.saved().profiles.len(), 2);

        fixture.click("Delete profile");
        ui.invoke_confirm_accepted();
        assert_eq!(ui.get_confirm_text(), "");
        assert_eq!(ui.get_profile_name(), "Padrão");
        let saved = fixture.saved();
        assert_eq!(saved.profiles.len(), 1);
        assert_eq!(saved.active().config.password, "senha-producao");
    }

    #[test]
    fn projeto_desativado_e_padroes_ignorados_sao_salvos() {
        let fixture = Fixture::new();
        let ui = &fixture.ui;
        ui.invoke_project_toggled(0, false);
        ui.invoke_open_ignore(0);
        assert!(ui.get_ignore_open());
        assert_eq!(ui.get_ignore_title(), "site");
        ui.set_ignore_text("*.log\n\nvendor/\n".into());
        ui.invoke_save_ignore();
        assert!(!ui.get_ignore_open());
        assert_eq!(fixture.app.projects.row_data(0).unwrap().ignore_count, 2);

        fixture.app.persist();
        let saved = fixture.saved();
        let project = &saved.active().config.projects[0];
        assert!(!project.enabled);
        assert_eq!(project.ignore, vec!["*.log", "vendor/"]);
    }

    #[test]
    fn padrao_invalido_mantem_o_editor_aberto() {
        let fixture = Fixture::new();
        let ui = &fixture.ui;
        ui.invoke_open_ignore(0);
        ui.set_ignore_text("{a,b".into());
        ui.invoke_save_ignore();
        assert!(ui.get_ignore_open());
        assert!(ui.get_notice().contains("Invalid"));
        assert_eq!(fixture.app.projects.row_data(0).unwrap().ignore_count, 0);
    }

    #[test]
    fn configuracoes_gerais_sao_salvas_ao_voltar_ao_perfil() {
        let fixture = Fixture::new();
        let ui = &fixture.ui;
        fixture.click("Settings");
        assert!(ui.get_settings_open());
        fixture.click("Light");
        assert_eq!(ui.get_theme_index(), 2);
        fixture.click("Start syncing when the app opens");
        fixture.click("Mirror deletions on the server");

        // Padrão inválido: continua nas configurações.
        ui.set_global_ignore("{a,b".into());
        fixture.click("Padrão");
        assert!(ui.get_settings_open());
        assert!(ui.get_notice().contains("Invalid"));
        ui.set_notice(SharedString::new());

        ui.set_global_ignore("node_modules/\n*.tmp".into());
        fixture.click("Padrão");
        assert!(!ui.get_settings_open());

        let saved = fixture.saved();
        assert_eq!(saved.theme, Theme::Light);
        assert!(saved.auto_start);
        assert!(!saved.mirror_deletions);
        assert_eq!(saved.ignore, vec!["node_modules/", "*.tmp"]);
    }

    #[test]
    fn barra_lateral_recolhe_e_lembra_o_estado() {
        let fixture = Fixture::new();
        let ui = &fixture.ui;
        fixture.click("Collapse menu");
        assert!(ui.get_sidebar_collapsed());
        // Recolhida, os perfis e as ações continuam acessíveis.
        fixture.click("Settings");
        assert!(ui.get_settings_open());
        fixture.click("Padrão");
        assert!(!ui.get_settings_open());
        assert!(fixture.saved().sidebar_collapsed);

        fixture.click("Expand menu");
        assert!(!ui.get_sidebar_collapsed());
        fixture.app.persist();
        assert!(!fixture.saved().sidebar_collapsed);
    }

    #[test]
    fn acoes_principais_tem_botoes_acessiveis() {
        let fixture = Fixture::new();
        for label in [
            "Start syncing",
            "New profile",
            "Duplicate profile",
            "Profile name",
            "FTPS",
            "Sync this project",
            "Settings",
            "Test connection",
            "Add project",
            "Choose local folder",
            "Browse FTP folder",
            "Ignored files",
            "Remove project",
            "Clear logs",
        ] {
            assert!(
                ElementHandle::find_by_accessible_label(&fixture.ui, label)
                    .next()
                    .is_some(),
                "botão {label:?} não encontrado"
            );
        }
        fixture.click("Add project");
        assert_eq!(fixture.app.projects.row_count(), 2);
        fixture.click("Remove project");
        assert_eq!(fixture.app.projects.row_count(), 1);
    }

    #[test]
    fn primeira_abertura_pergunta_o_idioma() {
        let fixture = Fixture::with_language(None);
        let ui = &fixture.ui;
        assert!(ui.get_welcome_open());
        assert_eq!(ui.get_language_index(), i18n::index(i18n::detect()));

        // Escolher um idioma traduz a interface na hora.
        fixture.click("Español");
        assert_eq!(i18n::current(), Language::Es);
        assert!(
            ElementHandle::find_by_accessible_label(ui, "Configuración")
                .next()
                .is_some()
        );
        fixture.click("Continuar");
        assert!(!ui.get_welcome_open());

        // O perfil criado automaticamente ganha o nome no idioma escolhido.
        let saved = fixture.saved();
        assert_eq!(saved.language, Some(Language::Es));
        assert_eq!(saved.active().name, "Predeterminado");
        assert_eq!(ui.get_profile_name(), "Predeterminado");
    }

    #[test]
    fn trocar_idioma_nas_configuracoes() {
        let fixture = Fixture::new();
        let ui = &fixture.ui;
        assert!(!ui.get_welcome_open());
        fixture.click("Settings");
        fixture.click("Português");
        assert!(
            ElementHandle::find_by_accessible_label(ui, "Configurações")
                .next()
                .is_some()
        );
        assert_eq!(i18n::text(Text::NewProfile), "Novo perfil");
        fixture.click("Padrão");
        // Perfil existente com nome dado pelo usuário não é renomeado.
        let saved = fixture.saved();
        assert_eq!(saved.language, Some(Language::Pt));
        assert_eq!(saved.active().name, "Padrão");
    }

    /// Lê os textos de um `.po` (msgid → msgstr).
    fn po_entries(po: &str) -> std::collections::HashMap<String, String> {
        let unquote = |line: &str, prefix: &str| {
            line.strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix('"'))
                .map(|text| text.replace("\\\"", "\"").replace("\\\\", "\\"))
        };
        let mut entries = std::collections::HashMap::new();
        let mut msgid = None;
        for line in po.lines() {
            if let Some(id) = unquote(line, "msgid \"") {
                msgid = Some(id);
            } else if let (Some(id), Some(text)) = (msgid.take(), unquote(line, "msgstr \"")) {
                entries.insert(id, text);
            }
        }
        entries
    }

    #[test]
    fn todo_texto_da_interface_tem_traducao() {
        let ui = include_str!("../ui/app.slint");
        let ids: Vec<&str> = ui
            .split("@tr(\"")
            .skip(1)
            .map(|rest| &rest[..rest.find('"').unwrap()])
            .collect();
        assert!(ids.len() > 50, "textos encontrados: {}", ids.len());
        for (language, po) in [
            (
                "pt",
                include_str!("../translations/pt/LC_MESSAGES/ftp-file-synchronizer.po"),
            ),
            (
                "es",
                include_str!("../translations/es/LC_MESSAGES/ftp-file-synchronizer.po"),
            ),
        ] {
            let entries = po_entries(po);
            for id in &ids {
                let translated = entries.get(*id).map(String::as_str).unwrap_or_default();
                assert!(!translated.is_empty(), "sem tradução em {language}: {id:?}");
            }
        }
    }
}
