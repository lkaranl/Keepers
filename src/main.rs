use gtk4::{prelude::*, Application, Box as GtkBox, Button, Entry, Label, ListBox, Orientation, ScrolledWindow};
use gtk4::glib;
use libadwaita::{prelude::*, ApplicationWindow as AdwApplicationWindow, HeaderBar, StatusPage, StyleManager};
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

struct AppState {
    downloads: Vec<Arc<Mutex<DownloadTask>>>,
    records: Arc<Mutex<Vec<DownloadRecord>>>,
}

fn main() {
    let app = Application::builder()
        .application_id(APP_ID)
        .build();

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

fn save_downloads(records: &[DownloadRecord]) {
    let file_path = get_data_file_path();

    if let Ok(json) = serde_json::to_string_pretty(records) {
        let _ = std::fs::write(&file_path, json);
    }
}

fn build_ui(app: &Application) {
    let style_manager = StyleManager::default();
    style_manager.set_color_scheme(libadwaita::ColorScheme::ForceDark);

    // Carrega downloads salvos
    let saved_records = load_downloads();

    let state = Arc::new(Mutex::new(AppState {
        downloads: Vec::new(),
        records: Arc::new(Mutex::new(saved_records.clone())),
    }));

    let window = AdwApplicationWindow::builder()
        .application(app)
        .title("DownStream")
        .default_width(700)
        .default_height(500)
        .build();

    let main_box = GtkBox::new(Orientation::Vertical, 0);

    let header = HeaderBar::new();
    main_box.append(&header);

    let input_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();

    let url_entry = Entry::builder()
        .placeholder_text("Cole o link do arquivo aqui...")
        .hexpand(true)
        .build();

    let download_btn = Button::builder()
        .label("Baixar")
        .css_classes(vec!["suggested-action"])
        .build();

    input_box.append(&url_entry);
    input_box.append(&download_btn);

    let scrolled = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .margin_start(24)
        .margin_end(24)
        .margin_bottom(24)
        .build();

    let list_box = ListBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .css_classes(vec!["boxed-list"])
        .build();

    scrolled.set_child(Some(&list_box));

    let empty_state = StatusPage::builder()
        .icon_name("folder-download-symbolic")
        .title("Nenhum download")
        .description("Adicione um link acima para começar")
        .vexpand(true)
        .build();

    let content_stack = gtk4::Stack::new();
    content_stack.add_named(&empty_state, Some("empty"));
    content_stack.add_named(&scrolled, Some("list"));
    content_stack.set_visible_child_name("empty");

    main_box.append(&input_box);
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
                // Caso contrário, mostra como download completo/pausado/falhado
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

    let list_box_clone = list_box.clone();
    let url_entry_clone = url_entry.clone();
    let content_stack_clone = content_stack.clone();
    let state_clone = state.clone();

    download_btn.connect_clicked(move |_| {
        let url = url_entry_clone.text().to_string();
        if !url.is_empty() {
            add_download(&list_box_clone, &url, &state_clone);
            content_stack_clone.set_visible_child_name("list");
            url_entry_clone.set_text("");
        }
    });

    let download_btn_clone = download_btn.clone();
    url_entry.connect_activate(move |_| {
        download_btn_clone.emit_clicked();
    });

    window.set_content(Some(&main_box));
    window.present();
}

fn add_completed_download(list_box: &ListBox, record: &DownloadRecord, state: &Arc<Mutex<AppState>>) {
    let row_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    // Header com título
    let title_label = Label::builder()
        .label(&record.filename)
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .css_classes(vec!["title-3"])
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .build();

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
        .build();

    // Box de status
    let info_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(12)
        .build();

    let status_text = match record.status {
        DownloadStatus::InProgress => {
            if record.was_paused {
                "Pausado"
            } else {
                "Retomando..."
            }
        }
        DownloadStatus::Completed => "Concluído",
        DownloadStatus::Failed => "Falhou",
        DownloadStatus::Cancelled => "Cancelado",
    };

    let status_label = Label::builder()
        .label(status_text)
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .css_classes(vec!["caption", "dim-label"])
        .build();

    let date_label = Label::builder()
        .label(&format!("{}", record.date_added.format("%d/%m/%Y %H:%M")))
        .halign(gtk4::Align::End)
        .css_classes(vec!["caption", "dim-label"])
        .build();

    info_box.append(&status_label);
    info_box.append(&date_label);

    // Box de botões
    let buttons_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
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

        buttons_box.append(&resume_btn);
    }

    // Botão de abrir (apenas para completados)
    if record.status == DownloadStatus::Completed {
        let open_btn = Button::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text("Abrir arquivo")
            .build();

        let file_path = record.file_path.clone();
        open_btn.connect_clicked(move |_| {
            if let Some(ref path) = file_path {
                let _ = open::that(path);
            }
        });

        buttons_box.append(&open_btn);
    }

    // Botão de excluir
    let delete_btn = Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Remover da lista")
        .css_classes(vec!["destructive-action"])
        .build();

    let row_box_clone = row_box.clone();
    let record_url = record.url.clone();
    let state_records_delete = if let Ok(st) = state.lock() {
        st.records.clone()
    } else {
        Arc::new(Mutex::new(Vec::new()))
    };

    delete_btn.connect_clicked(move |_| {
        // Remove da UI
        if let Some(parent) = row_box_clone.parent() {
            if let Some(grandparent) = parent.parent() {
                if let Some(list_box) = grandparent.downcast_ref::<ListBox>() {
                    list_box.remove(&parent);
                }
            }
        }

        // Remove do state.records e do arquivo de dados
        if let Ok(mut records) = state_records_delete.lock() {
            records.retain(|r| r.url != record_url);
            save_downloads(&records);
        }
    });

    buttons_box.append(&delete_btn);

    row_box.append(&title_label);
    row_box.append(&progress_bar);
    row_box.append(&info_box);
    row_box.append(&buttons_box);

    list_box.append(&row_box);
}

fn add_download(list_box: &ListBox, url: &str, state: &Arc<Mutex<AppState>>) {
    let row_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    let filename = url.split('/').last().unwrap_or("download").to_string();

    // Header com título e tag de chunks paralelos
    let title_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::Start)
        .build();

    let title_label = Label::builder()
        .label(&filename)
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .css_classes(vec!["title-3"])
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .build();

    // Tag de chunks paralelos (inicialmente escondida)
    let parallel_tag = Label::builder()
        .label("⚡ Chunks Paralelos")
        .halign(gtk4::Align::Start)
        .css_classes(vec!["caption", "dim-label"])
        .visible(false)
        .tooltip_text("Download otimizado: arquivo baixado em múltiplas partes simultâneas")
        .build();

    // Adiciona estilo para destacar a tag
    let ctx = parallel_tag.style_context();
    ctx.add_class("tag");

    title_box.append(&title_label);
    title_box.append(&parallel_tag);

    // Barra de progresso
    let progress_bar = gtk4::ProgressBar::builder()
        .hexpand(true)
        .show_text(true)
        .build();

    // Box de status e velocidade
    let info_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(12)
        .build();

    let status_label = Label::builder()
        .label("Iniciando...")
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .css_classes(vec!["caption", "dim-label"])
        .build();

    let speed_eta_box = GtkBox::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .halign(gtk4::Align::End)
        .build();

    let speed_label = Label::builder()
        .label("")
        .halign(gtk4::Align::End)
        .css_classes(vec!["caption"])
        .build();

    let eta_label = Label::builder()
        .label("")
        .halign(gtk4::Align::End)
        .css_classes(vec!["caption", "dim-label"])
        .build();

    speed_eta_box.append(&speed_label);
    speed_eta_box.append(&eta_label);

    info_box.append(&status_label);
    info_box.append(&speed_eta_box);

    // Box de botões de ação
    let buttons_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::End)
        .build();

    // Botão de abrir arquivo (inicialmente escondido)
    let open_btn = Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Abrir arquivo")
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

    buttons_box.append(&open_btn);
    buttons_box.append(&pause_btn);
    buttons_box.append(&cancel_btn);
    buttons_box.append(&delete_btn);

    row_box.append(&title_box);
    row_box.append(&progress_bar);
    row_box.append(&info_box);
    row_box.append(&buttons_box);

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
    start_download(url, &filename, msg_tx, download_task.clone());

    // Monitora mensagens na thread principal do GTK usando spawn_future_local
    let progress_bar_clone = progress_bar.clone();
    let status_label_clone = status_label.clone();
    let speed_label_clone = speed_label.clone();
    let eta_label_clone = eta_label.clone();
    let parallel_tag_clone = parallel_tag.clone();
    let pause_btn_clone = pause_btn.clone();
    let cancel_btn_clone = cancel_btn.clone();
    let open_btn_clone = open_btn.clone();
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
                    status_label_clone.set_text(&status_text);
                    speed_label_clone.set_text(&speed);
                    eta_label_clone.set_text(&eta);
                    parallel_tag_clone.set_visible(parallel_chunks);

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
                    status_label_clone.set_text("Concluído");
                    speed_label_clone.set_text("✓");
                    eta_label_clone.set_text("");

                    // Esconde botões de controle e mostra botões de arquivo completo
                    pause_btn_clone.set_visible(false);
                    cancel_btn_clone.set_visible(false);
                    open_btn_clone.set_visible(true);
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
                    status_label_clone.set_text(&format!("Erro: {}", err));
                    speed_label_clone.set_text("");
                    eta_label_clone.set_text("");
                    pause_btn_clone.set_visible(false);
                    cancel_btn_clone.set_visible(false);
                    delete_btn_clone.set_visible(true);

                    // Atualiza registro de erro
                    let status = if err.contains("Cancelado") {
                        DownloadStatus::Cancelled
                    } else {
                        DownloadStatus::Failed
                    };

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

    // Handler para botão de pausa/retomar
    let download_task_clone = download_task.clone();
    let state_records_clone4 = state_records.clone();
    let record_url_clone4 = record_url.clone();

    pause_btn.connect_clicked(move |btn| {
        if let Ok(mut task) = download_task_clone.lock() {
            task.paused = !task.paused;
            let is_paused = task.paused;

            if is_paused {
                btn.set_icon_name("media-playback-start-symbolic");
                btn.set_tooltip_text(Some("Retomar"));
            } else {
                btn.set_icon_name("media-playback-pause-symbolic");
                btn.set_tooltip_text(Some("Pausar"));
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
    let state_records_clone2 = state_records.clone();
    let record_url_clone2 = record_url.clone();

    cancel_btn.connect_clicked(move |_| {
        if let Ok(mut task) = download_task_clone.lock() {
            task.cancelled = true;
        }

        // Marca registro como cancelado
        if let Ok(mut records) = state_records_clone2.lock() {
            if let Some(record) = records.iter_mut().find(|r| r.url == record_url_clone2) {
                record.status = DownloadStatus::Cancelled;
                record.date_completed = Some(Utc::now());
            }
            save_downloads(&records);
        }

        // Remove a ListBoxRow parent
        if let Some(parent) = row_box_clone_cancel.parent() {
            if let Some(grandparent) = parent.parent() {
                if let Some(list_box) = grandparent.downcast_ref::<ListBox>() {
                    list_box.remove(&parent);
                }
            }
        }
    });

    // Handler para botão de excluir
    let row_box_clone_delete = row_box.clone();
    let state_records_clone3 = state_records.clone();
    let record_url_clone3 = record_url.clone();

    delete_btn.connect_clicked(move |_| {
        // Remove do state.records
        if let Ok(mut records) = state_records_clone3.lock() {
            records.retain(|r| r.url != record_url_clone3);
            save_downloads(&records);
        }

        // Remove a ListBoxRow parent
        if let Some(parent) = row_box_clone_delete.parent() {
            if let Some(grandparent) = parent.parent() {
                if let Some(list_box) = grandparent.downcast_ref::<ListBox>() {
                    list_box.remove(&parent);
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
) {
    let url = url.to_string();
    let filename = filename.to_string();

    std::thread::spawn(move || {
        // Cria runtime tokio para operações assíncronas
        let rt = tokio::runtime::Runtime::new().unwrap();

        rt.block_on(async {
            // Diretório de download
            let download_dir = std::env::current_dir().unwrap_or_else(|_| {
                dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
            });

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

            // Faz requisição HEAD para obter tamanho total e verificar suporte a Range
            let (total_size, supports_range) = match client.head(&url).send().await {
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
                    let _ = tx.send(DownloadMessage::Error(format!("Erro ao obter info: {}", e))).await;
                    return;
                }
            };

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
                let start = chunk_id * chunk_size;
                let end = if chunk_id == NUM_CHUNKS - 1 {
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
    
    let response = client
        .get(url)
        .header(reqwest::header::RANGE, &range_header)
        .send()
        .await
        .map_err(|e| format!("Erro na requisição: {}", e))?;

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

    // Faz requisição com Range header para resume
    let mut request = client.get(url);
    if downloaded > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={}-", downloaded));
    }

    let response = match request.send().await {
        Ok(resp) => resp,
        Err(e) => {
            let _ = tx.send(DownloadMessage::Error(format!("Erro na requisição: {}", e))).await;
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
