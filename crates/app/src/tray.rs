//! Ícone na bandeja do sistema.
//!
//! - Windows e macOS: `tray-icon`, com as APIs nativas. O ícone é criado na
//!   thread da interface com o laço de eventos já rodando, porque é ela que
//!   processa as mensagens do sistema (no macOS, criar antes do laço faz o
//!   ícone não aparecer).
//! - Linux: `ksni`, que publica um StatusNotifierItem pelo D-Bus numa thread
//!   própria, sem GTK. Funciona no KDE, XFCE, Cinnamon, Waybar e no GNOME com a
//!   extensão AppIndicator. Sem bandeja no ambiente, avisa `Available(false)`.
//!
//! As ações chegam por `on_action` de qualquer thread, às vezes de dentro do
//! tratamento de mensagens do sistema. Quem recebe só agenda o trabalho no laço
//! de eventos e nunca mexe na janela ali: é isso que evita travar a janela.

use std::io::Cursor;
use std::sync::{Arc, OnceLock};

const SMALL_PNG: &[u8] = include_bytes!("../../../assets/icons/32x32.png");
const LARGE_PNG: &[u8] = include_bytes!("../../../assets/icons/64x64.png");
/// Selo verde: sincronizando.
const RUNNING_COLOR: [u8; 3] = [0x30, 0xd1, 0x58];
/// Selo laranja: arquivos que falharam.
const ATTENTION_COLOR: [u8; 3] = [0xff, 0x9f, 0x0a];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Mostrar a janela e trazê-la para a frente.
    Show,
    /// Iniciar ou parar a sincronização, conforme o estado atual.
    ToggleSync,
    Quit,
    /// A bandeja ficou disponível (`true`) ou sumiu (`false`), por exemplo ao
    /// reiniciar a barra do ambiente no Linux.
    Available(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Running,
    Attention,
}

/// O que a bandeja mostra. Os textos já chegam traduzidos.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub status: Status,
    /// Iniciando ou parando: o item de sincronização fica desativado.
    pub busy: bool,
    pub tooltip: String,
    pub show: String,
    pub sync: String,
    pub quit: String,
}

type Handler = Arc<dyn Fn(Action) + Send + Sync>;

pub struct Tray(platform::Tray);

impl Tray {
    /// Cria o ícone. Chamar na thread da interface, com o laço de eventos rodando.
    pub fn new(
        state: State,
        on_action: impl Fn(Action) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        platform::Tray::new(state, Arc::new(on_action)).map(Self)
    }

    pub fn update(&self, state: State) {
        self.0.update(state);
    }
}

/// No macOS, o ícone no Dock só aparece com a janela aberta, como nos apps de
/// barra de menus. Nos outros sistemas não faz nada.
pub fn set_dock_visible(visible: bool) {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

        let Some(main_thread) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(main_thread);
        let policy = if visible {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        };
        app.setActivationPolicy(policy);
        if visible {
            // `activate` só existe a partir do macOS 14.
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = visible;
}

/// Imagem RGBA (alfa não pré-multiplicado).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Image {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn decode(bytes: &[u8]) -> Image {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().expect("ícone embutido é um PNG válido");
    let mut rgba = vec![0; reader.output_buffer_size().expect("ícone pequeno")];
    let info = reader
        .next_frame(&mut rgba)
        .expect("ícone embutido é um PNG válido");
    rgba.truncate(info.buffer_size());
    Image {
        width: info.width,
        height: info.height,
        rgba,
    }
}

/// O ícone do app em 32 e 64 px.
fn base_images() -> &'static [Image; 2] {
    static IMAGES: OnceLock<[Image; 2]> = OnceLock::new();
    IMAGES.get_or_init(|| [decode(SMALL_PNG), decode(LARGE_PNG)])
}

/// Ícone do estado nos dois tamanhos (do menor para o maior).
fn images(status: Status) -> Vec<Image> {
    let color = match status {
        Status::Stopped => None,
        Status::Running => Some(RUNNING_COLOR),
        Status::Attention => Some(ATTENTION_COLOR),
    };
    base_images()
        .iter()
        .map(|image| match color {
            Some(color) => with_badge(image, color),
            None => image.clone(),
        })
        .collect()
}

/// Desenha um selo redondo no canto inferior direito, com uma borda
/// transparente em volta para destacá-lo do logo.
fn with_badge(image: &Image, color: [u8; 3]) -> Image {
    let size = image.width.min(image.height) as f32;
    let radius = size * 0.2;
    let gap = size * 0.07;
    let center = (
        image.width as f32 - radius - gap,
        image.height as f32 - radius - gap,
    );
    let mut result = image.clone();
    for (index, pixel) in result.rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = (index as u32 % image.width) as f32 + 0.5;
        let y = (index as u32 / image.width) as f32 + 0.5;
        let distance = ((x - center.0).powi(2) + (y - center.1).powi(2)).sqrt();
        let cut = coverage(distance, radius + gap);
        if cut == 0.0 {
            continue;
        }
        let fill = coverage(distance, radius);
        let below = pixel[3] as f32 / 255.0 * (1.0 - cut);
        let alpha = fill + below * (1.0 - fill);
        for channel in 0..3 {
            pixel[channel] = if alpha > 0.0 {
                ((color[channel] as f32 * fill + pixel[channel] as f32 * below * (1.0 - fill))
                    / alpha)
                    .round() as u8
            } else {
                0
            };
        }
        pixel[3] = (alpha * 255.0).round() as u8;
    }
    result
}

/// Quanto de um pixel a esta distância do centro cai dentro do círculo
/// (borda suavizada em 1 px).
fn coverage(distance: f32, radius: f32) -> f32 {
    (radius - distance + 0.5).clamp(0.0, 1.0)
}

#[cfg(target_os = "linux")]
mod platform {
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::thread;

    use ksni::blocking::{Handle, TrayMethods};
    use ksni::menu::StandardItem;

    use super::{Action, Handler, State, images};

    const PRODUCT: &str = "FTP File Synchronizer";

    struct Item {
        state: State,
        handler: Handler,
    }

    impl Item {
        fn entry(&self, label: &str, enabled: bool, action: Action) -> ksni::MenuItem<Self> {
            StandardItem {
                // No D-Bus, `_` marca a tecla de atalho; `__` é um `_` literal.
                label: label.replace('_', "__"),
                enabled,
                activate: Box::new(move |item: &mut Self| (item.handler)(action)),
                ..Default::default()
            }
            .into()
        }
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            sync_core::APP_ID.into()
        }

        fn title(&self) -> String {
            PRODUCT.into()
        }

        fn category(&self) -> ksni::Category {
            ksni::Category::ApplicationStatus
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            images(self.state.status)
                .into_iter()
                .map(|image| ksni::Icon {
                    width: image.width as i32,
                    height: image.height as i32,
                    // O D-Bus espera ARGB.
                    data: image
                        .rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|pixel| [pixel[3], pixel[0], pixel[1], pixel[2]])
                        .collect(),
                })
                .collect()
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: self.state.tooltip.clone(),
                ..Default::default()
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            (self.handler)(Action::Show);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            vec![
                self.entry(&self.state.show, true, Action::Show),
                ksni::MenuItem::Separator,
                self.entry(&self.state.sync, !self.state.busy, Action::ToggleSync),
                ksni::MenuItem::Separator,
                self.entry(&self.state.quit, true, Action::Quit),
            ]
        }

        fn watcher_online(&self) {
            (self.handler)(Action::Available(true));
        }

        fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
            log::warn!("Bandeja do sistema indisponível: {reason:?}");
            (self.handler)(Action::Available(false));
            // Continua ativo para reaparecer quando a barra voltar.
            true
        }
    }

    #[derive(Default)]
    struct Shared {
        state: Option<State>,
        handle: Option<Handle<Item>>,
        closed: bool,
    }

    pub struct Tray {
        shared: Arc<Mutex<Shared>>,
    }

    fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
        shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    impl Tray {
        pub fn new(state: State, handler: Handler) -> Result<Self, String> {
            let shared = Arc::new(Mutex::new(Shared {
                state: Some(state.clone()),
                ..Shared::default()
            }));
            // Registrar no D-Bus pode demorar; a interface não espera.
            let background = shared.clone();
            thread::Builder::new()
                .name("tray".into())
                .spawn(move || {
                    let item = Item {
                        state,
                        handler: handler.clone(),
                    };
                    match item.spawn() {
                        Ok(handle) => {
                            let mut shared = lock(&background);
                            if shared.closed {
                                handle.shutdown().wait();
                                return;
                            }
                            // O estado pode ter mudado enquanto registrava.
                            if let Some(state) = shared.state.clone() {
                                handle.update(|item| item.state = state);
                            }
                            shared.handle = Some(handle);
                            drop(shared);
                            handler(Action::Available(true));
                        }
                        Err(error) => {
                            log::warn!("Bandeja do sistema indisponível: {error}");
                            handler(Action::Available(false));
                        }
                    }
                })
                .map_err(|error| error.to_string())?;
            Ok(Self { shared })
        }

        pub fn update(&self, state: State) {
            let handle = {
                let mut shared = lock(&self.shared);
                if shared.state.as_ref() == Some(&state) {
                    return;
                }
                shared.state = Some(state.clone());
                shared.handle.clone()
            };
            if let Some(handle) = handle {
                handle.update(|item| item.state = state);
            }
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            let handle = {
                let mut shared = lock(&self.shared);
                shared.closed = true;
                shared.handle.take()
            };
            if let Some(handle) = handle {
                handle.shutdown().wait();
            }
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod platform {
    use std::cell::RefCell;

    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    use super::{Action, Handler, State, Status, images};

    const SHOW: &str = "show";
    const SYNC: &str = "sync";
    const QUIT: &str = "quit";

    pub struct Tray {
        icon: TrayIcon,
        show: MenuItem,
        sync: MenuItem,
        quit: MenuItem,
        state: RefCell<State>,
    }

    fn icon(status: Status) -> Result<Icon, String> {
        let images = images(status);
        // Windows: 32 px (a bandeja usa 16 a 32). macOS: 64 px, reduzido para a barra de menus.
        let image = if cfg!(windows) {
            &images[0]
        } else {
            &images[images.len() - 1]
        };
        Icon::from_rgba(image.rgba.clone(), image.width, image.height)
            .map_err(|error| error.to_string())
    }

    impl Tray {
        pub fn new(state: State, handler: Handler) -> Result<Self, String> {
            let show = MenuItem::with_id(SHOW, &state.show, true, None);
            let sync = MenuItem::with_id(SYNC, &state.sync, !state.busy, None);
            let quit = MenuItem::with_id(QUIT, &state.quit, true, None);
            let menu = Menu::new();
            menu.append_items(&[
                &show,
                &PredefinedMenuItem::separator(),
                &sync,
                &PredefinedMenuItem::separator(),
                &quit,
            ])
            .map_err(|error| error.to_string())?;

            MenuEvent::set_event_handler(Some({
                let handler = handler.clone();
                move |event: MenuEvent| match event.id().0.as_str() {
                    SHOW => handler(Action::Show),
                    SYNC => handler(Action::ToggleSync),
                    QUIT => handler(Action::Quit),
                    _ => {}
                }
            }));
            // Windows: o clique esquerdo abre a janela e o direito, o menu.
            // macOS: o clique abre o menu, como nos outros ícones da barra.
            #[cfg(windows)]
            tray_icon::TrayIconEvent::set_event_handler(Some({
                let handler = handler.clone();
                move |event| {
                    if let tray_icon::TrayIconEvent::Click {
                        button: tray_icon::MouseButton::Left,
                        button_state: tray_icon::MouseButtonState::Up,
                        ..
                    } = event
                    {
                        handler(Action::Show);
                    }
                }
            }));

            let icon = TrayIconBuilder::new()
                .with_id(sync_core::APP_ID)
                .with_menu(Box::new(menu))
                .with_icon(icon(state.status)?)
                .with_tooltip(&state.tooltip)
                .with_menu_on_left_click(cfg!(target_os = "macos"))
                .build()
                .map_err(|error| error.to_string())?;
            handler(Action::Available(true));
            Ok(Self {
                icon,
                show,
                sync,
                quit,
                state: RefCell::new(state),
            })
        }

        pub fn update(&self, state: State) {
            let mut current = self.state.borrow_mut();
            if *current == state {
                return;
            }
            if current.status != state.status {
                match icon(state.status) {
                    Ok(icon) => {
                        let _ = self.icon.set_icon(Some(icon));
                    }
                    Err(error) => log::warn!("Ícone da bandeja inválido: {error}"),
                }
            }
            let _ = self.icon.set_tooltip(Some(&state.tooltip));
            self.show.set_text(&state.show);
            self.sync.set_text(&state.sync);
            self.sync.set_enabled(!state.busy);
            self.quit.set_text(&state.quit);
            *current = state;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(image: &Image, x: u32, y: u32) -> [u8; 4] {
        let start = ((y * image.width + x) * 4) as usize;
        image.rgba[start..start + 4].try_into().unwrap()
    }

    #[test]
    fn icones_embutidos_sao_rgba_nos_dois_tamanhos() {
        let [small, large] = base_images();
        assert_eq!((small.width, small.height), (32, 32));
        assert_eq!((large.width, large.height), (64, 64));
        assert_eq!(small.rgba.len(), 32 * 32 * 4);
        assert_eq!(large.rgba.len(), 64 * 64 * 4);
    }

    #[test]
    fn selo_do_estado_fica_no_canto_e_preserva_o_resto() {
        let base = &base_images()[1];
        let stopped = images(Status::Stopped);
        assert_eq!(&stopped[1], base);

        let running = &images(Status::Running)[1];
        // Centro do selo: raio 12,8 e folga 4,48 a partir da borda.
        let center = pixel(running, 46, 46);
        assert_eq!(center, [0x30, 0xd1, 0x58, 255]);
        assert_eq!(pixel(running, 10, 10), pixel(base, 10, 10));

        let attention = &images(Status::Attention)[0];
        assert_eq!(pixel(attention, 23, 23), [0xff, 0x9f, 0x0a, 255]);
    }

    #[test]
    fn borda_do_selo_fica_transparente() {
        let opaque = Image {
            width: 64,
            height: 64,
            rgba: [10, 20, 30, 255].repeat(64 * 64),
        };
        let badged = with_badge(&opaque, RUNNING_COLOR);
        // Entre o selo (raio 12,8) e o fim da folga (raio 17,28), a 15 px do centro.
        let center = 64.0 - 12.8 - 4.48;
        let x = (center - 15.0_f32).round() as u32;
        let y = center.round() as u32;
        assert_eq!(pixel(&badged, x, y)[3], 0);
        assert_eq!(pixel(&badged, 0, 0), [10, 20, 30, 255]);
    }
}
