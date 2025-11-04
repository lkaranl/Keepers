use gtk4::{prelude::*, Application, Box as GtkBox, Button, Entry, Label, ListBox, Orientation, ScrolledWindow};
use gtk4::glib;
use libadwaita::{prelude::*, ApplicationWindow as AdwApplicationWindow, HeaderBar, StatusPage, StyleManager};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::io::{BufRead, BufReader};
use std::thread;
use std::time::Duration;
use std::path::PathBuf;

const APP_ID: &str = "com.downstream.app";

#[derive(Clone, Debug)]
enum DownloadMessage {
    Progress(f64, String), // (progress, status_text)
    Complete,
    Error(String),
}

struct AppState {
    downloads: Vec<String>, // Lista de URLs em download
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
        .spacing(8)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    let filename = url.split('/').last().unwrap_or("download").to_string();

    let title_label = Label::builder()
        .label(&filename)
        .halign(gtk4::Align::Start)
        .css_classes(vec!["title-4"])
        .build();

    let progress_box = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .build();

    let progress_bar = gtk4::ProgressBar::builder()
        .hexpand(true)
        .show_text(true)
        .build();

    let status_label = Label::builder()
        .label("Iniciando...")
        .css_classes(vec!["dim-label"])
        .build();

    progress_box.append(&progress_bar);
    progress_box.append(&status_label);

    row_box.append(&title_label);
    row_box.append(&progress_box);

    list_box.append(&row_box);

    if let Ok(mut state) = state.lock() {
        state.downloads.push(url.to_string());
    }

    // Cria channel para comunicação entre threads usando glib
    let (tx, rx) = glib::MainContext::channel(glib::Priority::DEFAULT);

    // Inicia o download em thread separada
    start_download(url, &filename, tx);

    // Monitora mensagens na thread principal do GTK
    let progress_bar_clone = progress_bar.clone();
    let status_label_clone = status_label.clone();

    rx.attach(None, move |msg| {
        match msg {
            DownloadMessage::Progress(progress, status_text) => {
                progress_bar_clone.set_fraction(progress);
                progress_bar_clone.set_text(Some(&format!("{:.0}%", progress * 100.0)));
                status_label_clone.set_text(&status_text);
                glib::ControlFlow::Continue
            }
            DownloadMessage::Complete => {
                progress_bar_clone.set_fraction(1.0);
                progress_bar_clone.set_text(Some("100%"));
                status_label_clone.set_text("Concluído ✓");
                glib::ControlFlow::Break
            }
            DownloadMessage::Error(err) => {
                status_label_clone.set_text(&format!("Erro: {}", err));
                glib::ControlFlow::Break
            }
        }
    });
}

fn start_download(url: &str, filename: &str, tx: glib::Sender<DownloadMessage>) {
    let url = url.to_string();
    let filename = filename.to_string();

    thread::spawn(move || {
        // Diretório de download (diretório atual ou ~/Downloads)
        let download_dir = std::env::current_dir().unwrap_or_else(|_| {
            dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
        });

        let file_path = download_dir.join(&filename);
        let control_file = download_dir.join(format!("{}.aria2", filename));

        // Inicia aria2c
        let mut child = match Command::new("aria2c")
            .args(&[
                &url,
                "--continue=true",
                "--max-connection-per-server=16",
                "--min-split-size=1M",
                "--split=16",
                "--dir", download_dir.to_str().unwrap(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn() {
                Ok(child) => child,
                Err(e) => {
                    let _ = tx.send(DownloadMessage::Error(format!("Falha ao iniciar: {}", e)));
                    return;
                }
            };

        // Thread para ler stderr e extrair o tamanho total
        let stderr = child.stderr.take();
        let tx_clone = tx.clone();

        if let Some(stderr) = stderr {
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().flatten() {
                    // Tenta extrair informações da saída do aria2c
                    if line.contains("FileAlloc") {
                        let _ = tx_clone.send(DownloadMessage::Progress(0.0, "Alocando...".to_string()));
                    }
                }
            });
        }

        // Monitora o progresso verificando o tamanho do arquivo
        let mut last_size = 0u64;
        let mut total_size = 0u64;
        let mut stall_count = 0;

        loop {
            thread::sleep(Duration::from_millis(500));

            // Verifica se o processo ainda está rodando
            if let Ok(Some(status)) = child.try_wait() {
                if status.success() {
                    let _ = tx.send(DownloadMessage::Complete);
                } else {
                    let _ = tx.send(DownloadMessage::Error("Download falhou".to_string()));
                }
                break;
            }

            // Tenta obter tamanho do arquivo sendo baixado
            if let Ok(metadata) = std::fs::metadata(&file_path) {
                let current_size = metadata.len();

                // Tenta obter tamanho total do arquivo .aria2 control file
                if total_size == 0 {
                    // Estima tamanho total (aria2c mostra no arquivo de controle)
                    // Por enquanto, vamos apenas mostrar bytes baixados
                    total_size = current_size * 2; // estimativa inicial
                }

                if current_size > last_size {
                    let speed = (current_size - last_size) * 2; // bytes/segundo (checamos a cada 0.5s)
                    let speed_mb = speed as f64 / 1_048_576.0;

                    // Calcula progresso se temos tamanho total
                    let progress = if total_size > 0 && current_size <= total_size {
                        current_size as f64 / total_size as f64
                    } else if current_size > total_size {
                        // Ajusta estimativa
                        total_size = current_size * 2;
                        0.5
                    } else {
                        0.0
                    };

                    let status_text = if speed_mb > 0.1 {
                        format!("{:.1} MB/s", speed_mb)
                    } else {
                        "Baixando...".to_string()
                    };

                    let _ = tx.send(DownloadMessage::Progress(progress, status_text));
                    last_size = current_size;
                    stall_count = 0;
                } else {
                    stall_count += 1;
                    if stall_count > 60 {
                        // 30 segundos sem progresso
                        let _ = tx.send(DownloadMessage::Error("Download travou".to_string()));
                        let _ = child.kill();
                        break;
                    }
                }
            }

            // Se arquivo de controle .aria2 não existe mais, download completo
            if last_size > 0 && !control_file.exists() {
                let _ = tx.send(DownloadMessage::Complete);
                break;
            }
        }

        // Aguarda o processo finalizar
        let _ = child.wait();
    });
}
