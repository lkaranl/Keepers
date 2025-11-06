use gtk4::{prelude::*, Application, Box as GtkBox, Button, Entry, Label, ListBox, Orientation, ScrolledWindow, MenuButton, PopoverMenu, CssProvider, FileChooserNative, FileChooserAction};
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
    Progress(f64, String, String, String, bool), // (progress, status_text, speed, eta, parallel_chunks)
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
    
    menu.append(Some("Sair"), Some("app.quit"));

    let popover = PopoverMenu::from_model(Some(&menu));
    menu_button.set_popover(Some(&popover));

    header.pack_end(&menu_button);

    // Ação para configurações de pasta de downloads
    let config_action = gio::SimpleAction::new("config-downloads", None);
    let window_clone_config = window.clone();
    let state_clone_config = state.clone();
    config_action.connect_activate(move |_, _| {
        let config_window = window_clone_config.clone();
        let config_state = state_clone_config.clone();
        
        // Cria diálogo de seleção de pasta
        let dialog = FileChooserNative::builder()
            .title("Selecionar Pasta de Downloads")
            .action(FileChooserAction::SelectFolder)
            .transient_for(&config_window)
            .modal(true)
            .build();
        
        dialog.connect_response(move |dialog, response| {
            if response == gtk4::ResponseType::Accept {
                if let Some(file) = dialog.file() {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().to_string();

                        // Atualiza configuração
                        if let Ok(app_state) = config_state.lock() {
                            if let Ok(mut config) = app_state.config.lock() {
                                config.download_directory = Some(path_str.clone());
                                save_config(&config);
                            }
                        }
                    }
                }
            }
            // FileChooserNative se auto-gerencia, não precisa de destroy() manual
        });
        
        dialog.show();
    });
    app.add_action(&config_action);

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

    scrolled.set_child(Some(&list_box));

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
                add_completed_download(&list_box, &record, &state);
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
            add_download(&list_box, &url, &state);
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
                .body("Cole o link do arquivo que você deseja baixar:")
                .build();

            // Adiciona botões de ação
            dialog.add_response("cancel", "Cancelar");
            dialog.add_response("download", "Baixar");
            dialog.set_response_appearance("download", ResponseAppearance::Suggested);
            dialog.set_close_response("cancel");
            
            // Desabilita botão "Baixar" inicialmente (antes de conectar signals)
            dialog.set_response_enabled("download", false);

            // Campo de entrada de URL dentro da modal
            let url_entry = Entry::builder()
                .placeholder_text("https://exemplo.com/arquivo.zip")
                .activates_default(false)
                .build();

            // Container para o entry com margens adequadas
            let entry_box = GtkBox::builder()
                .orientation(Orientation::Vertical)
                .spacing(8)
                .margin_top(8)
                .margin_bottom(8)
                .margin_start(8)
                .margin_end(8)
                .build();

            entry_box.append(&url_entry);
            dialog.set_extra_child(Some(&entry_box));

            // Conecta validação em tempo real
            let dialog_clone = dialog.clone();
            url_entry.connect_changed(move |entry| {
                let url = entry.text().to_string().trim().to_string();
                // Remove classe de erro quando usuário começar a digitar
                entry.remove_css_class("error");
                // Valida se tem conteúdo e começa com http:// ou https://
                let is_valid = !url.is_empty() && (url.starts_with("http://") || url.starts_with("https://"));
                dialog_clone.set_response_enabled("download", is_valid);
                // Define resposta padrão apenas se válido (para permitir Enter)
                if is_valid {
                    dialog_clone.set_default_response(Some("download"));
                    // Reativa o activates_default quando válido
                    entry.set_activates_default(true);
                } else {
                    dialog_clone.set_default_response(None);
                    entry.set_activates_default(false);
                }
            });

            // Clones necessários para o callback
            let list_box_dialog = list_box_clone.clone();
            let content_stack_dialog = content_stack_clone.clone();
            let state_dialog = state_clone.clone();

            // Conecta resposta da modal
            dialog.connect_response(None, move |dialog, response| {
                if response == "download" {
                    let url = url_entry.text().to_string().trim().to_string();
                    // Valida se tem conteúdo e começa com http:// ou https://
                    if !url.is_empty() && (url.starts_with("http://") || url.starts_with("https://")) {
                        add_download(&list_box_dialog, &url, &state_dialog);
                        content_stack_dialog.set_visible_child_name("list");
                        dialog.close();
                    } else {
                        // Se não for válido, não fecha a modal e mostra feedback visual
                        url_entry.add_css_class("error");
                    }
                } else {
                    dialog.close();
                }
            });

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

    window.set_content(Some(&main_box));
    
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

        /* Progress bar minimalista */
        .download-progress {{
            min-height: 4px;
            border-radius: 0;
        }}
        
        .download-progress trough {{
            background-color: alpha(currentColor, 0.08);
            border-radius: 0;
        }}
        
        .download-progress progress {{
            background-color: {};
            color: {};
            border-radius: 0;
        }}
        
        .download-progress.cancelled-progress progress {{
            background-color: {};
            color: {};
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
    ",
        RADIUS_LARGE,
        COLOR_INFO,
        COLOR_INFO,
        COLOR_NEUTRAL,
        COLOR_NEUTRAL,
        COLOR_SUCCESS,
        COLOR_INFO,
        COLOR_WARNING,
        COLOR_ERROR,
        COLOR_NEUTRAL,
        OPACITY_DIM_TEXT,
        OPACITY_CANCELLED
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

fn add_completed_download(list_box: &ListBox, record: &DownloadRecord, state: &Arc<Mutex<AppState>>) {
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

    // Se cancelado, aplica estilo especial na barra de progresso
    if is_cancelled {
        progress_bar.add_css_class("cancelled-progress");
    }

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
            add_download(&list_box_clone, &record_url, &state_clone);
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
            add_download(&list_box_clone, &record_url, &state_clone);
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

    // Botão de excluir
    let delete_btn = Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Remover da lista")
        .css_classes(vec!["destructive-action"])
        .build();

    let row_box_clone = row_box.clone();
    let record_url = record.url.clone();
    let state_clone = state.clone();

    delete_btn.connect_clicked(move |_| {
        // Remove do state.records e do arquivo de dados PRIMEIRO
        let mut should_remove_ui = true;
        if let Ok(app_state) = state_clone.lock() {
            if let Ok(mut records) = app_state.records.lock() {
                let before_count = records.len();
                records.retain(|r| r.url != record_url);
                let after_count = records.len();
                
                if before_count != after_count {
                    // Salvou com sucesso, agora remove da UI
                    save_downloads(&records);
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

fn add_download(list_box: &ListBox, url: &str, state: &Arc<Mutex<AppState>>) {
    let row_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(SPACING_MEDIUM)
        .margin_top(SPACING_MEDIUM)
        .margin_bottom(SPACING_MEDIUM)
        .margin_start(SPACING_MEDIUM)
        .margin_end(SPACING_MEDIUM)
        .css_classes(vec!["download-card"])
        .build();

    let filename = url.split('/').last().unwrap_or("download").to_string();

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

    title_box.append(&title_label);
    title_box.append(&parallel_tag_box);

    // Barra de progresso
    let progress_bar = gtk4::ProgressBar::builder()
        .hexpand(true)
        .show_text(true)
        .css_classes(vec!["download-progress"])
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

    // Organiza botões de forma consistente
    primary_actions_box.append(&open_btn);
    primary_actions_box.append(&open_folder_btn);
    primary_actions_box.append(&pause_btn);

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
    let pause_btn_clone = pause_btn.clone();
    let cancel_btn_clone = cancel_btn.clone();
    let open_btn_clone = open_btn.clone();
    let open_folder_btn_clone = open_folder_btn.clone();
    let delete_btn_clone = delete_btn.clone();
    let download_task_clone_msg = download_task.clone();
    let record_url_clone = record_url.clone();
    let state_records_clone = state_records.clone();

    glib::spawn_future_local(async move {
        let mut last_save = std::time::Instant::now();

        while let Ok(msg) = msg_rx.recv().await {
            match msg {
                DownloadMessage::Progress(progress, status_text, speed, eta, parallel_chunks) => {
                    progress_bar_clone.set_fraction(progress);
                    progress_bar_clone.set_text(Some(&format!("{:.0}%", progress * 100.0)));
                    
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

                    status_icon_clone.set_icon_name(Some(icon_name));
                    status_label_clone.set_markup(&markup_status(&status_text));
                    speed_label_clone.set_markup(&markup_metadata_primary(&speed));
                    eta_label_clone.set_markup(&markup_metadata_secondary(&eta));
                    parallel_tag_box_clone.set_visible(parallel_chunks);

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
                            }
                            save_downloads(&records);
                        }
                        last_save = std::time::Instant::now();
                    }
                }
                DownloadMessage::Complete => {
                    progress_bar_clone.set_fraction(1.0);
                    progress_bar_clone.set_text(Some("100%"));
                    
                    // Atualiza badge para completo (verde)
                    status_badge_clone.remove_css_class("in-progress");
                    status_badge_clone.remove_css_class("paused");
                    status_badge_clone.remove_css_class("failed");
                    status_badge_clone.remove_css_class("cancelled");
                    status_badge_clone.add_css_class("completed");

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
                        }
                        save_downloads(&records);
                    }

                    break;
                }
                DownloadMessage::Error(err) => {
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

    // Handler para botão de pausa/retomar
    let download_task_clone = download_task.clone();
    let state_records_clone4 = state_records.clone();
    let record_url_clone4 = record_url.clone();
    let status_badge_clone_pause = status_badge.clone();
    let status_icon_clone_pause = status_icon.clone();
    let status_label_clone_pause = status_label.clone();

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
            } else {
                btn.set_icon_name("media-playback-pause-symbolic");
                btn.set_tooltip_text(Some("Pausar"));

                // Atualiza UI para em progresso
                status_badge_clone_pause.remove_css_class("paused");
                status_badge_clone_pause.remove_css_class("in-progress");
                status_badge_clone_pause.add_css_class("in-progress");
                status_icon_clone_pause.set_icon_name(Some("folder-download-symbolic"));
                status_label_clone_pause.set_markup(&markup_status("Em progresso"));
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

        // Atualiza barra de progresso
        progress_bar_clone_cancel.add_css_class("cancelled-progress");

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
            add_download(&list_box_clone_restart, &record_url_clone_restart, &state_clone_restart);
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

    delete_btn.connect_clicked(move |_| {
        // Remove do state.records e salva no arquivo PRIMEIRO
        let mut should_remove_ui = true;
        if let Ok(app_state) = state_clone_delete.lock() {
            if let Ok(mut records) = app_state.records.lock() {
                let before_count = records.len();
                records.retain(|r| r.url != record_url_clone3);
                let after_count = records.len();
                
                if before_count != after_count {
                    // Salvou com sucesso, agora remove da UI
                    save_downloads(&records);
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

            // Se não suporta Range ou tamanho desconhecido, usa download sequencial
            if !supports_range || total_size == 0 || total_size < 1024 * 1024 {
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

                let eta_text = if total_size > 0 && speed_bytes > 0.0 {
                    let remaining_bytes = total_size - total_downloaded;
                    let eta_seconds = remaining_bytes as f64 / speed_bytes;
                    format_eta(eta_seconds)
                } else {
                    String::new()
                };

                let status = format!("{}/{}", format_bytes(total_downloaded), format_bytes(total_size));
                let _ = tx.send(DownloadMessage::Progress(progress_ratio, status, speed_text, eta_text, true)).await;

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
            let eta_text = if total_size > 0 && speed_bytes > 0.0 {
                let remaining_bytes = total_size - downloaded;
                let eta_seconds = remaining_bytes as f64 / speed_bytes;
                format_eta(eta_seconds)
            } else {
                String::new()
            };

            let status = format!("{}/{}", format_bytes(downloaded), format_bytes(total_size));

            let _ = tx.send(DownloadMessage::Progress(progress, status, speed_text, eta_text, parallel_chunks)).await;

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

