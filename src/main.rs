use gtk4::{prelude::*, Application, Box as GtkBox, Button, Entry, Label, ListBox, Orientation, ScrolledWindow};
use gtk4::glib;
use libadwaita::{prelude::*, ApplicationWindow as AdwApplicationWindow, HeaderBar, StatusPage, StyleManager};
use std::sync::{Arc, Mutex};
use std::path::PathBuf;
use std::time::Instant;
use futures_util::StreamExt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use async_channel;

const APP_ID: &str = "com.downstream.app";

#[derive(Clone, Debug)]
enum DownloadMessage {
    Progress(f64, String, String), // (progress, status_text, speed)
    Complete,
    Error(String),
}

#[derive(Debug)]
struct DownloadTask {
    paused: bool,
    cancelled: bool,
    completed: bool,
    file_path: Option<PathBuf>,
}

struct AppState {
    downloads: Vec<Arc<Mutex<DownloadTask>>>,
}

fn main() {
    let app = Application::builder()
        .application_id(APP_ID)
        .build();

    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &Application) {
    let style_manager = StyleManager::default();
    style_manager.set_color_scheme(libadwaita::ColorScheme::ForceDark);

    let state = Arc::new(Mutex::new(AppState {
        downloads: Vec::new(),
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

    // Header com título
    let title_label = Label::builder()
        .label(&filename)
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .css_classes(vec!["title-3"])
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .build();

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

    let speed_label = Label::builder()
        .label("")
        .halign(gtk4::Align::End)
        .css_classes(vec!["caption"])
        .build();

    info_box.append(&status_label);
    info_box.append(&speed_label);

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

    row_box.append(&title_label);
    row_box.append(&progress_bar);
    row_box.append(&info_box);
    row_box.append(&buttons_box);

    list_box.append(&row_box);

    // Cria o download task
    let download_task = Arc::new(Mutex::new(DownloadTask {
        paused: false,
        cancelled: false,
        completed: false,
        file_path: None,
    }));

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
    let pause_btn_clone = pause_btn.clone();
    let cancel_btn_clone = cancel_btn.clone();
    let open_btn_clone = open_btn.clone();
    let delete_btn_clone = delete_btn.clone();
    let download_task_clone_msg = download_task.clone();

    glib::spawn_future_local(async move {
        while let Ok(msg) = msg_rx.recv().await {
            match msg {
                DownloadMessage::Progress(progress, status_text, speed) => {
                    progress_bar_clone.set_fraction(progress);
                    progress_bar_clone.set_text(Some(&format!("{:.0}%", progress * 100.0)));
                    status_label_clone.set_text(&status_text);
                    speed_label_clone.set_text(&speed);
                }
                DownloadMessage::Complete => {
                    progress_bar_clone.set_fraction(1.0);
                    progress_bar_clone.set_text(Some("100%"));
                    status_label_clone.set_text("Concluído");
                    speed_label_clone.set_text("✓");

                    // Esconde botões de controle e mostra botões de arquivo completo
                    pause_btn_clone.set_visible(false);
                    cancel_btn_clone.set_visible(false);
                    open_btn_clone.set_visible(true);
                    delete_btn_clone.set_visible(true);

                    // Marca como completo
                    if let Ok(mut task) = download_task_clone_msg.lock() {
                        task.completed = true;
                    }

                    break;
                }
                DownloadMessage::Error(err) => {
                    status_label_clone.set_text(&format!("Erro: {}", err));
                    speed_label_clone.set_text("");
                    pause_btn_clone.set_visible(false);
                    cancel_btn_clone.set_visible(false);
                    delete_btn_clone.set_visible(true);
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
    pause_btn.connect_clicked(move |btn| {
        if let Ok(mut task) = download_task_clone.lock() {
            task.paused = !task.paused;
            if task.paused {
                btn.set_icon_name("media-playback-start-symbolic");
                btn.set_tooltip_text(Some("Retomar"));
            } else {
                btn.set_icon_name("media-playback-pause-symbolic");
                btn.set_tooltip_text(Some("Pausar"));
            }
        }
    });

    // Handler para botão de cancelar
    let download_task_clone = download_task.clone();
    let row_box_clone_cancel = row_box.clone();
    cancel_btn.connect_clicked(move |_| {
        if let Ok(mut task) = download_task_clone.lock() {
            task.cancelled = true;
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
    delete_btn.connect_clicked(move |_| {
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

            // Faz requisição HEAD para obter tamanho total
            let total_size = match client.head(&url).send().await {
                Ok(resp) => {
                    resp.headers()
                        .get(reqwest::header::CONTENT_LENGTH)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(0)
                }
                Err(e) => {
                    let _ = tx.send(DownloadMessage::Error(format!("Erro ao obter info: {}", e))).await;
                    return;
                }
            };

            // Verifica se existe arquivo parcial para resume
            let mut downloaded = if temp_path.exists() {
                std::fs::metadata(&temp_path).map(|m| m.len()).unwrap_or(0)
            } else {
                0
            };

            // Abre ou cria arquivo para escrita
            let mut file = match if downloaded > 0 {
                OpenOptions::new().append(true).open(&temp_path)
            } else {
                File::create(&temp_path)
            } {
                Ok(f) => f,
                Err(e) => {
                    let _ = tx.send(DownloadMessage::Error(format!("Erro ao criar arquivo: {}", e))).await;
                    return;
                }
            };

            // Faz requisição com Range header para resume
            let mut request = client.get(&url);
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
                        let _ = std::fs::remove_file(&temp_path);
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

                    let status = format!("{}/{}", format_bytes(downloaded), format_bytes(total_size));

                    let _ = tx.send(DownloadMessage::Progress(progress, status, speed_text)).await;

                    last_update = Instant::now();
                    last_downloaded = downloaded;
                }
            }

            // Download completo - renomeia arquivo
            drop(file);
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
