use gtk4::{prelude::*, Application, Box as GtkBox, Button, Entry, Label, ListBox, Orientation, ScrolledWindow, MenuButton, PopoverMenu, CssProvider, FileChooserDialog, FileChooserAction};
use gtk4::glib;
use gtk4::gio;
use libadwaita::{prelude::*, ApplicationWindow as AdwApplicationWindow, HeaderBar, StatusPage, StyleManager, MessageDialog, ResponseAppearance};
use std::sync::{Arc, Mutex};
use std::path::PathBuf;
use std::time::Instant;
use futures_util::StreamExt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use tokio::sync::Mutex as AsyncMutex;
use async_channel;
use serde::{Serialize, Deserialize};
use chrono::{DateTime, Utc};

const APP_ID: &str = "com.downstream.app";
const DEFAULT_NUM_CHUNKS: u64 = 4; // Número padrão de chunks paralelos
const MIN_CHUNK_SIZE: u64 = 1024 * 1024; // 1MB - tamanho mínimo por chunk
const MAX_RETRIES: u32 = 3; // Número máximo de tentativas em caso de erro de conexão
const RETRY_DELAY_SECS: u64 = 2; // Delay entre tentativas em segundos

// ===== DESIGN TOKENS =====
// Sistema de espaçamento padronizado (ultra minimalista)
const SPACING_LARGE: i32 = 8;  // Espaçamento entre seções principais
const SPACING_MEDIUM: i32 = 6;  // Espaçamento entre grupos relacionados
const SPACING_SMALL: i32 = 4;   // Espaçamento entre elementos próximos
const SPACING_TINY: i32 = 2;    // Espaçamento mínimo dentro de componentes

// Sistema de border radius (ultra minimalista)
const RADIUS_LARGE: &str = "6px";   // Cards, badges grandes
const RADIUS_MEDIUM: &str = "4px";  // Componentes médios

// Sistema de cores (usando paleta Tailwind para consistência)
const COLOR_SUCCESS: &str = "#10b981";  // Verde - Downloads concluídos
const COLOR_INFO: &str = "#3b82f6";     // Azul - Em progresso
const COLOR_WARNING: &str = "#f59e0b";  // Âmbar - Pausado
const COLOR_ERROR: &str = "#ef4444";    // Vermelho - Falhas
const COLOR_NEUTRAL: &str = "#6b7280";  // Cinza - Cancelado

// Sistema de opacidade
const OPACITY_DIM_TEXT: f32 = 0.75;     // Texto secundário
const OPACITY_CANCELLED: f32 = 0.65;    // Items cancelados

#[derive(Clone, Debug)]
enum DownloadMessage {
    Progress(f64, String, String, String, bool, u64), // (progress, status_text, speed, eta, parallel_chunks, speed_bytes)
    Complete,
    Error(String),
}

#[derive(Debug)]
struct DownloadTask {
    paused: bool,
    cancelled: bool,
    file_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DownloadRecord {
    url: String,
    filename: String,
    file_path: Option<String>,
    status: DownloadStatus,
    date_added: DateTime<Utc>,
    date_completed: Option<DateTime<Utc>>,
    downloaded_bytes: u64, // Quantidade já baixada (para resume)
    total_bytes: u64,      // Tamanho total do arquivo
    #[serde(default)]      // Para compatibilidade com arquivos antigos
    was_paused: bool,      // Se estava pausado quando o app foi fechado
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum DownloadStatus {
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppConfig {
    download_directory: Option<String>, // Caminho da pasta de downloads padrão
    window_width: Option<i32>, // Largura da janela
    window_height: Option<i32>, // Altura da janela
}

struct AppState {
    downloads: Vec<Arc<Mutex<DownloadTask>>>,
    records: Arc<Mutex<Vec<DownloadRecord>>>,
    config: Arc<Mutex<AppConfig>>,
    download_speeds: Arc<Mutex<std::collections::HashMap<String, u64>>>, // URL -> velocidade em bytes/s
}

// Função para sanitizar e limitar o tamanho do nome do arquivo
fn sanitize_filename(url: &str) -> String {
    // Extrai o nome do arquivo da URL
    let filename = url.split('/').last().unwrap_or("download").to_string();

    // Remove query parameters se houver
    let filename_clean = filename.split('?').next().unwrap_or(&filename);

    // Remove caracteres inválidos no sistema de arquivos
    let filename_safe = filename_clean
        .replace(['<', '>', ':', '"', '|', '?', '*'], "_")
        .replace(['\\', '/'], "_");

    // Limita o tamanho do nome (considerando extensão)
    const MAX_FILENAME_LENGTH: usize = 200; // Limite seguro para a maioria dos sistemas

    if filename_safe.len() > MAX_FILENAME_LENGTH {
        // Tenta preservar a extensão
        if let Some(dot_pos) = filename_safe.rfind('.') {
            let extension = &filename_safe[dot_pos..];
            let name_part = &filename_safe[..dot_pos];

            // Se a extensão é razoável (< 10 chars), preserva ela
            if extension.len() < 10 {
                let max_name_len = MAX_FILENAME_LENGTH - extension.len();
                format!("{}{}", &name_part[..max_name_len.min(name_part.len())], extension)
            } else {
                // Extensão muito grande, trunca tudo
                filename_safe[..MAX_FILENAME_LENGTH].to_string()
            }
        } else {
            // Sem extensão, apenas trunca
            filename_safe[..MAX_FILENAME_LENGTH].to_string()
        }
    } else if filename_safe.is_empty() || filename_safe == "/" {
        // Nome vazio ou inválido
        "download".to_string()
    } else {
        filename_safe
    }
}

fn main() {
    let app = Application::builder()
        .application_id(APP_ID)
        .build();

    // Cria ações globais para o menu
    let show_action = gio::SimpleAction::new("show", None);
    let quit_action = gio::SimpleAction::new("quit", None);
    
    let app_clone = app.clone();
    show_action.connect_activate(move |_, _| {
        if let Some(window) = app_clone.active_window() {
            window.present();
            window.set_visible(true);
        }
    });
    
    let app_clone = app.clone();
    quit_action.connect_activate(move |_, _| {
        app_clone.quit();
    });
    
    app.add_action(&show_action);
    app.add_action(&quit_action);

    app.connect_activate(build_ui);
    app.run();
}

fn get_data_file_path() -> PathBuf {
    // Obtém diretório de dados do app (funciona em Linux, Windows, macOS)
    let data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("keeper");

    // Cria o diretório se não existir
    let _ = std::fs::create_dir_all(&data_dir);

    data_dir.join("downloads.json")
}

fn get_config_file_path() -> PathBuf {
    let data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("keeper");
    let _ = std::fs::create_dir_all(&data_dir);
    data_dir.join("config.json")
}

fn load_config() -> AppConfig {
    let file_path = get_config_file_path();
    if !file_path.exists() {
        return AppConfig {
            download_directory: None,
            window_width: None,
            window_height: None,
        };
    }
    match std::fs::read_to_string(&file_path) {
        Ok(contents) => {
            serde_json::from_str(&contents).unwrap_or_else(|_| AppConfig {
                download_directory: None,
                window_width: None,
                window_height: None,
            })
        }
        Err(_) => AppConfig {
            download_directory: None,
            window_width: None,
            window_height: None,
        },
    }
}

fn save_config(config: &AppConfig) {
    let file_path = get_config_file_path();
    match serde_json::to_string_pretty(config) {
        Ok(json) => {
            let temp_path = file_path.with_extension("json.tmp");
            if let Err(e) = std::fs::write(&temp_path, json) {
                eprintln!("Erro ao escrever arquivo de configuração temporário: {}", e);
                return;
            }
            if let Err(e) = std::fs::rename(&temp_path, &file_path) {
                eprintln!("Erro ao renomear arquivo de configuração: {}", e);
                let _ = std::fs::remove_file(&temp_path);
            }
        }
        Err(e) => {
            eprintln!("Erro ao serializar configuração: {}", e);
        }
    }
}

fn get_download_directory(config: &AppConfig) -> PathBuf {
    if let Some(ref dir) = config.download_directory {
        PathBuf::from(dir)
    } else {
        dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
    }
}

fn load_downloads() -> Vec<DownloadRecord> {
    let file_path = get_data_file_path();

    if !file_path.exists() {
        return Vec::new();
    }

    match std::fs::read_to_string(&file_path) {
        Ok(contents) => {
            serde_json::from_str(&contents).unwrap_or_else(|_| Vec::new())
        }
        Err(_) => Vec::new(),
    }
}

fn format_file_size(bytes: u64) -> String {
    if bytes == 0 {
        return "Desconhecido".to_string();
    }
    
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

fn save_downloads(records: &[DownloadRecord]) {
    let file_path = get_data_file_path();

    match serde_json::to_string_pretty(records) {
        Ok(json) => {
            // Tenta escrever o arquivo, criando um arquivo temporário primeiro para garantir atomicidade
            let temp_path = file_path.with_extension("json.tmp");
            if let Err(e) = std::fs::write(&temp_path, json) {
                eprintln!("Erro ao escrever arquivo temporário: {}", e);
                return;
            }
            // Renomeia o arquivo temporário para o arquivo final (operação atômica)
            if let Err(e) = std::fs::rename(&temp_path, &file_path) {
                eprintln!("Erro ao renomear arquivo: {}", e);
                let _ = std::fs::remove_file(&temp_path);
            }
        }
        Err(e) => {
            eprintln!("Erro ao serializar downloads: {}", e);
        }
    }
}

fn build_ui(app: &Application) {
    let style_manager = StyleManager::default();
    style_manager.set_color_scheme(libadwaita::ColorScheme::ForceDark);

    // Carrega downloads salvos e configurações
    let saved_records = load_downloads();
    let config = load_config();
    let config_clone = config.clone();

    let state = Arc::new(Mutex::new(AppState {
        downloads: Vec::new(),
        records: Arc::new(Mutex::new(saved_records.clone())),
        config: Arc::new(Mutex::new(config)),
        download_speeds: Arc::new(Mutex::new(std::collections::HashMap::new())),
    }));

    let window = AdwApplicationWindow::builder()
        .application(app)
        .title("Keepers")
        .default_width(700)
        .default_height(500)
        .build();

    // Aplica tamanho salvo se existir
    if let Some(width) = config_clone.window_width {
        if let Some(height) = config_clone.window_height {
            window.set_default_size(width, height);
        }
    }


    // ToastOverlay para notificações in-app
    let toast_overlay = libadwaita::ToastOverlay::new();

    let main_box = GtkBox::new(Orientation::Vertical, 0);

    let header = HeaderBar::new();

    // Botão principal de adicionar download no header (moderno)
    let add_download_btn = Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Adicionar novo download (Ctrl+N)")
        .css_classes(vec!["suggested-action"])
        .margin_start(SPACING_LARGE)
        .margin_end(SPACING_LARGE)
        .build();

    header.pack_end(&add_download_btn);

    // Box para badges de atividade
    let badges_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .margin_end(12)
        .build();

    // Badge de downloads ativos (em progresso)
    let active_badge_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(4)
        .css_classes(vec!["badge-container", "active"])
        .visible(false)
        .build();

    let active_icon = gtk4::Image::builder()
        .icon_name("folder-download-symbolic")
        .pixel_size(16)
        .build();

    let active_label = Label::builder()
        .css_classes(vec!["badge-label"])
        .build();

    active_badge_box.append(&active_icon);
    active_badge_box.append(&active_label);

    // Badge de downloads pausados
    let paused_badge_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(4)
        .css_classes(vec!["badge-container", "paused"])
        .visible(false)
        .build();

    let paused_icon = gtk4::Image::builder()
        .icon_name("media-playback-pause-symbolic")
        .pixel_size(16)
        .build();

    let paused_label = Label::builder()
        .css_classes(vec!["badge-label"])
        .build();

    paused_badge_box.append(&paused_icon);
    paused_badge_box.append(&paused_label);

    // Badge de downloads com erro
    let error_badge_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(4)
        .css_classes(vec!["badge-container", "error"])
        .visible(false)
        .build();

    let error_icon = gtk4::Image::builder()
        .icon_name("dialog-error-symbolic")
        .pixel_size(16)
        .build();

    let error_label = Label::builder()
        .css_classes(vec!["badge-label"])
        .build();

    error_badge_box.append(&error_icon);
    error_badge_box.append(&error_label);

    badges_box.append(&active_badge_box);
    badges_box.append(&paused_badge_box);
    badges_box.append(&error_badge_box);

    header.pack_start(&badges_box);

    // Função para atualizar badges
    let update_badges = {
        let state_badges = state.clone();
        let active_badge_box_update = active_badge_box.clone();
        let paused_badge_box_update = paused_badge_box.clone();
        let error_badge_box_update = error_badge_box.clone();
        let active_label_update = active_label.clone();
        let paused_label_update = paused_label.clone();
        let error_label_update = error_label.clone();

        move || {
            if let Ok(app_state) = state_badges.lock() {
                if let Ok(records) = app_state.records.lock() {
                    // Conta downloads por status
                    let active_count = records.iter().filter(|r|
                        r.status == DownloadStatus::InProgress && !r.was_paused
                    ).count();

                    let paused_count = records.iter().filter(|r|
                        r.status == DownloadStatus::InProgress && r.was_paused
                    ).count();

                    let error_count = records.iter().filter(|r|
                        r.status == DownloadStatus::Failed || r.status == DownloadStatus::Cancelled
                    ).count();

                    // Atualiza badge de ativos
                    if active_count > 0 {
                        active_label_update.set_text(&active_count.to_string());
                        active_badge_box_update.set_tooltip_text(Some(&format!("{} download(s) ativo(s)", active_count)));
                        active_badge_box_update.set_visible(true);
                    } else {
                        active_badge_box_update.set_visible(false);
                    }

                    // Atualiza badge de pausados
                    if paused_count > 0 {
                        paused_label_update.set_text(&paused_count.to_string());
                        paused_badge_box_update.set_tooltip_text(Some(&format!("{} download(s) pausado(s)", paused_count)));
                        paused_badge_box_update.set_visible(true);
                    } else {
                        paused_badge_box_update.set_visible(false);
                    }

                    // Atualiza badge de erros
                    if error_count > 0 {
                        error_label_update.set_text(&error_count.to_string());
                        error_badge_box_update.set_tooltip_text(Some(&format!("{} download(s) com erro/cancelado(s)", error_count)));
                        error_badge_box_update.set_visible(true);
                    } else {
                        error_badge_box_update.set_visible(false);
                    }
                }
            }
        }
    };

    // Atualiza badges inicialmente
    update_badges();

    // Atualiza badges a cada 2 segundos
    glib::timeout_add_seconds_local(2, {
        let update_fn = update_badges.clone();
        move || {
            update_fn();
            glib::ControlFlow::Continue
        }
    });

    // Adiciona menu button no header para system tray
    let menu_button = MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Menu principal")
        .build();

    let menu = gio::Menu::new();
    menu.append(Some("Mostrar Janela"), Some("app.show"));

    // Submenu de configurações
    let config_menu = gio::Menu::new();
    config_menu.append(Some("Pasta de Downloads"), Some("app.config-downloads"));

    let config_section = gio::Menu::new();
    config_section.append_submenu(Some("Configurações"), &config_menu);
    menu.append_section(None, &config_section);

    menu.append(Some("Sobre"), Some("app.about"));
    menu.append(Some("Sair"), Some("app.quit"));

    let popover = PopoverMenu::from_model(Some(&menu));
    menu_button.set_popover(Some(&popover));

    header.pack_end(&menu_button);

    // Ação para configurações de pasta de downloads
    let config_action = gio::SimpleAction::new("config-downloads", None);
    let window_clone_config = window.clone();
    let state_clone_config = state.clone();
    let toast_overlay_config = toast_overlay.clone();
    config_action.connect_activate(move |_, _| {
        let config_window = window_clone_config.clone();
        let config_state = state_clone_config.clone();
        let toast_overlay_response = toast_overlay_config.clone();

        // Cria diálogo de seleção de pasta
        let dialog = FileChooserDialog::new(
            Some("Selecionar Pasta de Downloads"),
            Some(&config_window),
            FileChooserAction::SelectFolder,
            &[("Cancelar", gtk4::ResponseType::Cancel), ("Selecionar", gtk4::ResponseType::Accept)],
        );

        dialog.set_modal(true);

        // Conecta a resposta
        let config_state_response = config_state.clone();
        dialog.connect_response(move |dialog, response| {
            if response == gtk4::ResponseType::Accept {
                if let Some(file) = dialog.file() {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().to_string();
                        let path_display = path.clone();

                        // Atualiza configuração
                        if let Ok(app_state) = config_state_response.lock() {
                            if let Ok(mut config) = app_state.config.lock() {
                                config.download_directory = Some(path_str.clone());
                                save_config(&config);

                                // Mostra toast com confirmação
                                let toast = libadwaita::Toast::new(&format!(
                                    "Pasta de downloads alterada para:\n{}",
                                    path_str
                                ));
                                toast.set_timeout(5);
                                toast.set_priority(libadwaita::ToastPriority::High);

                                // Adiciona botão de ação para abrir a pasta
                                toast.set_button_label(Some("Abrir Pasta"));
                                let path_for_action = path_display.clone();
                                toast.connect_button_clicked(move |_| {
                                    let _ = open::that(&path_for_action);
                                });

                                toast_overlay_response.add_toast(toast);
                            }
                        }
                    }
                }
            }
            dialog.close();
        });

        dialog.show();
    });
    app.add_action(&config_action);

    // Ação para mostrar diálogo "Sobre"
    let about_action = gio::SimpleAction::new("about", None);
    let window_clone_about = window.clone();
    about_action.connect_activate(move |_, _| {
        let about_window = libadwaita::AboutWindow::builder()
            .transient_for(&window_clone_about)
            .application_name("Keeper")
            .application_icon("folder-download")
            .developer_name("Karan Luciano")
            .version("1.0.0")
            .comments("Gerenciador minimalista de downloads com suporte a downloads paralelos")
            .website("https://github.com/KaranLuciano/Keeper")
            .issue_url("https://github.com/KaranLuciano/Keeper/issues")
            .copyright("© 2025 Karan Luciano")
            .license_type(gtk4::License::MitX11)
            .build();

        // Adiciona desenvolvedores
        about_window.set_developers(&["Karan Luciano"]);

        // Adiciona tecnologias utilizadas
        about_window.add_credit_section(
            Some("Tecnologias"),
            &[
                "Rust - Linguagem de programação",
                "GTK4 - Interface gráfica",
                "libadwaita - Design GNOME",
                "Tokio - Runtime assíncrono",
                "Reqwest - Cliente HTTP",
            ],
        );

        about_window.present();
    });
    app.add_action(&about_action);

    main_box.append(&header);

    let scrolled = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .margin_start(SPACING_LARGE)
        .margin_end(SPACING_LARGE)
        .margin_bottom(SPACING_LARGE)
        .build();

    let list_box = ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(vec!["boxed-list"])
        .build();

    // Container principal para incluir painel de métricas + lista
    let list_container = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(SPACING_MEDIUM)
        .build();

    // Painel de métricas fixo no topo
    let metrics_panel = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .css_classes(vec!["metrics-panel"])
        .margin_top(SPACING_MEDIUM)
        .build();

    // Título do painel
    let metrics_title = Label::builder()
        .label("Resumo Geral")
        .halign(gtk4::Align::Start)
        .css_classes(vec!["title-4"])
        .build();

    // Grid para organizar as métricas em colunas
    let metrics_grid = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_LARGE)
        .homogeneous(true)
        .margin_top(SPACING_SMALL)
        .margin_bottom(SPACING_SMALL)
        .build();

    // Métrica: Downloads por Status
    let status_metrics_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .css_classes(vec!["metric-card"])
        .build();

    let status_metrics_title = Label::builder()
        .label("Downloads")
        .halign(gtk4::Align::Start)
        .css_classes(vec!["caption-heading", "dim-label"])
        .build();

    let status_metrics_value = Label::builder()
        .label("0 total")
        .halign(gtk4::Align::Start)
        .css_classes(vec!["title-2", "metric-value"])
        .build();

    let status_metrics_details = Label::builder()
        .label("0 ativos • 0 pausados • 0 erros")
        .halign(gtk4::Align::Start)
        .css_classes(vec!["caption", "dim-label"])
        .wrap(true)
        .build();

    status_metrics_box.append(&status_metrics_title);
    status_metrics_box.append(&status_metrics_value);
    status_metrics_box.append(&status_metrics_details);

    // Métrica: Velocidade Agregada
    let speed_metrics_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .css_classes(vec!["metric-card"])
        .build();

    let speed_metrics_title = Label::builder()
        .label("Velocidade")
        .halign(gtk4::Align::Start)
        .css_classes(vec!["caption-heading", "dim-label"])
        .build();

    let speed_metrics_value = Label::builder()
        .label("0 B/s")
        .halign(gtk4::Align::Start)
        .css_classes(vec!["title-2", "metric-value"])
        .build();

    let speed_metrics_details = Label::builder()
        .label("Nenhum download ativo")
        .halign(gtk4::Align::Start)
        .css_classes(vec!["caption", "dim-label"])
        .wrap(true)
        .build();

    speed_metrics_box.append(&speed_metrics_title);
    speed_metrics_box.append(&speed_metrics_value);
    speed_metrics_box.append(&speed_metrics_details);

    // Métrica: Espaço Total
    let space_metrics_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .css_classes(vec!["metric-card"])
        .build();

    let space_metrics_title = Label::builder()
        .label("Espaço Total")
        .halign(gtk4::Align::Start)
        .css_classes(vec!["caption-heading", "dim-label"])
        .build();

    let space_metrics_value = Label::builder()
        .label("0 B")
        .halign(gtk4::Align::Start)
        .css_classes(vec!["title-2", "metric-value"])
        .build();

    let space_metrics_details = Label::builder()
        .label("0 B completados")
        .halign(gtk4::Align::Start)
        .css_classes(vec!["caption", "dim-label"])
        .wrap(true)
        .build();

    space_metrics_box.append(&space_metrics_title);
    space_metrics_box.append(&space_metrics_value);
    space_metrics_box.append(&space_metrics_details);

    // Adiciona as métricas ao grid
    metrics_grid.append(&status_metrics_box);
    metrics_grid.append(&speed_metrics_box);
    metrics_grid.append(&space_metrics_box);

    metrics_panel.append(&metrics_title);
    metrics_panel.append(&metrics_grid);

    // Adiciona painel e lista ao container
    list_container.append(&metrics_panel);
    list_container.append(&list_box);

    scrolled.set_child(Some(&list_container));

    // Função para atualizar métricas do painel
    let update_metrics = {
        let state_metrics = state.clone();
        let status_value_update = status_metrics_value.clone();
        let status_details_update = status_metrics_details.clone();
        let speed_value_update = speed_metrics_value.clone();
        let speed_details_update = speed_metrics_details.clone();
        let space_value_update = space_metrics_value.clone();
        let space_details_update = space_metrics_details.clone();

        move || {
            if let Ok(app_state) = state_metrics.lock() {
                if let Ok(records) = app_state.records.lock() {
                    // Contadores por status
                    let total_count = records.len();
                    let active_count = records.iter().filter(|r|
                        r.status == DownloadStatus::InProgress && !r.was_paused
                    ).count();
                    let paused_count = records.iter().filter(|r|
                        r.status == DownloadStatus::InProgress && r.was_paused
                    ).count();
                    let error_count = records.iter().filter(|r|
                        r.status == DownloadStatus::Failed || r.status == DownloadStatus::Cancelled
                    ).count();
                    let completed_count = records.iter().filter(|r|
                        r.status == DownloadStatus::Completed
                    ).count();

                    // Atualiza métrica de status
                    status_value_update.set_text(&format!("{} total", total_count));
                    status_details_update.set_text(&format!(
                        "{} ativos • {} pausados • {} erros",
                        active_count, paused_count, error_count
                    ));

                    // Calcula velocidade agregada de todos os downloads ativos
                    if let Ok(speeds) = app_state.download_speeds.lock() {
                        let total_speed: u64 = speeds.values().sum();
                        if total_speed > 0 {
                            let speed_str = if total_speed >= 1_048_576 {
                                format!("{:.2} MB/s", total_speed as f64 / 1_048_576.0)
                            } else if total_speed >= 1_024 {
                                format!("{:.2} KB/s", total_speed as f64 / 1_024.0)
                            } else {
                                format!("{} B/s", total_speed)
                            };
                            speed_value_update.set_text(&speed_str);
                            speed_details_update.set_text(&format!("{} download(s) ativo(s)", active_count));
                        } else if active_count > 0 {
                            speed_value_update.set_text("0 B/s");
                            speed_details_update.set_text("Calculando velocidade...");
                        } else {
                            speed_value_update.set_text("0 B/s");
                            speed_details_update.set_text("Nenhum download ativo");
                        }
                    }

                    // Calcula espaço total
                    let total_size: u64 = records.iter()
                        .filter(|r| r.total_bytes > 0)
                        .map(|r| r.total_bytes)
                        .sum();

                    let completed_size: u64 = records.iter()
                        .filter(|r| r.status == DownloadStatus::Completed)
                        .map(|r| r.downloaded_bytes)
                        .sum();

                    let total_size_str = if total_size >= 1_073_741_824 {
                        format!("{:.2} GB", total_size as f64 / 1_073_741_824.0)
                    } else if total_size >= 1_048_576 {
                        format!("{:.2} MB", total_size as f64 / 1_048_576.0)
                    } else if total_size >= 1_024 {
                        format!("{:.2} KB", total_size as f64 / 1_024.0)
                    } else {
                        format!("{} B", total_size)
                    };

                    let completed_size_str = if completed_size >= 1_073_741_824 {
                        format!("{:.2} GB", completed_size as f64 / 1_073_741_824.0)
                    } else if completed_size >= 1_048_576 {
                        format!("{:.2} MB", completed_size as f64 / 1_048_576.0)
                    } else if completed_size >= 1_024 {
                        format!("{:.2} KB", completed_size as f64 / 1_024.0)
                    } else {
                        format!("{} B", completed_size)
                    };

                    space_value_update.set_text(&total_size_str);
                    space_details_update.set_text(&format!(
                        "{} completados ({} downloads)",
                        completed_size_str, completed_count
                    ));
                }
            }
        }
    };

    // Atualiza métricas inicialmente
    update_metrics();

    // Atualiza métricas a cada 2 segundos
    glib::timeout_add_seconds_local(2, {
        let update_fn = update_metrics.clone();
        move || {
            update_fn();
            glib::ControlFlow::Continue
        }
    });

    // Estado vazio com botão de ação proeminente
    let empty_state_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .vexpand(true)
        .valign(gtk4::Align::Center)
        .spacing(8)
        .build();

    let empty_status = StatusPage::builder()
        .icon_name("folder-download-symbolic")
        .title("Nenhum download")
        .description("Clique no botão + acima ou pressione Ctrl+N para adicionar um novo download")
        .build();

    // Botão proeminente no estado vazio (ação secundária, pois o primário está no header)
    let empty_add_btn = Button::builder()
        .label("Adicionar Download")
        .icon_name("list-add-symbolic")
        .halign(gtk4::Align::Center)
        .css_classes(vec!["pill", "suggested-action"])
        .build();

    let empty_btn_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .halign(gtk4::Align::Center)
        .build();
    empty_btn_box.append(&empty_add_btn);

    empty_state_box.append(&empty_status);
    empty_state_box.append(&empty_btn_box);

    let content_stack = gtk4::Stack::new();
    content_stack.add_named(&empty_state_box, Some("empty"));
    content_stack.add_named(&scrolled, Some("list"));
    content_stack.set_visible_child_name("empty");

    main_box.append(&content_stack);

    // Carrega downloads salvos e adiciona à lista
    if !saved_records.is_empty() {
        content_stack.set_visible_child_name("list");

        // Separa downloads que devem retomar automaticamente
        let mut to_resume = Vec::new();

        for record in saved_records {
            // Se estava em progresso e NÃO estava pausado, marca para retomar
            if record.status == DownloadStatus::InProgress && !record.was_paused {
                to_resume.push(record.url.clone());
            } else {
                // Caso contrário, mostra como download completo/pausado/falhado/cancelado
                add_completed_download(&list_box, &record, &state, &content_stack);
            }
        }

        // Remove downloads que vão retomar do JSON (evita duplicação)
        if !to_resume.is_empty() {
            if let Ok(app_state) = state.lock() {
                if let Ok(mut records) = app_state.records.lock() {
                    for url in &to_resume {
                        records.retain(|r| &r.url != url);
                    }
                    save_downloads(&records);
                }
            }
        }

        // Retoma downloads ativos
        for url in to_resume {
            add_download(&list_box, &url, &state, &content_stack);
        }
    }

    // Cria função para mostrar o diálogo de adicionar download
    let show_add_dialog = {
        let list_box_clone = list_box.clone();
        let content_stack_clone = content_stack.clone();
        let state_clone = state.clone();
        let window_clone = window.clone();

        move || {
            // Cria a modal
            let dialog = MessageDialog::builder()
                .transient_for(&window_clone)
                .heading("Adicionar Download")
                .body("Insira a URL completa do arquivo que deseja baixar")
                .build();

            // Adiciona botões de ação
            dialog.add_response("cancel", "Cancelar");
            dialog.add_response("download", "Iniciar Download");
            dialog.set_response_appearance("download", ResponseAppearance::Suggested);
            dialog.set_close_response("cancel");

            // Desabilita botão "Baixar" inicialmente
            dialog.set_response_enabled("download", false);

            // Container principal com melhor espaçamento
            let main_box = GtkBox::builder()
                .orientation(Orientation::Vertical)
                .spacing(12)
                .margin_top(12)
                .margin_bottom(12)
                .margin_start(16)
                .margin_end(16)
                .build();

            // Label descritivo
            let label = Label::builder()
                .label("URL do arquivo")
                .halign(gtk4::Align::Start)
                .css_classes(vec!["title-4"])
                .build();

            // Campo de entrada de URL com tamanho melhor
            let url_entry = Entry::builder()
                .placeholder_text("https://exemplo.com/arquivo.zip")
                .activates_default(false)
                .width_request(450)
                .build();

            // Tenta capturar URL do clipboard automaticamente
            if let Some(display) = gtk4::gdk::Display::default() {
                let clipboard = display.clipboard();
                let url_entry_clone = url_entry.clone();
                clipboard.read_text_async(None::<&gio::Cancellable>, move |result| {
                    if let Ok(Some(text)) = result {
                        let text = text.to_string().trim().to_string();
                        // Verifica se é uma URL válida
                        if (text.starts_with("http://") || text.starts_with("https://")) && !text.contains('\n') {
                            url_entry_clone.set_text(&text);
                        }
                    }
                });
            }

            // Preview do nome do arquivo (inicialmente invisível)
            let preview_box = GtkBox::builder()
                .orientation(Orientation::Horizontal)
                .spacing(8)
                .halign(gtk4::Align::Start)
                .visible(false)
                .build();

            let preview_icon = gtk4::Image::builder()
                .icon_name("document-save-symbolic")
                .pixel_size(16)
                .build();

            let preview_label = Label::builder()
                .halign(gtk4::Align::Start)
                .css_classes(vec!["dim-label", "caption"])
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .build();

            preview_box.append(&preview_icon);
            preview_box.append(&preview_label);

            // Histórico recente de URLs (últimos 5 downloads)
            let history_expander = libadwaita::ExpanderRow::builder()
                .title("Histórico Recente")
                .subtitle("Clique para reutilizar uma URL anterior")
                .build();

            // Pega os últimos 5 downloads do histórico
            if let Ok(app_state) = state_clone.lock() {
                if let Ok(records) = app_state.records.lock() {
                    let recent_urls: Vec<_> = records.iter()
                        .rev()
                        .take(5)
                        .map(|r| (r.url.clone(), r.filename.clone()))
                        .collect();

                    for (url_hist, filename_hist) in recent_urls {
                        let history_row = libadwaita::ActionRow::builder()
                            .title(&filename_hist)
                            .subtitle(&url_hist)
                            .activatable(true)
                            .build();

                        let url_entry_hist = url_entry.clone();
                        let url_hist_clone = url_hist.clone();
                        history_row.connect_activated(move |_| {
                            url_entry_hist.set_text(&url_hist_clone);
                            url_entry_hist.grab_focus();
                        });

                        history_expander.add_row(&history_row);
                    }
                }
            }

            // Texto de ajuda
            let help_label = Label::builder()
                .label("O download iniciará automaticamente após adicionar")
                .halign(gtk4::Align::Start)
                .css_classes(vec!["dim-label", "caption"])
                .build();

            main_box.append(&label);
            main_box.append(&url_entry);
            main_box.append(&preview_box);
            main_box.append(&help_label);

            // Só mostra histórico se houver registros
            if history_expander.first_child().is_some() {
                let separator = gtk4::Separator::builder()
                    .orientation(Orientation::Horizontal)
                    .margin_top(12)
                    .margin_bottom(12)
                    .build();
                main_box.append(&separator);
                main_box.append(&history_expander);
            }

            dialog.set_extra_child(Some(&main_box));

            // Label de erro para duplicatas
            let error_label = Label::builder()
                .halign(gtk4::Align::Start)
                .css_classes(vec!["error", "caption"])
                .wrap(true)
                .visible(false)
                .build();

            main_box.append(&error_label);

            // Conecta validação em tempo real
            let dialog_clone = dialog.clone();
            let error_label_changed = error_label.clone();
            let preview_box_changed = preview_box.clone();
            let preview_label_changed = preview_label.clone();
            url_entry.connect_changed(move |entry| {
                let url = entry.text().to_string().trim().to_string();
                // Remove classe de erro quando usuário começar a digitar
                entry.remove_css_class("error");
                // Esconde mensagem de erro
                error_label_changed.set_visible(false);
                // Valida se tem conteúdo e começa com http:// ou https://
                let is_valid = !url.is_empty() && (url.starts_with("http://") || url.starts_with("https://"));
                dialog_clone.set_response_enabled("download", is_valid);

                // Mostra preview do nome do arquivo se a URL for válida
                if is_valid {
                    // Extrai e sanitiza o nome do arquivo da URL
                    let filename_clean = sanitize_filename(&url);

                    if filename_clean != "download" {
                        preview_label_changed.set_text(&format!("📄 Arquivo: {}", filename_clean));
                        preview_box_changed.set_visible(true);
                    } else {
                        preview_box_changed.set_visible(false);
                    }

                    dialog_clone.set_default_response(Some("download"));
                    // Reativa o activates_default quando válido
                    entry.set_activates_default(true);
                } else {
                    preview_box_changed.set_visible(false);
                    dialog_clone.set_default_response(None);
                    entry.set_activates_default(false);
                }
            });

            // Clones necessários para o callback
            let list_box_dialog = list_box_clone.clone();
            let content_stack_dialog = content_stack_clone.clone();
            let state_dialog = state_clone.clone();
            let url_entry_response = url_entry.clone();

            // Conecta resposta da modal
            let error_label_response = error_label.clone();
            dialog.connect_response(None, move |dialog, response| {
                if response == "download" {
                    let url = url_entry_response.text().to_string().trim().to_string();

                    // Valida se tem conteúdo e começa com http:// ou https://
                    if url.is_empty() || (!url.starts_with("http://") && !url.starts_with("https://")) {
                        // URL inválida
                        url_entry_response.add_css_class("error");
                        error_label_response.set_text("URL inválida. Use http:// ou https://");
                        error_label_response.set_visible(true);
                        return;
                    }

                    // Verifica se já existe um download com esta URL
                    let mut existing_record: Option<DownloadRecord> = None;
                    if let Ok(app_state) = state_dialog.lock() {
                        if let Ok(records) = app_state.records.lock() {
                            existing_record = records.iter().find(|r| r.url == url).cloned();
                        }
                    }

                    if let Some(record) = existing_record {
                        // URL duplicada - mostra diálogo de aviso
                        let warning_dialog = libadwaita::MessageDialog::new(
                            Some(dialog),
                            Some("Download Duplicado"),
                            Some("Este arquivo já existe na lista de downloads."),
                        );

                        let status_text = match record.status {
                            DownloadStatus::InProgress => if record.was_paused { "pausado" } else { "em progresso" },
                            DownloadStatus::Completed => "concluído",
                            DownloadStatus::Failed => "com falha",
                            DownloadStatus::Cancelled => "cancelado",
                        };

                        let body_text = format!(
                            "Arquivo: {}\n\nStatus: {}\nAdicionado em: {}",
                            record.filename,
                            status_text,
                            record.date_added.format("%d/%m/%Y às %H:%M")
                        );

                        warning_dialog.set_body(&body_text);
                        warning_dialog.add_response("ok", "Entendi");
                        warning_dialog.set_response_appearance("ok", libadwaita::ResponseAppearance::Suggested);
                        warning_dialog.set_default_response(Some("ok"));
                        warning_dialog.set_close_response("ok");

                        warning_dialog.present();
                    } else {
                        // URL válida e não duplicada, pode adicionar
                        add_download(&list_box_dialog, &url, &state_dialog, &content_stack_dialog);
                        content_stack_dialog.set_visible_child_name("list");
                        dialog.close();
                    }
                } else {
                    dialog.close();
                }
            });

            // Foca automaticamente no campo de entrada quando a modal abre
            url_entry.grab_focus();

            dialog.present();
        }
    };

    // Cria ação para adicionar download (permite atalho de teclado)
    let add_action = gio::SimpleAction::new("add-download", None);
    let show_add_dialog_action = show_add_dialog.clone();
    add_action.connect_activate(move |_, _| {
        show_add_dialog_action();
    });
    window.add_action(&add_action);

    // Adiciona atalho de teclado Ctrl+N
    app.set_accels_for_action("win.add-download", &["<Ctrl>N"]);

    // Conecta botão do header
    let show_add_dialog_header = show_add_dialog.clone();
    add_download_btn.connect_clicked(move |_| {
        show_add_dialog_header();
    });

    // Conecta botão do empty state
    empty_add_btn.connect_clicked(move |_| {
        show_add_dialog();
    });

    toast_overlay.set_child(Some(&main_box));
    window.set_content(Some(&toast_overlay));
    
    // Adiciona CSS customizado usando design tokens
    let provider = CssProvider::new();
    let css = format!("
        /* ===== DESIGN SYSTEM BASEADO EM TOKENS ===== */

        /* Cor de fundo do container principal (ScrolledWindow) */
        scrolledwindow {{
            background-color: transparent;
        }}

        /* Cor de fundo da lista de downloads (ListBox) */
        list {{
            background-color: transparent;
        }}

        /* Cor de fundo da lista de downloads com classe boxed-list */
        .boxed-list {{
            background-color: transparent;
        }}

        /* Botão de adicionar no header - margens ajustadas */
        headerbar button.suggested-action {{
            margin-left: 8px;
            margin-right: 8px;
        }}

        /* Card minimalista - sem bordas, sem background */
        .download-card {{
            border: none;
            border-radius: {};
            background-color: alpha(currentColor, 0.08);
            padding: 10px;
        }}

        /* Progress bar visível e moderna - altura aumentada */
        .download-progress {{
            min-height: 20px;
            border-radius: 6px;
            font-size: 11px;
            font-weight: 600;
        }}

        .download-progress trough {{
            background-color: alpha(currentColor, 0.1);
            border-radius: 6px;
            min-height: 20px;
        }}

        /* Texto da porcentagem sempre visível e contrastante */
        .download-progress text {{
            color: @window_fg_color;
            text-shadow: 0 0 3px rgba(0, 0, 0, 0.5);
        }}

        /* Barra de progresso - Em Progresso (Azul) */
        .download-progress.in-progress trough progress {{
            background: {};
            min-height: 20px;
            border-radius: 6px;
        }}

        .download-progress.in-progress text {{
            color: white;
        }}

        /* Barra de progresso - Pausado (Amarelo/Âmbar) */
        .download-progress.paused trough progress {{
            background: {};
            min-height: 20px;
            border-radius: 6px;
        }}

        .download-progress.paused text {{
            color: rgba(0, 0, 0, 0.9);
        }}

        /* Barra de progresso - Completo (Verde) */
        .download-progress.completed trough progress {{
            background: {};
            min-height: 20px;
            border-radius: 6px;
        }}

        .download-progress.completed text {{
            color: white;
        }}

        /* Barra de progresso - Cancelado (Cinza) */
        .download-progress.cancelled trough progress {{
            background: {};
            min-height: 20px;
            border-radius: 6px;
        }}

        .download-progress.cancelled text {{
            color: white;
        }}

        /* Barra de progresso - Falhou (Vermelho) */
        .download-progress.failed trough progress {{
            background: {};
            min-height: 20px;
            border-radius: 6px;
        }}

        .download-progress.failed text {{
            color: white;
        }}

        /* Badges minimalistas - sem background, apenas cor de texto */
        .status-badge {{
            border-radius: 0;
            padding: 0;
            margin: 0;
            background-color: transparent;
        }}

        .status-badge.completed {{
            color: {};
        }}

        .status-badge.in-progress {{
            color: {};
        }}

        .status-badge.paused {{
            color: {};
        }}

        .status-badge.failed {{
            color: {};
        }}

        .status-badge.cancelled {{
            color: {};
        }}

        /* Metadados minimalistas - sem background */
        .metadata-group {{
            padding: 0;
            border-radius: 0;
            background-color: transparent;
        }}

        /* Melhor contraste para labels secundários */
        .dim-label {{
            opacity: {};
        }}

        /* Downloads cancelados com melhor legibilidade */
        .cancelled-download {{
            opacity: {};
        }}

        /* Melhorias para modais de entrada */
        messagedialog entry {{
            min-height: 40px;
            font-size: 14px;
            padding: 8px 12px;
        }}

        /* Estado de erro no campo */
        entry.error {{
            border-color: {};
            background-color: alpha({}, 0.1);
        }}

        /* ===== BADGES DE ATIVIDADE NO HEADER ===== */

        /* Container do badge - estilo pill moderno */
        .badge-container {{
            background-color: alpha(currentColor, 0.08);
            border-radius: 12px;
            padding: 4px 10px;
            margin-left: 4px;
            margin-right: 4px;
        }}

        /* Badge de downloads ativos - azul */
        .badge-container.active {{
            background-color: alpha({}, 0.15);
        }}

        .badge-container.active .badge-label {{
            color: {};
            font-weight: 700;
        }}

        /* Badge de downloads pausados - amarelo/âmbar */
        .badge-container.paused {{
            background-color: alpha({}, 0.15);
        }}

        .badge-container.paused .badge-label {{
            color: {};
            font-weight: 700;
        }}

        /* Badge de downloads com erro - vermelho */
        .badge-container.error {{
            background-color: alpha({}, 0.15);
        }}

        .badge-container.error .badge-label {{
            color: {};
            font-weight: 700;
        }}

        /* Label do badge - tipografia */
        .badge-label {{
            font-size: 12px;
            font-weight: 600;
            letter-spacing: 0.5px;
        }}

        /* ===== PAINEL DE MÉTRICAS ===== */

        /* Container do painel */
        .metrics-panel {{
            background-color: alpha(currentColor, 0.03);
            border-radius: {};
            padding: {};
            margin-bottom: {};
        }}

        /* Cards individuais de métrica */
        .metric-card {{
            background-color: alpha(currentColor, 0.05);
            border-radius: {};
            padding: {};
            min-width: 180px;
        }}

        /* Valor principal da métrica */
        .metric-value {{
            font-weight: 700;
            color: @accent_color;
        }}
    ",
        RADIUS_LARGE,
        // Cores da barra de progresso por status
        COLOR_INFO,           // in-progress (azul)
        COLOR_WARNING,        // paused (amarelo/âmbar)
        COLOR_SUCCESS,        // completed (verde)
        COLOR_NEUTRAL,        // cancelled (cinza)
        COLOR_ERROR,          // failed (vermelho)
        // Cores dos badges de status
        COLOR_SUCCESS,        // completed badge
        COLOR_INFO,           // in-progress badge
        COLOR_WARNING,        // paused badge
        COLOR_ERROR,          // failed badge
        COLOR_NEUTRAL,        // cancelled badge
        // Opacidades
        OPACITY_DIM_TEXT,
        OPACITY_CANCELLED,
        // Estado de erro
        COLOR_ERROR,          // border-color do erro
        COLOR_ERROR,          // background-color do erro
        // Badges de atividade no header
        COLOR_INFO,           // active badge background
        COLOR_INFO,           // active badge text
        COLOR_WARNING,        // paused badge background
        COLOR_WARNING,        // paused badge text
        COLOR_ERROR,          // error badge background
        COLOR_ERROR,          // error badge text
        // Painel de métricas
        RADIUS_LARGE,         // border-radius do painel
        "16px",               // padding do painel
        "12px",               // margin-bottom do painel
        RADIUS_MEDIUM,        // border-radius dos cards
        "12px"                // padding dos cards
    );
    
    provider.load_from_data(&css);
    
    // Adiciona o provider CSS ao display
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(&display, &provider, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
    
    // Salva tamanho da janela periodicamente durante redimensionamento
    let state_save_size = state.clone();
    let window_save_size = window.clone();
    let save_timer_running = Arc::new(Mutex::new(false));
    
    {
        let window_timer = window_save_size.clone();
        let state_timer = state_save_size.clone();
        let timer_running = save_timer_running.clone();
        
        glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            if let Ok(mut running) = timer_running.lock() {
                if *running {
                    let (w, h) = window_timer.default_size();
                    if let Ok(app_state) = state_timer.lock() {
                        if let Ok(mut config) = app_state.config.lock() {
                            config.window_width = Some(w);
                            config.window_height = Some(h);
                            save_config(&config);
                        }
                    }
                    *running = false;
                }
            }
            glib::ControlFlow::Continue
        });
    }
    
    // Marca que precisa salvar quando a janela for redimensionada
    // Usa um timer periódico que verifica o tamanho da janela
    let window_check = window_save_size.clone();
    let timer_check = save_timer_running.clone();
    let last_size = Arc::new(Mutex::new((0, 0)));
    
    {
        let window_size_check = window_check.clone();
        let timer_size_check = timer_check.clone();
        let last_size_check = last_size.clone();
        
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let (w, h) = window_size_check.default_size();
            let mut changed = false;
            {
                if let Ok(mut last) = last_size_check.lock() {
                    if w != last.0 || h != last.1 {
                        *last = (w, h);
                        changed = true;
                    }
                }
            }
            if changed {
                if let Ok(mut running) = timer_size_check.lock() {
                    *running = true;
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // Salva tamanho quando a janela for fechada/minimizada
    let state_close = state.clone();
    let window_close = window.clone();
    window.connect_close_request(move |_| {
        let (w, h) = window_close.default_size();
        if let Ok(app_state) = state_close.lock() {
            if let Ok(mut config) = app_state.config.lock() {
                config.window_width = Some(w);
                config.window_height = Some(h);
                save_config(&config);
            }
        }
        window_close.set_visible(false);
        glib::Propagation::Stop
    });
    
    window.present();
    
    // Nota: Esta implementação adiciona um menu no header
    // Para um verdadeiro system tray icon no Linux, você precisaria:
    // 1. Adicionar dependência libappindicator (via bindings Rust)
    // 2. Ou usar uma biblioteca como tray-item
    // Por enquanto, o menu no header funciona como alternativa
}

fn add_completed_download(list_box: &ListBox, record: &DownloadRecord, state: &Arc<Mutex<AppState>>, content_stack: &gtk4::Stack) {
    let row_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(SPACING_MEDIUM)
        .margin_top(SPACING_MEDIUM)
        .margin_bottom(SPACING_MEDIUM)
        .margin_start(SPACING_MEDIUM)
        .margin_end(SPACING_MEDIUM)
        .css_classes(vec!["download-card"])
        .build();

    // Se estiver cancelado, aplica estilo especial (opaco)
    let is_cancelled = record.status == DownloadStatus::Cancelled;
    if is_cancelled {
        row_box.add_css_class("cancelled-download");
    }

    // Header com título - tipografia melhorada
    let title_label = Label::builder()
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .css_classes(vec!["title-2"])
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();

    // Se cancelado, adiciona risco no meio do texto usando Pango markup
    if is_cancelled {
        title_label.set_markup(&markup_title_strikethrough(&record.filename));
    } else {
        title_label.set_markup(&markup_title(&record.filename));
    }

    // Barra de progresso
    let (fraction, text) = if record.status == DownloadStatus::InProgress && record.total_bytes > 0 {
        let progress = record.downloaded_bytes as f64 / record.total_bytes as f64;
        (progress, format!("{:.0}%", progress * 100.0))
    } else if record.status == DownloadStatus::Completed {
        (1.0, "100%".to_string())
    } else {
        (0.0, "0%".to_string())
    };

    let progress_bar = gtk4::ProgressBar::builder()
        .hexpand(true)
        .show_text(true)
        .fraction(fraction)
        .text(&text)
        .css_classes(vec!["download-progress"])
        .build();

    // Aplica classe CSS baseada no status
    let progress_status_class = match record.status {
        DownloadStatus::Completed => "completed",
        DownloadStatus::InProgress => {
            if record.was_paused {
                "paused"
            } else {
                "in-progress"
            }
        }
        DownloadStatus::Failed => "failed",
        DownloadStatus::Cancelled => "cancelled",
    };
    progress_bar.add_css_class(progress_status_class);

    // Box de status e metadados
    let info_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_MEDIUM)
        .build();

    // Box para status com badge colorido
    let status_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_SMALL)
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .build();

    let (status_text, status_icon_name) = match record.status {
        DownloadStatus::InProgress => {
            if record.was_paused {
                ("Pausado", Some("media-playback-pause-symbolic"))
            } else {
                ("Em progresso", Some("folder-download-symbolic"))
            }
        }
        DownloadStatus::Completed => ("Concluído", Some("emblem-ok-symbolic")),
        DownloadStatus::Failed => ("Falhou", Some("dialog-error-symbolic")),
        DownloadStatus::Cancelled => ("Cancelado", Some("process-stop-symbolic")),
    };

    // Badge colorido para status
    let status_badge = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_SMALL)
        .halign(gtk4::Align::Start)
        .css_classes(vec!["status-badge"])
        .build();

    // Determina a classe CSS baseada no status
    let badge_class = match record.status {
        DownloadStatus::Completed => "completed",
        DownloadStatus::InProgress => {
            if record.was_paused {
                "paused"
            } else {
                "in-progress"
            }
        }
        DownloadStatus::Failed => "failed",
        DownloadStatus::Cancelled => "cancelled",
    };
    status_badge.add_css_class(badge_class);

    // Ícone de status (GTK symbolic)
    if let Some(icon_name) = status_icon_name {
        let status_icon = gtk4::Image::builder()
            .icon_name(icon_name)
            .pixel_size(16)
            .build();
        status_badge.append(&status_icon);
    }

    // Texto de status
    let status_label = Label::builder()
        .halign(gtk4::Align::Start)
        .build();

    status_label.set_markup(&markup_status(status_text));

    status_badge.append(&status_label);
    status_box.append(&status_badge);

    // Box para metadados (tamanho e data) - layout horizontal minimalista
    let metadata_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_SMALL)
        .halign(gtk4::Align::End)
        .css_classes(vec!["metadata-group"])
        .build();

    // Label para tamanho do arquivo
    let size_label = Label::builder()
        .halign(gtk4::Align::End)
        .build();

    let size_text = if record.total_bytes > 0 {
        format_file_size(record.total_bytes)
    } else {
        "Desconhecido".to_string()
    };
    size_label.set_markup(&markup_metadata_primary(&size_text));

    let date_label = Label::builder()
        .halign(gtk4::Align::End)
        .css_classes(vec!["dim-label"])
        .build();

    // Data em tamanho menor e peso normal
    let date_text = format!("{}", record.date_added.format("%d/%m/%Y %H:%M"));
    date_label.set_markup(&markup_metadata_secondary(&date_text));

    metadata_box.append(&size_label);
    metadata_box.append(&date_label);

    info_box.append(&status_box);
    info_box.append(&metadata_box);

    // Box de botões - mantém estrutura consistente em todos os estados
    let buttons_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_MEDIUM)
        .halign(gtk4::Align::End)
        .build();

    // Container para botões de ação primária (à esquerda)
    let primary_actions_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_SMALL)
        .hexpand(true)
        .halign(gtk4::Align::Start)
        .build();

    // Container para botões destrutivos (à direita)
    let destructive_actions_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_SMALL)
        .halign(gtk4::Align::End)
        .build();

    // Botão de retomar (apenas para downloads em progresso)
    if record.status == DownloadStatus::InProgress {
        let resume_btn = Button::builder()
            .icon_name("media-playback-start-symbolic")
            .tooltip_text("Retomar download")
            .css_classes(vec!["suggested-action"])
            .build();

        let record_url = record.url.clone();
        let row_box_clone = row_box.clone();
        let list_box_clone = list_box.clone();
        let state_clone = state.clone();
        let content_stack_clone = content_stack.clone();
        let state_records = if let Ok(st) = state.lock() {
            st.records.clone()
        } else {
            Arc::new(Mutex::new(Vec::new()))
        };

        resume_btn.connect_clicked(move |_| {
            // Remove da UI
            if let Some(parent) = row_box_clone.parent() {
                if let Some(grandparent) = parent.parent() {
                    if let Some(lb) = grandparent.downcast_ref::<ListBox>() {
                        lb.remove(&parent);
                    }
                }
            }

            // Remove do state.records e do JSON
            if let Ok(mut records) = state_records.lock() {
                records.retain(|r| r.url != record_url);
                save_downloads(&records);
            }

            // Reinicia o download (vai usar o arquivo .part existente)
            add_download(&list_box_clone, &record_url, &state_clone, &content_stack_clone);
        });

        primary_actions_box.append(&resume_btn);
    }

    // Botão de reiniciar (apenas para downloads cancelados)
    if record.status == DownloadStatus::Cancelled {
        let restart_btn = Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Reiniciar download do zero")
            .css_classes(vec!["suggested-action"])
            .build();

        let record_url = record.url.clone();
        let record_filename = record.filename.clone();
        let row_box_clone = row_box.clone();
        let list_box_clone = list_box.clone();
        let state_clone = state.clone();
        let content_stack_clone = content_stack.clone();
        let state_records = if let Ok(st) = state.lock() {
            st.records.clone()
        } else {
            Arc::new(Mutex::new(Vec::new()))
        };

        restart_btn.connect_clicked(move |_| {
            // Remove da UI
            if let Some(parent) = row_box_clone.parent() {
                if let Some(grandparent) = parent.parent() {
                    if let Some(lb) = grandparent.downcast_ref::<ListBox>() {
                        lb.remove(&parent);
                    }
                }
            }

            // Remove do state.records e do JSON
            if let Ok(mut records) = state_records.lock() {
                records.retain(|r| r.url != record_url);
                save_downloads(&records);
            }

            // Remove arquivo parcial se existir (para começar do zero)
            let download_dir = if let Ok(app_state) = state_clone.lock() {
                if let Ok(config_guard) = app_state.config.lock() {
                    get_download_directory(&config_guard)
                } else {
                    dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
                }
            } else {
                dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
            };
            let temp_path = download_dir.join(format!("{}.part", record_filename));
            if temp_path.exists() {
                let _ = std::fs::remove_file(&temp_path);
            }

            // Inicia novo download do zero
            add_download(&list_box_clone, &record_url, &state_clone, &content_stack_clone);
        });

        primary_actions_box.append(&restart_btn);
    }

    // Botão de abrir (apenas para completados)
    if record.status == DownloadStatus::Completed {
        let open_btn = Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Abrir arquivo")
            .build();

        let file_path = record.file_path.clone();
        open_btn.connect_clicked(move |_| {
            if let Some(ref path) = file_path {
                let _ = open::that(path);
            }
        });

        primary_actions_box.append(&open_btn);

        // Botão de abrir explorador de arquivos
        let open_folder_btn = Button::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text("Abrir pasta no explorador")
            .build();

        let file_path_folder = record.file_path.clone();
        open_folder_btn.connect_clicked(move |_| {
            if let Some(ref path) = file_path_folder {
                // Abre a pasta que contém o arquivo
                if let Some(parent) = PathBuf::from(path).parent() {
                    let _ = open::that(parent);
                }
            }
        });

        primary_actions_box.append(&open_folder_btn);
    }

    // Botão de informações (sempre visível)
    let info_btn = Button::builder()
        .icon_name("info-symbolic")
        .tooltip_text("Ver estatísticas e detalhes")
        .build();

    let record_clone = record.clone();
    info_btn.connect_clicked(move |_| {
        // Cria diálogo de informações
        let dialog = libadwaita::MessageDialog::new(
            None::<&AdwApplicationWindow>,
            Some("Informações do Download"),
            None,
        );

        dialog.add_response("close", "Fechar");
        dialog.set_response_appearance("close", libadwaita::ResponseAppearance::Default);
        dialog.set_default_response(Some("close"));
        dialog.set_close_response("close");

        // Container principal
        let main_box = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(16)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(16)
            .margin_end(16)
            .build();

        // Nome do arquivo
        let filename_group = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();

        let filename_label = Label::builder()
            .label("Nome do Arquivo")
            .halign(gtk4::Align::Start)
            .css_classes(vec!["title-4"])
            .build();

        let filename_value = Label::builder()
            .label(&record_clone.filename)
            .halign(gtk4::Align::Start)
            .wrap(true)
            .selectable(true)
            .css_classes(vec!["caption"])
            .build();

        filename_group.append(&filename_label);
        filename_group.append(&filename_value);

        // URL de origem com botão de copiar
        let url_group = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();

        let url_label = Label::builder()
            .label("URL de Origem")
            .halign(gtk4::Align::Start)
            .css_classes(vec!["title-4"])
            .build();

        let url_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .build();

        let url_value = Label::builder()
            .label(&record_clone.url)
            .halign(gtk4::Align::Start)
            .hexpand(true)
            .wrap(true)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .selectable(true)
            .css_classes(vec!["caption"])
            .build();

        let copy_btn = Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Copiar URL")
            .valign(gtk4::Align::Start)
            .build();

        let record_url_copy = record_clone.url.clone();
        let dialog_clone = dialog.clone();
        copy_btn.connect_clicked(move |_| {
            if let Some(display) = gtk4::gdk::Display::default() {
                let clipboard = display.clipboard();
                clipboard.set_text(&record_url_copy);

                // Feedback visual temporário
                dialog_clone.set_body("URL copiada para a área de transferência");
            }
        });

        url_box.append(&url_value);
        url_box.append(&copy_btn);
        url_group.append(&url_label);
        url_group.append(&url_box);

        // Tamanho do arquivo
        let size_group = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();

        let size_label = Label::builder()
            .label("Tamanho")
            .halign(gtk4::Align::Start)
            .css_classes(vec!["title-4"])
            .build();

        let size_value = Label::builder()
            .label(&format_file_size(record_clone.total_bytes))
            .halign(gtk4::Align::Start)
            .css_classes(vec!["caption"])
            .build();

        size_group.append(&size_label);
        size_group.append(&size_value);

        // Status
        let status_group = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();

        let status_label = Label::builder()
            .label("Status")
            .halign(gtk4::Align::Start)
            .css_classes(vec!["title-4"])
            .build();

        let status_text = match record_clone.status {
            DownloadStatus::InProgress => if record_clone.was_paused { "Pausado" } else { "Em Progresso" },
            DownloadStatus::Completed => "Concluído",
            DownloadStatus::Failed => "Falhou",
            DownloadStatus::Cancelled => "Cancelado",
        };

        let status_value = Label::builder()
            .label(status_text)
            .halign(gtk4::Align::Start)
            .css_classes(vec!["caption"])
            .build();

        status_group.append(&status_label);
        status_group.append(&status_value);

        // Data de início
        let date_group = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();

        let date_label = Label::builder()
            .label("Data de Início")
            .halign(gtk4::Align::Start)
            .css_classes(vec!["title-4"])
            .build();

        let date_value = Label::builder()
            .label(&format!("{}", record_clone.date_added.format("%d/%m/%Y às %H:%M:%S")))
            .halign(gtk4::Align::Start)
            .css_classes(vec!["caption"])
            .build();

        date_group.append(&date_label);
        date_group.append(&date_value);

        // Data de conclusão (se completado)
        if let Some(completed_date) = record_clone.date_completed {
            let completed_group = GtkBox::builder()
                .orientation(Orientation::Vertical)
                .spacing(4)
                .build();

            let completed_label = Label::builder()
                .label("Data de Conclusão")
                .halign(gtk4::Align::Start)
                .css_classes(vec!["title-4"])
                .build();

            let completed_value = Label::builder()
                .label(&format!("{}", completed_date.format("%d/%m/%Y às %H:%M:%S")))
                .halign(gtk4::Align::Start)
                .css_classes(vec!["caption"])
                .build();

            completed_group.append(&completed_label);
            completed_group.append(&completed_value);
            main_box.append(&completed_group);
        }

        // Caminho do arquivo (se completado)
        if let Some(ref file_path) = record_clone.file_path {
            let path_group = GtkBox::builder()
                .orientation(Orientation::Vertical)
                .spacing(4)
                .build();

            let path_label = Label::builder()
                .label("Caminho do Arquivo")
                .halign(gtk4::Align::Start)
                .css_classes(vec!["title-4"])
                .build();

            let path_value = Label::builder()
                .label(file_path)
                .halign(gtk4::Align::Start)
                .wrap(true)
                .selectable(true)
                .css_classes(vec!["caption"])
                .build();

            path_group.append(&path_label);
            path_group.append(&path_value);
            main_box.append(&path_group);
        }

        main_box.append(&filename_group);
        main_box.append(&url_group);
        main_box.append(&size_group);
        main_box.append(&status_group);
        main_box.append(&date_group);

        dialog.set_extra_child(Some(&main_box));
        dialog.present();
    });

    primary_actions_box.append(&info_btn);

    // Botão de excluir
    let delete_btn = Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Remover da lista")
        .css_classes(vec!["destructive-action"])
        .build();

    let row_box_clone = row_box.clone();
    let record_url = record.url.clone();
    let state_clone = state.clone();
    let content_stack_clone = content_stack.clone();

    delete_btn.connect_clicked(move |_| {
        // Remove do state.records e do arquivo de dados PRIMEIRO
        let mut should_remove_ui = true;
        let mut is_empty = false;
        if let Ok(app_state) = state_clone.lock() {
            if let Ok(mut records) = app_state.records.lock() {
                let before_count = records.len();
                records.retain(|r| r.url != record_url);
                let after_count = records.len();

                if before_count != after_count {
                    // Salvou com sucesso, agora remove da UI
                    save_downloads(&records);
                    // Verifica se ficou vazio
                    is_empty = after_count == 0;
                } else {
                    // Não encontrou o registro, pode já ter sido removido
                    should_remove_ui = false;
                }
            }
        }

        // Remove da UI
        if should_remove_ui {
            if let Some(parent) = row_box_clone.parent() {
                if let Some(grandparent) = parent.parent() {
                    if let Some(list_box) = grandparent.downcast_ref::<ListBox>() {
                        list_box.remove(&parent);

                        // Se a lista ficou vazia, mostra o estado vazio
                        if is_empty {
                            content_stack_clone.set_visible_child_name("empty");
                        }
                    }
                }
            }
        }
    });

    destructive_actions_box.append(&delete_btn);

    // Monta a estrutura de botões de forma consistente
    buttons_box.append(&primary_actions_box);
    buttons_box.append(&destructive_actions_box);

    row_box.append(&title_label);
    row_box.append(&progress_bar);
    row_box.append(&info_box);
    row_box.append(&buttons_box);

    // Design minimalista - sem separadores entre cards
    list_box.append(&row_box);
}

fn add_download(list_box: &ListBox, url: &str, state: &Arc<Mutex<AppState>>, content_stack: &gtk4::Stack) {
    let row_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(SPACING_MEDIUM)
        .margin_top(SPACING_MEDIUM)
        .margin_bottom(SPACING_MEDIUM)
        .margin_start(SPACING_MEDIUM)
        .margin_end(SPACING_MEDIUM)
        .css_classes(vec!["download-card"])
        .build();

    let filename = sanitize_filename(url);

    // Header com título e tag de chunks paralelos
    let title_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_MEDIUM)
        .halign(gtk4::Align::Start)
        .build();

    let title_label = Label::builder()
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .css_classes(vec!["title-2"])
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();

    // Título com peso bold e tamanho large
    title_label.set_markup(&markup_title(&filename));

    // Tag de chunks paralelos (inicialmente escondida)
    let parallel_tag_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_TINY)
        .halign(gtk4::Align::Start)
        .visible(false)
        .tooltip_text("Download otimizado: arquivo baixado em múltiplas partes simultâneas")
        .build();

    let parallel_icon = gtk4::Image::builder()
        .icon_name("network-transmit-receive-symbolic")
        .pixel_size(12)
        .build();

    let parallel_label = Label::builder()
        .label("Chunks Paralelos")
        .css_classes(vec!["caption", "dim-label"])
        .build();

    parallel_tag_box.append(&parallel_icon);
    parallel_tag_box.append(&parallel_label);

    // Tag de retomando download (inicialmente escondida)
    let resume_tag_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_TINY)
        .halign(gtk4::Align::Start)
        .visible(false)
        .tooltip_text("Continuando download de onde parou")
        .build();

    let resume_icon = gtk4::Image::builder()
        .icon_name("media-skip-forward-symbolic")
        .pixel_size(12)
        .build();

    let resume_label = Label::builder()
        .label("Retomando")
        .css_classes(vec!["caption", "dim-label"])
        .build();

    resume_tag_box.append(&resume_icon);
    resume_tag_box.append(&resume_label);

    title_box.append(&title_label);
    title_box.append(&parallel_tag_box);
    title_box.append(&resume_tag_box);

    // Barra de progresso
    let progress_bar = gtk4::ProgressBar::builder()
        .hexpand(true)
        .show_text(true)
        .css_classes(vec!["download-progress", "in-progress"])
        .build();

    // Box de status e velocidade
    let info_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_MEDIUM)
        .build();

    // Box para status com badge colorido
    let status_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_SMALL)
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .build();

    // Badge colorido para status (inicialmente azul para "em progresso")
    let status_badge = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_SMALL)
        .halign(gtk4::Align::Start)
        .css_classes(vec!["status-badge", "in-progress"])
        .build();

    // Ícone de status (GTK symbolic)
    let status_icon = gtk4::Image::builder()
        .icon_name("folder-download-symbolic")
        .pixel_size(16)
        .build();

    // Texto de status
    let status_label = Label::builder()
        .halign(gtk4::Align::Start)
        .build();

    status_label.set_markup(&markup_status("Iniciando..."));

    status_badge.append(&status_icon);
    status_badge.append(&status_label);
    status_box.append(&status_badge);

    // Box para metadados (tamanho, velocidade e ETA) - layout horizontal minimalista
    let metadata_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_SMALL)
        .halign(gtk4::Align::End)
        .css_classes(vec!["metadata-group"])
        .build();

    // Label para tamanho do arquivo (inicialmente vazio, será atualizado quando disponível)
    let size_label = Label::builder()
        .halign(gtk4::Align::End)
        .build();

    size_label.set_markup(&markup_metadata_primary(""));

    let speed_label = Label::builder()
        .halign(gtk4::Align::End)
        .build();

    // Velocidade com peso semibold para destaque (inicialmente vazio)
    speed_label.set_markup(&markup_metadata_primary(""));

    let eta_label = Label::builder()
        .halign(gtk4::Align::End)
        .css_classes(vec!["dim-label"])
        .build();

    // ETA em tamanho small e peso normal (inicialmente vazio)
    eta_label.set_markup(&markup_metadata_secondary(""));

    metadata_box.append(&size_label);
    metadata_box.append(&speed_label);
    metadata_box.append(&eta_label);

    info_box.append(&status_box);
    info_box.append(&metadata_box);

    // Box de botões de ação - mantém estrutura consistente
    let buttons_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_MEDIUM)
        .halign(gtk4::Align::End)
        .build();

    // Container para botões de ação primária (à esquerda)
    let primary_actions_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_SMALL)
        .hexpand(true)
        .halign(gtk4::Align::Start)
        .build();

    // Container para botões destrutivos (à direita)
    let destructive_actions_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(SPACING_SMALL)
        .halign(gtk4::Align::End)
        .build();

    // Botão de abrir arquivo (inicialmente escondido)
    let open_btn = Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text("Abrir arquivo")
        .visible(false)
        .build();

    // Botão de abrir explorador de arquivos (inicialmente escondido)
    let open_folder_btn = Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Abrir pasta no explorador")
        .visible(false)
        .build();

    // Botão de pausa/retomar
    let pause_btn = Button::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text("Pausar")
        .build();

    // Botão de cancelar
    let cancel_btn = Button::builder()
        .icon_name("process-stop-symbolic")
        .tooltip_text("Cancelar")
        .css_classes(vec!["destructive-action"])
        .build();

    // Botão de excluir (inicialmente escondido)
    let delete_btn = Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Remover da lista")
        .visible(false)
        .css_classes(vec!["destructive-action"])
        .build();

    // Botão de informações (sempre visível)
    let info_btn = Button::builder()
        .icon_name("info-symbolic")
        .tooltip_text("Ver estatísticas e detalhes")
        .build();

    // Organiza botões de forma consistente
    primary_actions_box.append(&open_btn);
    primary_actions_box.append(&open_folder_btn);
    primary_actions_box.append(&pause_btn);
    primary_actions_box.append(&info_btn);

    destructive_actions_box.append(&cancel_btn);
    destructive_actions_box.append(&delete_btn);

    buttons_box.append(&primary_actions_box);
    buttons_box.append(&destructive_actions_box);

    row_box.append(&title_box);
    row_box.append(&progress_bar);
    row_box.append(&info_box);
    row_box.append(&buttons_box);

    // Design minimalista - sem separadores entre cards
    list_box.append(&row_box);

    // Cria o download task
    let download_task = Arc::new(Mutex::new(DownloadTask {
        paused: false,
        cancelled: false,
        file_path: None,
    }));

    // Cria registro de download inicial (em progresso e não pausado)
    let initial_record = DownloadRecord {
        url: url.to_string(),
        filename: filename.clone(),
        file_path: None,
        status: DownloadStatus::InProgress,
        date_added: Utc::now(),
        date_completed: None,
        downloaded_bytes: 0,
        total_bytes: 0,
        was_paused: false,  // Iniciando download ativo
    };

    let record_url = url.to_string();
    let state_records = if let Ok(state) = state.lock() {
        state.records.clone()
    } else {
        Arc::new(Mutex::new(Vec::new()))
    };

    // Salva registro inicial como InProgress (ou atualiza existente)
    if let Ok(mut records) = state_records.lock() {
        // Verifica se já existe um registro com essa URL
        if let Some(existing) = records.iter_mut().find(|r| r.url == initial_record.url) {
            // Atualiza o registro existente
            existing.status = DownloadStatus::InProgress;
            existing.date_completed = None;
            existing.was_paused = false;  // Retomando, então não está pausado
        } else {
            // Adiciona novo registro
            records.push(initial_record);
        }
        save_downloads(&records);
    }

    if let Ok(mut state) = state.lock() {
        state.downloads.push(download_task.clone());
    }

    // Cria channel para comunicação entre threads usando async-channel
    let (msg_tx, msg_rx) = async_channel::unbounded();

    // Inicia o download em thread separada
    let config_clone = if let Ok(app_state) = state.lock() {
        app_state.config.clone()
    } else {
        Arc::new(Mutex::new(AppConfig {
            download_directory: None,
            window_width: None,
            window_height: None,
        }))
    };
    start_download(url, &filename, msg_tx, download_task.clone(), state_records.clone(), config_clone);

    // Monitora mensagens na thread principal do GTK usando spawn_future_local
    let progress_bar_clone = progress_bar.clone();
    let status_badge_clone = status_badge.clone();
    let status_icon_clone = status_icon.clone();
    let status_label_clone = status_label.clone();
    let size_label_clone = size_label.clone();
    let speed_label_clone = speed_label.clone();
    let eta_label_clone = eta_label.clone();
    let parallel_tag_box_clone = parallel_tag_box.clone();
    let resume_tag_box_clone = resume_tag_box.clone();
    let pause_btn_clone = pause_btn.clone();
    let cancel_btn_clone = cancel_btn.clone();
    let open_btn_clone = open_btn.clone();
    let open_folder_btn_clone = open_folder_btn.clone();
    let delete_btn_clone = delete_btn.clone();
    let download_task_clone_msg = download_task.clone();
    let record_url_clone = record_url.clone();
    let state_records_clone = state_records.clone();
    let state_clone = state.clone();

    glib::spawn_future_local(async move {
        let mut last_save = std::time::Instant::now();

        while let Ok(msg) = msg_rx.recv().await {
            match msg {
                DownloadMessage::Progress(progress, status_text, speed, eta, parallel_chunks, speed_bytes) => {
                    progress_bar_clone.set_fraction(progress);
                    progress_bar_clone.set_text(Some(&format!("{:.0}%", progress * 100.0)));

                    // Armazena velocidade atual no HashMap
                    if let Ok(app_state) = state_clone.lock() {
                        if let Ok(mut speeds) = app_state.download_speeds.lock() {
                            speeds.insert(record_url_clone.clone(), speed_bytes);
                        }
                    }

                    // Atualiza tamanho do arquivo se disponível no registro
                    if let Ok(records) = state_records_clone.lock() {
                        if let Some(record) = records.iter().find(|r| r.url == record_url_clone) {
                            if record.total_bytes > 0 {
                                let size_text = format_file_size(record.total_bytes);
                                size_label_clone.set_markup(&markup_metadata_primary(&size_text));
                            }
                        }
                    }
                    
                    // Atualiza ícone de status e badge baseado no status_text
                    let (icon_name, badge_class) = if status_text.contains("Pausado") || status_text.contains("Pausar") {
                        ("media-playback-pause-symbolic", "paused")
                    } else if status_text.contains("Erro") || status_text.contains("Falha") {
                        ("dialog-error-symbolic", "failed")
                    } else {
                        ("folder-download-symbolic", "in-progress")
                    };

                    // Atualiza classe CSS do badge
                    status_badge_clone.remove_css_class("completed");
                    status_badge_clone.remove_css_class("in-progress");
                    status_badge_clone.remove_css_class("paused");
                    status_badge_clone.remove_css_class("failed");
                    status_badge_clone.remove_css_class("cancelled");
                    status_badge_clone.add_css_class(badge_class);

                    // Atualiza classe CSS da barra de progresso
                    progress_bar_clone.remove_css_class("completed");
                    progress_bar_clone.remove_css_class("in-progress");
                    progress_bar_clone.remove_css_class("paused");
                    progress_bar_clone.remove_css_class("failed");
                    progress_bar_clone.remove_css_class("cancelled");
                    progress_bar_clone.add_css_class(badge_class);

                    status_icon_clone.set_icon_name(Some(icon_name));
                    status_label_clone.set_markup(&markup_status(&status_text));
                    speed_label_clone.set_markup(&markup_metadata_primary(&speed));
                    eta_label_clone.set_markup(&markup_metadata_secondary(&eta));

                    // Mostra tag apropriada baseado no modo de download
                    if parallel_chunks {
                        // Download em chunks paralelos
                        parallel_tag_box_clone.set_visible(true);
                        resume_tag_box_clone.set_visible(false);
                    } else {
                        // Verifica se é um resume (tem bytes já baixados)
                        let is_resuming = if let Ok(records) = state_records_clone.lock() {
                            if let Some(record) = records.iter().find(|r| r.url == record_url_clone) {
                                record.downloaded_bytes > 0
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                        parallel_tag_box_clone.set_visible(false);
                        resume_tag_box_clone.set_visible(is_resuming);
                    }

                    // Atualiza registro a cada 5 segundos
                    if last_save.elapsed().as_secs() >= 5 {
                        // Verifica se está pausado neste momento
                        let is_currently_paused = if let Ok(task) = download_task_clone_msg.lock() {
                            task.paused
                        } else {
                            false
                        };

                        if let Ok(mut records) = state_records_clone.lock() {
                            if let Some(record) = records.iter_mut().find(|r| r.url == record_url_clone) {
                                record.was_paused = is_currently_paused;
                                // Atualiza downloaded_bytes baseado no progresso
                                if record.total_bytes > 0 {
                                    record.downloaded_bytes = (progress * record.total_bytes as f64) as u64;
                                }
                            }
                            save_downloads(&records);
                        }
                        last_save = std::time::Instant::now();
                    }
                }
                DownloadMessage::Complete => {
                    progress_bar_clone.set_fraction(1.0);
                    progress_bar_clone.set_text(Some("100%"));

                    // Remove velocidade do HashMap quando completa
                    if let Ok(app_state) = state_clone.lock() {
                        if let Ok(mut speeds) = app_state.download_speeds.lock() {
                            speeds.remove(&record_url_clone);
                        }
                    }

                    // Atualiza badge para completo (verde)
                    status_badge_clone.remove_css_class("in-progress");
                    status_badge_clone.remove_css_class("paused");
                    status_badge_clone.remove_css_class("failed");
                    status_badge_clone.remove_css_class("cancelled");
                    status_badge_clone.add_css_class("completed");

                    // Atualiza barra de progresso para completo (verde)
                    progress_bar_clone.remove_css_class("in-progress");
                    progress_bar_clone.remove_css_class("paused");
                    progress_bar_clone.remove_css_class("failed");
                    progress_bar_clone.remove_css_class("cancelled");
                    progress_bar_clone.add_css_class("completed");

                    // Ícone verde para completo
                    status_icon_clone.set_icon_name(Some("emblem-ok-symbolic"));
                    status_label_clone.set_markup(&markup_status("Concluído"));
                    speed_label_clone.set_markup(&markup_metadata_primary(""));
                    eta_label_clone.set_markup(&markup_metadata_secondary(""));

                    // Esconde botões de controle e mostra botões de arquivo completo
                    pause_btn_clone.set_visible(false);
                    cancel_btn_clone.set_visible(false);
                    open_btn_clone.set_visible(true);
                    open_folder_btn_clone.set_visible(true);
                    delete_btn_clone.set_visible(true);

                    // Marca como completo e obtém o caminho do arquivo
                    let file_path_str = if let Ok(task) = download_task_clone_msg.lock() {
                        task.file_path.as_ref().map(|p| p.to_string_lossy().to_string())
                    } else {
                        None
                    };

                    // Atualiza registro no arquivo
                    if let Ok(mut records) = state_records_clone.lock() {
                        if let Some(record) = records.iter_mut().find(|r| r.url == record_url_clone) {
                            record.status = DownloadStatus::Completed;
                            record.file_path = file_path_str;
                            record.date_completed = Some(Utc::now());
                            record.downloaded_bytes = record.total_bytes; // Marca como 100% completo
                        }
                        save_downloads(&records);
                    }

                    break;
                }
                DownloadMessage::Error(err) => {
                    // Remove velocidade do HashMap quando há erro
                    if let Ok(app_state) = state_clone.lock() {
                        if let Ok(mut speeds) = app_state.download_speeds.lock() {
                            speeds.remove(&record_url_clone);
                        }
                    }

                    // Atualiza ícone de status e badge baseado no tipo de erro
                    let (icon_name, badge_class, status) = if err.contains("Cancelado") {
                        ("process-stop-symbolic", "cancelled", DownloadStatus::Cancelled) // cinza
                    } else {
                        ("dialog-error-symbolic", "failed", DownloadStatus::Failed) // vermelho
                    };

                    // Atualiza classe CSS do badge
                    status_badge_clone.remove_css_class("completed");
                    status_badge_clone.remove_css_class("in-progress");
                    status_badge_clone.remove_css_class("paused");
                    status_badge_clone.remove_css_class("failed");
                    status_badge_clone.remove_css_class("cancelled");
                    status_badge_clone.add_css_class(badge_class);

                    // Atualiza classe CSS da barra de progresso
                    progress_bar_clone.remove_css_class("completed");
                    progress_bar_clone.remove_css_class("in-progress");
                    progress_bar_clone.remove_css_class("paused");
                    progress_bar_clone.remove_css_class("failed");
                    progress_bar_clone.remove_css_class("cancelled");
                    progress_bar_clone.add_css_class(badge_class);

                    status_icon_clone.set_icon_name(Some(icon_name));
                    status_label_clone.set_markup(&markup_status(&format!("Erro: {}", err)));
                    speed_label_clone.set_markup(&markup_metadata_primary(""));
                    eta_label_clone.set_markup(&markup_metadata_secondary(""));
                    pause_btn_clone.set_visible(false);
                    cancel_btn_clone.set_visible(false);
                    delete_btn_clone.set_visible(true);

                    // Atualiza registro de erro

                    if let Ok(mut records) = state_records_clone.lock() {
                        if let Some(record) = records.iter_mut().find(|r| r.url == record_url_clone) {
                            record.status = status;
                            record.date_completed = Some(Utc::now());
                        }
                        save_downloads(&records);
                    }

                    break;
                }
            }
        }
    });

    // Handler para botão de abrir arquivo
    let download_task_clone = download_task.clone();
    open_btn.connect_clicked(move |_| {
        if let Ok(task) = download_task_clone.lock() {
            if let Some(ref path) = task.file_path {
                // Abre o arquivo com o app padrão do sistema
                if let Err(e) = open::that(path) {
                    eprintln!("Erro ao abrir arquivo: {}", e);
                }
            }
        }
    });

    // Handler para botão de abrir pasta no explorador
    let download_task_clone_folder = download_task.clone();
    open_folder_btn.connect_clicked(move |_| {
        if let Ok(task) = download_task_clone_folder.lock() {
            if let Some(ref path) = task.file_path {
                // Abre a pasta que contém o arquivo no explorador
                if let Some(parent) = PathBuf::from(path).parent() {
                    if let Err(e) = open::that(parent) {
                        eprintln!("Erro ao abrir pasta: {}", e);
                    }
                }
            }
        }
    });

    // Handler para botão de informações
    let state_records_clone_info = state_records.clone();
    let record_url_clone_info = record_url.clone();
    info_btn.connect_clicked(move |_| {
        // Pega as informações do registro
        if let Ok(records) = state_records_clone_info.lock() {
            if let Some(record) = records.iter().find(|r| r.url == record_url_clone_info) {
                // Cria diálogo de informações
                let dialog = libadwaita::MessageDialog::new(
                    None::<&AdwApplicationWindow>,
                    Some("Informações do Download"),
                    None,
                );

                dialog.add_response("close", "Fechar");
                dialog.set_response_appearance("close", libadwaita::ResponseAppearance::Default);
                dialog.set_default_response(Some("close"));
                dialog.set_close_response("close");

                // Container principal
                let main_box = GtkBox::builder()
                    .orientation(Orientation::Vertical)
                    .spacing(16)
                    .margin_top(12)
                    .margin_bottom(12)
                    .margin_start(16)
                    .margin_end(16)
                    .build();

                // Nome do arquivo
                let filename_group = GtkBox::builder()
                    .orientation(Orientation::Vertical)
                    .spacing(4)
                    .build();

                let filename_label = Label::builder()
                    .label("Nome do Arquivo")
                    .halign(gtk4::Align::Start)
                    .css_classes(vec!["title-4"])
                    .build();

                let filename_value = Label::builder()
                    .label(&record.filename)
                    .halign(gtk4::Align::Start)
                    .wrap(true)
                    .selectable(true)
                    .css_classes(vec!["caption"])
                    .build();

                filename_group.append(&filename_label);
                filename_group.append(&filename_value);

                // URL de origem com botão de copiar
                let url_group = GtkBox::builder()
                    .orientation(Orientation::Vertical)
                    .spacing(4)
                    .build();

                let url_label = Label::builder()
                    .label("URL de Origem")
                    .halign(gtk4::Align::Start)
                    .css_classes(vec!["title-4"])
                    .build();

                let url_box = GtkBox::builder()
                    .orientation(Orientation::Horizontal)
                    .spacing(8)
                    .build();

                let url_value = Label::builder()
                    .label(&record.url)
                    .halign(gtk4::Align::Start)
                    .hexpand(true)
                    .wrap(true)
                    .ellipsize(gtk4::pango::EllipsizeMode::End)
                    .selectable(true)
                    .css_classes(vec!["caption"])
                    .build();

                let copy_btn = Button::builder()
                    .icon_name("edit-copy-symbolic")
                    .tooltip_text("Copiar URL")
                    .valign(gtk4::Align::Start)
                    .build();

                let record_url_copy = record.url.clone();
                let dialog_clone = dialog.clone();
                copy_btn.connect_clicked(move |_| {
                    if let Some(display) = gtk4::gdk::Display::default() {
                        let clipboard = display.clipboard();
                        clipboard.set_text(&record_url_copy);

                        // Feedback visual temporário
                        dialog_clone.set_body("URL copiada para a área de transferência");
                    }
                });

                url_box.append(&url_value);
                url_box.append(&copy_btn);
                url_group.append(&url_label);
                url_group.append(&url_box);

                // Tamanho do arquivo
                let size_group = GtkBox::builder()
                    .orientation(Orientation::Vertical)
                    .spacing(4)
                    .build();

                let size_label = Label::builder()
                    .label("Tamanho")
                    .halign(gtk4::Align::Start)
                    .css_classes(vec!["title-4"])
                    .build();

                let size_value = Label::builder()
                    .label(&format_file_size(record.total_bytes))
                    .halign(gtk4::Align::Start)
                    .css_classes(vec!["caption"])
                    .build();

                size_group.append(&size_label);
                size_group.append(&size_value);

                // Status
                let status_group = GtkBox::builder()
                    .orientation(Orientation::Vertical)
                    .spacing(4)
                    .build();

                let status_label = Label::builder()
                    .label("Status")
                    .halign(gtk4::Align::Start)
                    .css_classes(vec!["title-4"])
                    .build();

                let status_text = match record.status {
                    DownloadStatus::InProgress => if record.was_paused { "Pausado" } else { "Em Progresso" },
                    DownloadStatus::Completed => "Concluído",
                    DownloadStatus::Failed => "Falhou",
                    DownloadStatus::Cancelled => "Cancelado",
                };

                let status_value = Label::builder()
                    .label(status_text)
                    .halign(gtk4::Align::Start)
                    .css_classes(vec!["caption"])
                    .build();

                status_group.append(&status_label);
                status_group.append(&status_value);

                // Data de início
                let date_group = GtkBox::builder()
                    .orientation(Orientation::Vertical)
                    .spacing(4)
                    .build();

                let date_label = Label::builder()
                    .label("Data de Início")
                    .halign(gtk4::Align::Start)
                    .css_classes(vec!["title-4"])
                    .build();

                let date_value = Label::builder()
                    .label(&format!("{}", record.date_added.format("%d/%m/%Y às %H:%M:%S")))
                    .halign(gtk4::Align::Start)
                    .css_classes(vec!["caption"])
                    .build();

                date_group.append(&date_label);
                date_group.append(&date_value);

                // Data de conclusão (se completado)
                if let Some(completed_date) = record.date_completed {
                    let completed_group = GtkBox::builder()
                        .orientation(Orientation::Vertical)
                        .spacing(4)
                        .build();

                    let completed_label = Label::builder()
                        .label("Data de Conclusão")
                        .halign(gtk4::Align::Start)
                        .css_classes(vec!["title-4"])
                        .build();

                    let completed_value = Label::builder()
                        .label(&format!("{}", completed_date.format("%d/%m/%Y às %H:%M:%S")))
                        .halign(gtk4::Align::Start)
                        .css_classes(vec!["caption"])
                        .build();

                    completed_group.append(&completed_label);
                    completed_group.append(&completed_value);
                    main_box.append(&completed_group);
                }

                // Caminho do arquivo (se completado)
                if let Some(ref file_path) = record.file_path {
                    let path_group = GtkBox::builder()
                        .orientation(Orientation::Vertical)
                        .spacing(4)
                        .build();

                    let path_label = Label::builder()
                        .label("Caminho do Arquivo")
                        .halign(gtk4::Align::Start)
                        .css_classes(vec!["title-4"])
                        .build();

                    let path_value = Label::builder()
                        .label(file_path)
                        .halign(gtk4::Align::Start)
                        .wrap(true)
                        .selectable(true)
                        .css_classes(vec!["caption"])
                        .build();

                    path_group.append(&path_label);
                    path_group.append(&path_value);
                    main_box.append(&path_group);
                }

                main_box.append(&filename_group);
                main_box.append(&url_group);
                main_box.append(&size_group);
                main_box.append(&status_group);
                main_box.append(&date_group);

                dialog.set_extra_child(Some(&main_box));
                dialog.present();
            }
        }
    });

    // Handler para botão de pausa/retomar
    let download_task_clone = download_task.clone();
    let state_records_clone4 = state_records.clone();
    let record_url_clone4 = record_url.clone();
    let status_badge_clone_pause = status_badge.clone();
    let status_icon_clone_pause = status_icon.clone();
    let status_label_clone_pause = status_label.clone();
    let progress_bar_clone_pause = progress_bar.clone();

    pause_btn.connect_clicked(move |btn| {
        if let Ok(mut task) = download_task_clone.lock() {
            task.paused = !task.paused;
            let is_paused = task.paused;

            if is_paused {
                btn.set_icon_name("media-playback-start-symbolic");
                btn.set_tooltip_text(Some("Retomar"));

                // Atualiza UI para pausado
                status_badge_clone_pause.remove_css_class("in-progress");
                status_badge_clone_pause.remove_css_class("paused");
                status_badge_clone_pause.add_css_class("paused");
                status_icon_clone_pause.set_icon_name(Some("media-playback-pause-symbolic"));
                status_label_clone_pause.set_markup(&markup_status("Pausado"));

                // Atualiza barra de progresso para pausado
                progress_bar_clone_pause.remove_css_class("in-progress");
                progress_bar_clone_pause.remove_css_class("paused");
                progress_bar_clone_pause.add_css_class("paused");
            } else {
                btn.set_icon_name("media-playback-pause-symbolic");
                btn.set_tooltip_text(Some("Pausar"));

                // Atualiza UI para em progresso
                status_badge_clone_pause.remove_css_class("paused");
                status_badge_clone_pause.remove_css_class("in-progress");
                status_badge_clone_pause.add_css_class("in-progress");
                status_icon_clone_pause.set_icon_name(Some("folder-download-symbolic"));
                status_label_clone_pause.set_markup(&markup_status("Em progresso"));

                // Atualiza barra de progresso para em progresso
                progress_bar_clone_pause.remove_css_class("paused");
                progress_bar_clone_pause.remove_css_class("in-progress");
                progress_bar_clone_pause.add_css_class("in-progress");
            }

            // Atualiza was_paused no registro
            if let Ok(mut records) = state_records_clone4.lock() {
                if let Some(record) = records.iter_mut().find(|r| r.url == record_url_clone4) {
                    record.was_paused = is_paused;
                }
                save_downloads(&records);
            }
        }
    });

    // Handler para botão de cancelar
    let download_task_clone = download_task.clone();
    let row_box_clone_cancel = row_box.clone();
    let state_clone_cancel = state.clone();
    let record_url_clone2 = record_url.clone();
    let title_label_clone_cancel = title_label.clone();
    let progress_bar_clone_cancel = progress_bar.clone();
    let status_badge_clone_cancel = status_badge.clone();
    let status_label_clone_cancel = status_label.clone();
    let speed_label_clone_cancel = speed_label.clone();
    let eta_label_clone_cancel = eta_label.clone();
    let pause_btn_clone_cancel = pause_btn.clone();
    let cancel_btn_clone_cancel = cancel_btn.clone();
    let delete_btn_clone_cancel = delete_btn.clone();
    let buttons_box_clone_cancel = buttons_box.clone();
    let list_box_clone_cancel = list_box.clone();
    let filename_clone_cancel = filename.clone();
    let content_stack_clone_cancel = content_stack.clone();

    cancel_btn.connect_clicked(move |_| {
        // Cancela o download
        if let Ok(mut task) = download_task_clone.lock() {
            task.cancelled = true;
        }

        // Marca como cancelado no registro (mantém os metadados)
        if let Ok(app_state) = state_clone_cancel.lock() {
            if let Ok(mut records) = app_state.records.lock() {
                if let Some(record) = records.iter_mut().find(|r| r.url == record_url_clone2) {
                    record.status = DownloadStatus::Cancelled;
                    record.date_completed = Some(Utc::now());
                }
                save_downloads(&records);
            }
        }

        // Atualiza a UI para mostrar como cancelado (não remove da tela)
        // Aplica opacidade no container (melhor legibilidade)
        row_box_clone_cancel.add_css_class("cancelled-download");

        // Mantém título normal, sem strikethrough (melhor legibilidade)
        title_label_clone_cancel.set_markup(&markup_title(&filename_clone_cancel));

        // Atualiza barra de progresso para cancelado
        progress_bar_clone_cancel.remove_css_class("in-progress");
        progress_bar_clone_cancel.remove_css_class("paused");
        progress_bar_clone_cancel.remove_css_class("failed");
        progress_bar_clone_cancel.remove_css_class("completed");
        progress_bar_clone_cancel.add_css_class("cancelled");

        // Atualiza badge para cancelado (cinza)
        status_badge_clone_cancel.remove_css_class("in-progress");
        status_badge_clone_cancel.remove_css_class("paused");
        status_badge_clone_cancel.remove_css_class("failed");
        status_badge_clone_cancel.remove_css_class("completed");
        status_badge_clone_cancel.add_css_class("cancelled");

        // Atualiza status
        status_label_clone_cancel.set_markup(&markup_status("Cancelado"));
        speed_label_clone_cancel.set_markup(&markup_metadata_primary(""));
        eta_label_clone_cancel.set_markup(&markup_metadata_secondary(""));

        // Adiciona botão de reiniciar
        let restart_btn = Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Reiniciar download do zero")
            .css_classes(vec!["suggested-action"])
            .build();

        let record_url_clone_restart = record_url_clone2.clone();
        let row_box_clone_restart = row_box_clone_cancel.clone();
        let list_box_clone_restart = list_box_clone_cancel.clone();
        let state_clone_restart = state_clone_cancel.clone();
        let filename_clone_restart = filename_clone_cancel.clone();
        let content_stack_clone_restart = content_stack_clone_cancel.clone();

        restart_btn.connect_clicked(move |_| {
            // Remove da UI
            if let Some(parent) = row_box_clone_restart.parent() {
                if let Some(grandparent) = parent.parent() {
                    if let Some(lb) = grandparent.downcast_ref::<ListBox>() {
                        lb.remove(&parent);
                    }
                }
            }

            // Remove do state.records e do JSON
            if let Ok(app_state) = state_clone_restart.lock() {
                if let Ok(mut records) = app_state.records.lock() {
                    records.retain(|r| r.url != record_url_clone_restart);
                    save_downloads(&records);
                }
            }

            // Remove arquivo parcial se existir (para começar do zero)
            let download_dir = if let Ok(app_state) = state_clone_restart.lock() {
                if let Ok(config_guard) = app_state.config.lock() {
                    get_download_directory(&config_guard)
                } else {
                    dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
                }
            } else {
                dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
            };
            let temp_path = download_dir.join(format!("{}.part", filename_clone_restart));
            if temp_path.exists() {
                let _ = std::fs::remove_file(&temp_path);
            }

            // Inicia novo download do zero
            add_download(&list_box_clone_restart, &record_url_clone_restart, &state_clone_restart, &content_stack_clone_restart);
        });

        // Esconde botões de controle e mostra botão de reiniciar e excluir
        pause_btn_clone_cancel.set_visible(false);
        cancel_btn_clone_cancel.set_visible(false);
        delete_btn_clone_cancel.set_visible(true);

        // Adiciona restart_btn no container de primary actions
        if let Some(first_child) = buttons_box_clone_cancel.first_child() {
            if let Some(primary_box) = first_child.downcast_ref::<GtkBox>() {
                primary_box.prepend(&restart_btn);
            }
        }
    });

    // Handler para botão de excluir
    let row_box_clone_delete = row_box.clone();
    let state_clone_delete = state.clone();
    let record_url_clone3 = record_url.clone();
    let content_stack_clone_delete = content_stack.clone();

    delete_btn.connect_clicked(move |_| {
        // Remove do state.records e salva no arquivo PRIMEIRO
        let mut should_remove_ui = true;
        let mut is_empty = false;
        if let Ok(app_state) = state_clone_delete.lock() {
            if let Ok(mut records) = app_state.records.lock() {
                let before_count = records.len();
                records.retain(|r| r.url != record_url_clone3);
                let after_count = records.len();

                if before_count != after_count {
                    // Salvou com sucesso, agora remove da UI
                    save_downloads(&records);
                    // Verifica se ficou vazio
                    is_empty = after_count == 0;
                } else {
                    // Não encontrou o registro, pode já ter sido removido
                    should_remove_ui = false;
                }
            }
        }

        // Remove da UI
        if should_remove_ui {
            if let Some(parent) = row_box_clone_delete.parent() {
                if let Some(grandparent) = parent.parent() {
                    if let Some(list_box) = grandparent.downcast_ref::<ListBox>() {
                        list_box.remove(&parent);

                        // Se a lista ficou vazia, mostra o estado vazio
                        if is_empty {
                            content_stack_clone_delete.set_visible_child_name("empty");
                        }
                    }
                }
            }
        }
    });
}

fn start_download(
    url: &str,
    filename: &str,
    tx: async_channel::Sender<DownloadMessage>,
    download_task: Arc<Mutex<DownloadTask>>,
    state_records: Arc<Mutex<Vec<DownloadRecord>>>,
    config: Arc<Mutex<AppConfig>>,
) {
    let url = url.to_string();
    let filename = filename.to_string();

    std::thread::spawn(move || {
        // Cria runtime tokio para operações assíncronas
        let rt = tokio::runtime::Runtime::new().unwrap();

        rt.block_on(async {
            // Diretório de download usando configuração
            let download_dir = if let Ok(config_guard) = config.lock() {
                get_download_directory(&config_guard)
            } else {
                dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
            };

            let file_path = download_dir.join(&filename);
            let temp_path = download_dir.join(format!("{}.part", filename));

            // Cria client reqwest
            let client = match reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(DownloadMessage::Error(format!("Erro ao criar client: {}", e))).await;
                        return;
                    }
                };

            // Faz requisição HEAD para obter tamanho total e verificar suporte a Range (com retry)
            let (total_size, supports_range) = match retry_request(|| client.head(&url).send(), MAX_RETRIES, RETRY_DELAY_SECS).await {
                Ok(resp) => {
                    let size = resp.headers()
                        .get(reqwest::header::CONTENT_LENGTH)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(0);
                    
                    let supports = resp.headers()
                        .get(reqwest::header::ACCEPT_RANGES)
                        .and_then(|v| v.to_str().ok())
                        .map(|v| v == "bytes")
                        .unwrap_or(false);
                    
                    (size, supports)
                }
                Err(e) => {
                    let _ = tx.send(DownloadMessage::Error(format!("Erro ao obter info após {} tentativas: {}", MAX_RETRIES, e))).await;
                    return;
                }
            };

            // Atualiza total_bytes no registro quando disponível
            if total_size > 0 {
                if let Ok(mut records) = state_records.lock() {
                    if let Some(record) = records.iter_mut().find(|r| r.url == url) {
                        record.total_bytes = total_size;
                        save_downloads(&records);
                    }
                }
            }

            // Verifica se já existe arquivo .part (download pausado/interrompido)
            let is_resume = temp_path.exists();

            // Se não suporta Range, tamanho desconhecido, arquivo pequeno ou é resume, usa download sequencial
            // Motivo: download sequencial tem suporte completo a resume, download paralelo não
            if !supports_range || total_size == 0 || total_size < 1024 * 1024 || is_resume {
                // Download sequencial (código original)
                download_sequential(&client, &url, &temp_path, &file_path, total_size, &tx, &download_task, false).await;
                return;
            }

            // Download paralelo em chunks
            // Calcula número ótimo de chunks baseado no tamanho do arquivo
            // Arquivos grandes podem se beneficiar de mais chunks
            let num_chunks = calculate_optimal_chunks(total_size);
            let chunk_size = total_size / num_chunks;
            let last_chunk_size = total_size - (chunk_size * (num_chunks - 1));

            // Cria arquivo vazio
            let file_handle = match tokio::fs::File::create(&temp_path).await {
                Ok(f) => f,
                Err(e) => {
                    let _ = tx.send(DownloadMessage::Error(format!("Erro ao criar arquivo: {}", e))).await;
                    return;
                }
            };

            // Pre-aloca espaço no arquivo
            if let Err(e) = file_handle.set_len(total_size).await {
                let _ = tx.send(DownloadMessage::Error(format!("Erro ao pre-alocar arquivo: {}", e))).await;
                return;
            }
            drop(file_handle);

            // Abre arquivo para escrita paralela
            let file = match tokio::fs::OpenOptions::new()
                .write(true)
                .open(&temp_path)
                .await
            {
                Ok(f) => Arc::new(AsyncMutex::new(f)),
                Err(e) => {
                    let _ = tx.send(DownloadMessage::Error(format!("Erro ao abrir arquivo: {}", e))).await;
                    return;
                }
            };

            // Progresso compartilhado entre chunks
            let progress = Arc::new(AsyncMutex::new(vec![0u64; num_chunks as usize]));
            let last_update = Arc::new(AsyncMutex::new(Instant::now()));
            let last_downloaded = Arc::new(AsyncMutex::new(0u64));

            // Baixa cada chunk em paralelo
            let mut handles = Vec::new();

            for chunk_id in 0..num_chunks {
                let start = chunk_id * chunk_size;
                let end = if chunk_id == num_chunks - 1 {
                    start + last_chunk_size - 1
                } else {
                    start + chunk_size - 1
                };

                let url_clone = url.clone();
                let client_clone = client.clone();
                let file_clone = file.clone();
                let progress_clone = progress.clone();
                let download_task_clone = download_task.clone();
                let tx_clone = tx.clone();
                let last_update_clone = last_update.clone();
                let last_downloaded_clone = last_downloaded.clone();

                let handle = tokio::spawn(async move {
                    download_chunk(
                        &client_clone,
                        &url_clone,
                        start,
                        end,
                        chunk_id as usize,
                        file_clone,
                        progress_clone,
                        total_size,
                        &download_task_clone,
                        &tx_clone,
                        last_update_clone,
                        last_downloaded_clone,
                    ).await
                });

                handles.push(handle);
            }

            // Aguarda todos os chunks terminarem
            let mut all_success = true;
            for handle in handles {
                match handle.await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        eprintln!("Erro no chunk: {}", e);
                        all_success = false;
                    }
                    Err(e) => {
                        eprintln!("Erro ao aguardar chunk: {:?}", e);
                        all_success = false;
                    }
                }
            }

            drop(file);

            // Verifica cancelamento antes de verificar sucesso
            if let Ok(task) = download_task.lock() {
                if task.cancelled {
                    let _ = std::fs::remove_file(&temp_path);
                    let _ = tx.send(DownloadMessage::Error("Cancelado".to_string())).await;
                    return;
                }
            }

            if !all_success {
                let _ = tx.send(DownloadMessage::Error("Erro ao baixar chunks".to_string())).await;
                return;
            }

            // Download completo - renomeia arquivo
            if let Err(e) = std::fs::rename(&temp_path, &file_path) {
                let _ = tx.send(DownloadMessage::Error(format!("Erro ao finalizar: {}", e))).await;
                return;
            }

            // Salva o caminho do arquivo no download task
            if let Ok(mut task) = download_task.lock() {
                task.file_path = Some(file_path.clone());
            }

            let _ = tx.send(DownloadMessage::Complete).await;
        });
    });
}

async fn download_chunk(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    end: u64,
    chunk_id: usize,
    file: Arc<AsyncMutex<tokio::fs::File>>,
    progress: Arc<AsyncMutex<Vec<u64>>>,
    total_size: u64,
    download_task: &Arc<Mutex<DownloadTask>>,
    tx: &async_channel::Sender<DownloadMessage>,
    last_update: Arc<AsyncMutex<Instant>>,
    last_downloaded: Arc<AsyncMutex<u64>>,
) -> Result<(), String> {
    let range_header = format!("bytes={}-{}", start, end);
    
    // Tenta fazer requisição com retry automático
    let response = retry_request(|| {
        client
            .get(url)
            .header(reqwest::header::RANGE, &range_header)
            .send()
    }, MAX_RETRIES, RETRY_DELAY_SECS)
    .await
    .map_err(|e| format!("Erro na requisição após {} tentativas: {}", MAX_RETRIES, e))?;

    if !response.status().is_success() && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(format!("Status HTTP: {}", response.status()));
    }

    let mut stream = response.bytes_stream();
    let mut current_pos = start;

    while let Some(chunk_result) = stream.next().await {
        // Verifica cancelamento/pausa
        loop {
            let (cancelled, paused) = {
                if let Ok(task) = download_task.lock() {
                    (task.cancelled, task.paused)
                } else {
                    (false, false)
                }
            };

            if cancelled {
                return Err("Cancelado".to_string());
            }

            if !paused {
                break;
            }

            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        let chunk = chunk_result.map_err(|e| format!("Erro ao baixar chunk: {}", e))?;
        let chunk_len = chunk.len() as u64;

        // Escreve no arquivo na posição correta
        {
            let mut file_guard = file.lock().await;
            use tokio::io::AsyncSeekExt;
            use tokio::io::AsyncWriteExt;
            file_guard.seek(std::io::SeekFrom::Start(current_pos)).await
                .map_err(|e| format!("Erro ao posicionar arquivo: {}", e))?;
            file_guard.write_all(&chunk).await
                .map_err(|e| format!("Erro ao escrever arquivo: {}", e))?;
        }

        current_pos += chunk_len;

        // Atualiza progresso deste chunk
        {
            let mut progress_guard = progress.lock().await;
            progress_guard[chunk_id] = current_pos - start;
        }

        // Atualiza progresso total a cada 200ms
        {
            let mut last_update_guard = last_update.lock().await;
            if last_update_guard.elapsed().as_millis() >= 200 {
                let progress_guard = progress.lock().await;
                let total_downloaded: u64 = progress_guard.iter().sum();
                let progress_ratio = if total_size > 0 {
                    total_downloaded as f64 / total_size as f64
                } else {
                    0.0
                };

                let mut last_downloaded_guard = last_downloaded.lock().await;
                let elapsed_secs = last_update_guard.elapsed().as_secs_f64();
                let speed_bytes = if elapsed_secs > 0.0 {
                    (total_downloaded as f64 - *last_downloaded_guard as f64) / elapsed_secs
                } else {
                    0.0
                };
                let speed_text = format_speed(speed_bytes);

                let eta_text = if total_size > 0 && speed_bytes > 0.0 && total_downloaded < total_size {
                    let remaining_bytes = total_size - total_downloaded;
                    let eta_seconds = remaining_bytes as f64 / speed_bytes;
                    format_eta(eta_seconds)
                } else {
                    String::new()
                };

                let status = format!("{}/{}", format_bytes(total_downloaded), format_bytes(total_size));
                let _ = tx.send(DownloadMessage::Progress(progress_ratio, status, speed_text, eta_text, true, speed_bytes as u64)).await;

                *last_update_guard = Instant::now();
                *last_downloaded_guard = total_downloaded;
            }
        }
    }

    Ok(())
}

async fn download_sequential(
    client: &reqwest::Client,
    url: &str,
    temp_path: &PathBuf,
    file_path: &PathBuf,
    total_size: u64,
    tx: &async_channel::Sender<DownloadMessage>,
    download_task: &Arc<Mutex<DownloadTask>>,
    parallel_chunks: bool,
) {
    // Verifica se existe arquivo parcial para resume
    let mut downloaded = if temp_path.exists() {
        std::fs::metadata(temp_path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    // Abre ou cria arquivo para escrita
    let mut file = match if downloaded > 0 {
        OpenOptions::new().append(true).open(temp_path)
    } else {
        File::create(temp_path)
    } {
        Ok(f) => f,
        Err(e) => {
            let _ = tx.send(DownloadMessage::Error(format!("Erro ao criar arquivo: {}", e))).await;
            return;
        }
    };

    // Faz requisição com Range header para resume (com retry)
    let downloaded_bytes = downloaded;
    let response = match retry_request(|| {
        let mut req = client.get(url);
        if downloaded_bytes > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={}-", downloaded_bytes));
        }
        req.send()
    }, MAX_RETRIES, RETRY_DELAY_SECS).await {
        Ok(resp) => resp,
        Err(e) => {
            let _ = tx.send(DownloadMessage::Error(format!("Erro na requisição após {} tentativas: {}", MAX_RETRIES, e))).await;
            return;
        }
    };

    if !response.status().is_success() && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        let _ = tx.send(DownloadMessage::Error(format!("Status HTTP: {}", response.status()))).await;
        return;
    }

    // Stream de download
    let mut stream = response.bytes_stream();
    let mut last_update = Instant::now();
    let mut last_downloaded = downloaded;

    // Envia progresso inicial se estiver retomando
    if downloaded > 0 && total_size > 0 {
        let progress = downloaded as f64 / total_size as f64;
        let status = format!("{}/{}", format_bytes(downloaded), format_bytes(total_size));
        let _ = tx.send(DownloadMessage::Progress(progress, status, String::new(), String::new(), parallel_chunks, 0)).await;
    }

    while let Some(chunk_result) = stream.next().await {
        // Verifica se foi cancelado ou está pausado
        loop {
            let (cancelled, paused) = {
                if let Ok(task) = download_task.lock() {
                    (task.cancelled, task.paused)
                } else {
                    (false, false)
                }
            };

            if cancelled {
                let _ = std::fs::remove_file(temp_path);
                let _ = tx.send(DownloadMessage::Error("Cancelado".to_string())).await;
                return;
            }

            if !paused {
                break;
            }

            // Aguarda enquanto pausado
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        let chunk = match chunk_result {
            Ok(c) => c,
            Err(e) => {
                // Erro durante stream - não tenta retry aqui (já foi feito na requisição inicial)
                let _ = tx.send(DownloadMessage::Error(format!("Erro ao baixar: {}", e))).await;
                return;
            }
        };

        if let Err(e) = file.write_all(&chunk) {
            let _ = tx.send(DownloadMessage::Error(format!("Erro ao escrever: {}", e))).await;
            return;
        }

        downloaded += chunk.len() as u64;

        // Atualiza progresso a cada 200ms
        if last_update.elapsed().as_millis() >= 200 {
            let progress = if total_size > 0 {
                downloaded as f64 / total_size as f64
            } else {
                0.0
            };

            let speed_bytes = (downloaded - last_downloaded) as f64 / last_update.elapsed().as_secs_f64();
            let speed_text = format_speed(speed_bytes);

            // Calcula ETA (tempo restante estimado)
            let eta_text = if total_size > 0 && speed_bytes > 0.0 && downloaded < total_size {
                let remaining_bytes = total_size - downloaded;
                let eta_seconds = remaining_bytes as f64 / speed_bytes;
                format_eta(eta_seconds)
            } else {
                String::new()
            };

            let status = format!("{}/{}", format_bytes(downloaded), format_bytes(total_size));

            let _ = tx.send(DownloadMessage::Progress(progress, status, speed_text, eta_text, parallel_chunks, speed_bytes as u64)).await;

            last_update = Instant::now();
            last_downloaded = downloaded;
        }
    }

    // Download completo - renomeia arquivo
    drop(file);
    if let Err(e) = std::fs::rename(temp_path, file_path) {
        let _ = tx.send(DownloadMessage::Error(format!("Erro ao finalizar: {}", e))).await;
        return;
    }

    // Salva o caminho do arquivo no download task
    if let Ok(mut task) = download_task.lock() {
        task.file_path = Some(file_path.clone());
    }

    let _ = tx.send(DownloadMessage::Complete).await;
}

fn calculate_optimal_chunks(file_size: u64) -> u64 {
    // Calcula número ótimo de chunks baseado no tamanho do arquivo
    // - Arquivos pequenos (< 10MB): 2 chunks
    // - Arquivos médios (10MB - 100MB): 4 chunks (padrão)
    // - Arquivos grandes (100MB - 1GB): 6 chunks
    // - Arquivos muito grandes (> 1GB): 8 chunks
    // Garante que cada chunk tenha pelo menos MIN_CHUNK_SIZE
    
    let max_chunks_by_size = file_size / MIN_CHUNK_SIZE;
    let suggested_chunks = if file_size < 10 * 1024 * 1024 {
        2
    } else if file_size < 100 * 1024 * 1024 {
        DEFAULT_NUM_CHUNKS
    } else if file_size < 1024 * 1024 * 1024 {
        6
    } else {
        8
    };
    
    // Usa o menor valor entre o sugerido e o máximo possível
    suggested_chunks.min(max_chunks_by_size.max(1))
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

fn format_speed(bytes_per_sec: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;

    if bytes_per_sec >= MB {
        format!("{:.2} MB/s", bytes_per_sec / MB)
    } else if bytes_per_sec >= KB {
        format!("{:.2} KB/s", bytes_per_sec / KB)
    } else {
        format!("{:.0} B/s", bytes_per_sec)
    }
}

fn format_eta(seconds: f64) -> String {
    if seconds.is_infinite() || seconds.is_nan() || seconds < 0.0 {
        return String::new();
    }

    let total_seconds = seconds as u64;

    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let secs = total_seconds % 60;

    if hours > 0 {
        format!("{}h {}min", hours, minutes)
    } else if minutes > 0 {
        format!("{}min {}s", minutes, secs)
    } else if secs > 0 {
        format!("{}s", secs)
    } else {
        "< 1s".to_string()
    }
}

// Funções auxiliares para markup Pango padronizado
fn markup_title(text: &str) -> String {
    format!(
        "<span weight='bold' size='large'>{}</span>",
        glib::markup_escape_text(text)
    )
}

fn markup_title_strikethrough(text: &str) -> String {
    format!(
        "<s><span weight='bold' size='large'>{}</span></s>",
        glib::markup_escape_text(text)
    )
}

fn markup_status(text: &str) -> String {
    format!(
        "<span weight='600'>{}</span>",
        glib::markup_escape_text(text)
    )
}

// Removida: markup_status_icon - agora usa gtk4::Image com ícones simbólicos

fn markup_metadata_primary(text: &str) -> String {
    format!(
        "<span weight='600'>{}</span>",
        glib::markup_escape_text(text)
    )
}

fn markup_metadata_secondary(text: &str) -> String {
    format!(
        "<span size='small' weight='normal'>{}</span>",
        glib::markup_escape_text(text)
    )
}

// Função auxiliar para verificar se um erro é recuperável (timeout, conexão)
fn is_recoverable_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

// Função auxiliar para fazer retry automático em requisições
async fn retry_request<F, Fut, T>(request_fn: F, max_retries: u32, delay_secs: u64) -> Result<T, reqwest::Error>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, reqwest::Error>>,
{
    let mut last_error = None;
    
    for attempt in 0..max_retries {
        match request_fn().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                // Verifica se é erro recuperável
                if !is_recoverable_error(&e) {
                    // Erro não recuperável (404, 403, etc.) - não tenta novamente
                    return Err(e);
                }
                
                last_error = Some(e);
                
                // Se não é a última tentativa, aguarda antes de tentar novamente
                if attempt < max_retries - 1 {
                    // Delay exponencial: 2s, 4s, 8s...
                    let delay = delay_secs * (1 << attempt);
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                }
            }
        }
    }
    
    // Retorna o último erro se todas as tentativas falharam
    // Se não houver erro anterior (não deveria acontecer), tenta fazer uma última requisição
    match last_error {
        Some(e) => Err(e),
        None => {
            // Faz uma última tentativa
            request_fn().await
        }
    }
}

