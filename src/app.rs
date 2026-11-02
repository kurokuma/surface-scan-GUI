use crate::{
    input,
    model::{Config, Record},
    worker::{self, Event},
};
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use std::{
    net::Ipv4Addr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    time::{Duration, Instant},
};

enum UiEvent {
    Imported(Result<String, String>),
    Loaded(Result<Vec<Arc<Record>>, String>),
    Saved(Result<String, String>),
}

pub struct ScannerApp {
    input: String,
    ports: String,
    extract: bool,
    http: bool,
    https: bool,
    concurrency: usize,
    processes: usize,
    timeout: u64,
    body_kib: usize,
    insecure: bool,
    host: String,
    max_jobs: u64,
    max_rows: usize,
    rows: Vec<Arc<Record>>,
    visible: Vec<usize>,
    selected: Option<usize>,
    detail: String,
    table_focused: bool,
    query: String,
    status_filter: String,
    scheme_filter: usize,
    successes_only: bool,
    sort: usize,
    descending: bool,
    dirty: bool,
    last_filter: Instant,
    scan_rx: Option<Receiver<Event>>,
    cancel: Arc<AtomicBool>,
    running: bool,
    scan_handle: Option<std::thread::JoinHandle<()>>,
    total: u64,
    received: u64,
    started: Option<Instant>,
    finished_elapsed: Option<Duration>,
    stop_reason: String,
    message: String,
    ui_tx: Sender<UiEvent>,
    ui_rx: Receiver<UiEvent>,
    io_busy: bool,
}

impl ScannerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx
            .style_mut(|style| style.spacing.scroll = egui::style::ScrollStyle::solid());
        let mut fonts = egui::FontDefinitions::default();
        for path in [
            "C:/Windows/Fonts/meiryo.ttc",
            "C:/Windows/Fonts/YuGothM.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        ] {
            if let Ok(data) = std::fs::read(path) {
                fonts.font_data.insert(
                    "Japanese".into(),
                    Arc::new(egui::FontData::from_owned(data)),
                );
                fonts
                    .families
                    .get_mut(&egui::FontFamily::Proportional)
                    .unwrap()
                    .insert(0, "Japanese".into());
                fonts
                    .families
                    .get_mut(&egui::FontFamily::Monospace)
                    .unwrap()
                    .push("Japanese".into());
                break;
            }
        }
        for (name, path) in [
            ("ChineseSimplified", "C:/Windows/Fonts/msyh.ttc"),
            ("ChineseTraditional", "C:/Windows/Fonts/msjh.ttc"),
            ("CjkFallback", "C:/Windows/Fonts/simsun.ttc"),
        ] {
            if let Ok(data) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert(name.into(), Arc::new(egui::FontData::from_owned(data)));
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    fonts.families.get_mut(&family).unwrap().push(name.into());
                }
            }
        }
        cc.egui_ctx.set_fonts(fonts);
        let (ui_tx, ui_rx) = mpsc::channel();
        Self {
            input: String::new(),
            ports: "80,443,8080,8443".into(),
            extract: false,
            http: true,
            https: true,
            concurrency: 64,
            processes: 1,
            timeout: 5,
            body_kib: 16,
            insecure: true,
            host: String::new(),
            max_jobs: 100_000,
            max_rows: 10_000,
            rows: Vec::new(),
            visible: Vec::new(),
            selected: None,
            detail: String::new(),
            table_focused: false,
            query: String::new(),
            status_filter: String::new(),
            scheme_filter: 0,
            successes_only: false,
            sort: 0,
            descending: false,
            dirty: false,
            last_filter: Instant::now(),
            scan_rx: None,
            scan_handle: None,
            cancel: Arc::new(AtomicBool::new(false)),
            running: false,
            total: 0,
            received: 0,
            started: None,
            finished_elapsed: None,
            stop_reason: String::new(),
            message: "IPv4 / CIDRを入力してください".into(),
            ui_tx,
            ui_rx,
            io_busy: false,
        }
    }

    fn start(&mut self) -> anyhow::Result<()> {
        let ranges = input::targets(&self.input, self.extract)?;
        let ports = input::ports(&self.ports)?;
        let schemes: Vec<String> = [(self.http, "http"), (self.https, "https")]
            .into_iter()
            .filter(|(on, _)| *on)
            .map(|(_, s)| s.into())
            .collect();
        anyhow::ensure!(!schemes.is_empty(), "HTTP / HTTPSを選択してください");
        let host = self.host.trim().to_owned();
        anyhow::ensure!(
            host.is_empty()
                || (host.parse::<Ipv4Addr>().is_err()
                    && host.len() <= 253
                    && host.split('.').all(|label| !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-'))),
            "Host/SNIはドメイン名のみを指定してください"
        );
        let total = input::job_count(&ranges, ports.len(), schemes.len());
        anyhow::ensure!(total <= self.max_jobs, "予定プローブ数 {} が上限 {} を超えています。対象を絞るか詳細設定の上限を変更してください", total, self.max_jobs);
        let config = Config {
            ranges,
            ports,
            schemes,
            concurrency: self.concurrency,
            timeout_secs: self.timeout,
            body_limit: self.body_kib * 1024,
            insecure: self.insecure,
            host,
            worker_index: 0,
            workers: self.processes,
        };
        self.clear_results();
        self.total = total;
        self.cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(128);
        self.scan_rx = Some(rx);
        self.started = Some(Instant::now());
        self.finished_elapsed = None;
        self.stop_reason.clear();
        self.running = true;
        self.dirty = true;
        self.message = format!(
            "スキャン中 · {}プロセス × {}同時プローブ",
            self.processes, self.concurrency
        );
        if let Some(handle) = self.scan_handle.take() {
            let _ = handle.join();
        }
        self.scan_handle = Some(worker::launch(config, self.cancel.clone(), tx));
        Ok(())
    }

    fn clear_results(&mut self) {
        self.rows.clear();
        self.visible.clear();
        self.selected = None;
        self.detail.clear();
        self.table_focused = false;
        self.query.clear();
        self.status_filter.clear();
        self.scheme_filter = 0;
        self.successes_only = false;
        self.total = 0;
        self.received = 0;
        self.started = None;
        self.finished_elapsed = None;
        self.stop_reason.clear();
        self.scan_rx = None;
        self.dirty = false;
        self.message = "結果をクリアしました".into();
    }

    fn poll(&mut self) {
        let began = Instant::now();
        if let Some(rx) = &self.scan_rx {
            while began.elapsed() < Duration::from_millis(8) {
                match rx.try_recv() {
                    Ok(Event::Row(row)) => {
                        self.received += 1;
                        if self.rows.len() < self.max_rows {
                            self.rows.push(Arc::from(row));
                            self.dirty = true;
                        }
                        if self.rows.len() >= self.max_rows && self.received < self.total {
                            self.cancel.store(true, Ordering::Relaxed);
                            self.stop_reason = format!("保持上限 {} 件", self.max_rows);
                            self.message = format!(
                                "保持上限 {} 件に達したため停止中。保存後に対象を分割してください",
                                self.max_rows
                            );
                        }
                    }
                    Ok(Event::Problem(e)) => {
                        self.message = format!("エラー: {e}");
                    }
                    Ok(Event::Done(stopped)) => {
                        self.running = false;
                        self.finished_elapsed = self.started.map(|t| t.elapsed());
                        self.dirty = true;
                        if !self.message.starts_with("エラー:") {
                            self.message = format!(
                                "{} · 受信 {} / {} · 保持 {} 件 {}",
                                if stopped { "停止" } else { "完了" },
                                self.received,
                                self.total,
                                self.rows.len(),
                                self.stop_reason
                            );
                        }
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if self.running {
                            self.message = "workerとの接続が終了しました".into();
                            self.running = false;
                            self.finished_elapsed = self.started.map(|t| t.elapsed());
                        }
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }
        while let Ok(event) = self.ui_rx.try_recv() {
            self.io_busy = false;
            match event {
                UiEvent::Imported(Ok(text)) => {
                    self.input = text;
                    self.message = "テキストを読み込みました".into();
                }
                UiEvent::Saved(Ok(path)) => self.message = format!("保存しました: {path}"),
                UiEvent::Loaded(Ok(rows)) => {
                    self.rows = rows;
                    self.selected = None;
                    self.detail.clear();
                    self.query.clear();
                    self.status_filter.clear();
                    self.scheme_filter = 0;
                    self.successes_only = false;
                    self.dirty = true;
                    self.total = 0;
                    self.received = 0;
                    self.started = None;
                    self.finished_elapsed = None;
                    self.message = format!("保存結果を読み込みました · {} 件", self.rows.len());
                }
                UiEvent::Imported(Err(e)) | UiEvent::Saved(Err(e)) | UiEvent::Loaded(Err(e)) => {
                    self.message = e
                }
            }
        }
        if self.dirty && (!self.running || self.last_filter.elapsed() > Duration::from_millis(250))
        {
            self.filter();
        }
    }

    fn filter(&mut self) {
        let query = self.query.to_lowercase();
        let status = self.status_filter.trim();
        self.visible = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                (!self.successes_only || r.status.is_some())
                    && (self.scheme_filter == 0
                        || r.scheme
                            == if self.scheme_filter == 1 {
                                "http"
                            } else {
                                "https"
                            })
                    && (status.is_empty()
                        || r.status
                            .map(|s| s.to_string().starts_with(status))
                            .unwrap_or(false))
                    && (query.is_empty() || r.search_text().contains(&query))
            })
            .map(|(i, _)| i)
            .collect();
        let rows = &self.rows;
        let column = self.sort;
        let descending = self.descending;
        self.visible.sort_by(|&a, &b| {
            let a = &rows[a];
            let b = &rows[b];
            let order = match column {
                0 => {
                    a.ip.parse::<Ipv4Addr>()
                        .ok()
                        .cmp(&b.ip.parse::<Ipv4Addr>().ok())
                }
                1 => a.port.cmp(&b.port),
                2 => a.scheme.cmp(&b.scheme),
                3 => a.status.cmp(&b.status),
                4 => a.title.cmp(&b.title),
                5 => a.body.cmp(&b.body),
                6 => a.server.cmp(&b.server),
                7 => a.content_type.cmp(&b.content_type),
                8 => a.body_bytes.cmp(&b.body_bytes),
                9 => a.elapsed_ms.cmp(&b.elapsed_ms),
                10 => a
                    .tls
                    .as_ref()
                    .map(|t| &t.version)
                    .cmp(&b.tls.as_ref().map(|t| &t.version)),
                11 => a.favicon_mmh3.cmp(&b.favicon_mmh3),
                _ => a.error.cmp(&b.error),
            };
            if descending {
                order.reverse()
            } else {
                order
            }
        });
        self.dirty = false;
        self.last_filter = Instant::now();
    }

    fn import(&mut self) {
        self.io_busy = true;
        let tx = self.ui_tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let path = rfd::FileDialog::new()
                    .add_filter("Text", &["txt", "log", "csv"])
                    .pick_file()
                    .ok_or("読み込みをキャンセルしました")?;
                let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
                if metadata.len() > 2 * 1024 * 1024 {
                    return Err("入力ファイルは2 MiB以内にしてください".into());
                }
                std::fs::read_to_string(path)
                    .map(|s| s.trim_start_matches('\u{feff}').to_owned())
                    .map_err(|e| format!("UTF-8テキストとして読み込めません: {e}"))
            })();
            let _ = tx.send(UiEvent::Imported(result));
        });
    }

    fn save(&mut self, json: bool, filtered: bool) {
        let rows: Vec<_> = if filtered {
            self.visible.iter().map(|&i| self.rows[i].clone()).collect()
        } else {
            self.rows.clone()
        };
        self.io_busy = true;
        let tx = self.ui_tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let ext = if json { "json" } else { "csv" };
                let path = rfd::FileDialog::new()
                    .add_filter(ext, &[ext])
                    .set_file_name(format!("fingerprints.{ext}"))
                    .save_file()
                    .ok_or("保存をキャンセルしました")?;
                crate::export::save(&path, &rows, json).map_err(|e| e.to_string())?;
                Ok(path.display().to_string())
            })();
            let _ = tx.send(UiEvent::Saved(result));
        });
    }

    fn select(&mut self, index: usize) {
        self.selected = Some(index);
        self.detail = serde_json::to_string_pretty(&self.rows[index]).unwrap_or_default();
    }

    fn load_results(&mut self) {
        self.io_busy = true;
        let tx = self.ui_tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<_, String> {
                let path = rfd::FileDialog::new()
                    .add_filter("保存結果", &["json", "csv"])
                    .pick_file()
                    .ok_or("結果の読込をキャンセルしました")?;
                crate::export::load(&path).map_err(|e| format!("結果の読込に失敗しました: {e:#}"))
            })();
            let _ = tx.send(UiEvent::Loaded(result));
        });
    }
}

impl eframe::App for ScannerApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        if self.running || self.io_busy {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        egui::TopBottomPanel::top("brand").show(ctx, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("WEB SERVICE").color(Color32::from_rgb(78, 211, 187)));
                ui.heading("/ FINGERPRINT");
                ui.separator();
                ui.label("IPv4 ネットワーク調査");
                if self.running {
                    ui.spinner();
                }
            });
            ui.add_space(8.0);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(&self.message);
            if self.total > 0 {
                let progress = (self.received as f64 / self.total as f64) as f32;
                let elapsed = self
                    .finished_elapsed
                    .or_else(|| self.started.map(|t| t.elapsed()))
                    .map(|t| t.as_secs_f64())
                    .unwrap_or(0.0);
                ui.add(egui::ProgressBar::new(progress).text(format!(
                    "{} / {} · {:.1}秒 · {:.1} probes/s",
                    self.received,
                    self.total,
                    elapsed,
                    self.received as f64 / elapsed.max(0.1)
                )));
            }
        });
        egui::SidePanel::left("settings").default_width(310.0).resizable(true).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(8.0); ui.heading("スキャン設定"); ui.separator();
                ui.add_enabled_ui(!self.running, |ui| {
                    ui.label("対象 IPv4 / CIDR");
                    egui::ScrollArea::vertical()
                        .id_salt("target-input")
                        .min_scrolled_height(140.0)
                        .max_height(140.0)
                        .auto_shrink([false, false])
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                        .show(ui, |ui| {
                            ui.add(egui::TextEdit::multiline(&mut self.input).desired_width(f32::INFINITY).desired_rows(7)
                                .hint_text("192.168.1.1\n192.168.2.0/24\n\n1行ずつ入力 / そのまま貼り付け"));
                        });
                    if ui.add_enabled(!self.io_busy, egui::Button::new("テキストファイルを読み込む…")).clicked() { self.import(); }
                    ui.checkbox(&mut self.extract, "雑多なテキストからIPv4/CIDRを抽出");
                    ui.add_space(8.0); ui.label("ポート / 範囲 (1–65535)");
                    ui.text_edit_singleline(&mut self.ports).on_hover_text("例: 80,443,8000-8100 または 1-65535");
                    ui.horizontal(|ui| { ui.checkbox(&mut self.http, "HTTP"); ui.checkbox(&mut self.https, "HTTPS"); });
                    ui.label("選択した各ポートで選択した全方式を試行");
                    ui.separator();
                    egui::Grid::new("limits").num_columns(2).spacing([10.0,8.0]).show(ui, |ui| {
                        ui.label("プロセス数"); ui.add(egui::DragValue::new(&mut self.processes).range(1..=8)); ui.end_row();
                        ui.label("同時数 / プロセス"); ui.add(egui::DragValue::new(&mut self.concurrency).range(1..=1024)); ui.end_row();
                        ui.label("タイムアウト / 秒"); ui.add(egui::DragValue::new(&mut self.timeout).range(1..=120)); ui.end_row();
                        ui.label("本文・icon上限 / KiB"); ui.add(egui::DragValue::new(&mut self.body_kib).range(1..=1024)); ui.end_row();
                    });
                    ui.checkbox(&mut self.insecure, "未検証・自己署名証明書も収集").on_hover_text("有効時はTLSの信頼性・ホスト名を検証しません。署名ハンドシェイクは検証します。");
                    ui.collapsing("詳細設定", |ui| {
                        ui.label("Host / TLS SNI（任意のドメイン名）"); ui.text_edit_singleline(&mut self.host);
                        ui.label("接続先は入力IPに固定されます");
                        ui.horizontal(|ui| { ui.label("プローブ上限"); ui.add(egui::DragValue::new(&mut self.max_jobs).range(1..=10_000_000).speed(100.0)); });
                        ui.horizontal(|ui| { ui.label("結果保持上限"); ui.add(egui::DragValue::new(&mut self.max_rows).range(1..=100_000).speed(100.0)); });
                        ui.label("保持上限に達すると停止します。本文・Base64・JSON表示にメモリを使用します。");
                    });
                });
                ui.add_space(12.0);
                if self.running {
                    if ui.add_sized([ui.available_width(),36.0], egui::Button::new("停止する")).clicked() {
                        self.cancel.store(true, Ordering::Relaxed); self.stop_reason = "ユーザーによる停止".into(); self.message = "停止中…".into();
                    }
                } else if ui.add_enabled(!self.io_busy, egui::Button::new("スキャン開始").fill(Color32::from_rgb(24,111,100)).min_size(egui::vec2(ui.available_width(),36.0))).clicked() {
                    if let Err(e) = self.start() { self.message = e.to_string(); }
                }
                ui.add_space(8.0);
                ui.small("CIDRはネットワーク・ブロードキャストを含む全アドレス。重複は除去します。リダイレクトは追跡しません。");
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("結果");
                if ui.add_enabled(!self.running && !self.io_busy,egui::Button::new("結果を読込…")).clicked() { self.load_results(); }
                if ui.add_enabled(!self.running && !self.io_busy && !self.rows.is_empty(),egui::Button::new("結果をクリア")).on_hover_text("結果・詳細・検索条件・進捗表示をクリアします。スキャン設定は保持します。").clicked() { self.clear_results(); }
                ui.label(format!("{} / {} 件", self.visible.len(), self.rows.len()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!self.io_busy && !self.rows.is_empty(), |ui| {
                        ui.menu_button("保存…", |ui| {
                            for (label, json, filtered) in [
                                ("全件 JSON", true, false),
                                ("全件 CSV", false, false),
                                ("表示結果 JSON", true, true),
                                ("表示結果 CSV", false, true),
                            ] {
                                if ui.button(label).clicked() {
                                    if self.dirty {
                                        self.filter();
                                    }
                                    self.save(json, filtered);
                                    ui.close_menu();
                                }
                            }
                        });
                    });
                });
            });
            ui.horizontal_wrapped(|ui| {
                self.dirty |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text("全フィールド検索（body / hash / TLS…）")
                            .desired_width(300.0),
                    )
                    .changed();
                ui.label("Status");
                self.dirty |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.status_filter)
                            .hint_text("200 / 4")
                            .desired_width(60.0),
                    )
                    .changed();
                let before = self.scheme_filter;
                egui::ComboBox::from_id_salt("scheme")
                    .selected_text(["全方式", "HTTP", "HTTPS"][self.scheme_filter])
                    .show_ui(ui, |ui| {
                        for (i, label) in ["全方式", "HTTP", "HTTPS"].iter().enumerate() {
                            ui.selectable_value(&mut self.scheme_filter, i, *label);
                        }
                    });
                self.dirty |= before != self.scheme_filter;
                self.dirty |= ui
                    .checkbox(&mut self.successes_only, "HTTP応答あり")
                    .changed();
            });
            ui.small("行を選択して↑↓で移動 · IPをダブルクリックしてブラウザで開く · 列名でソート");
            ui.separator();
            let mut scroll_selection = false;
            if self.table_focused && !ctx.wants_keyboard_input() && !self.visible.is_empty() {
                let direction = ctx.input_mut(|i| {
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) { 1 }
                    else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) { -1 }
                    else { 0 }
                });
                if direction != 0 {
                    let position = next_row(&self.visible, self.selected, direction);
                    self.select(self.visible[position]);
                    scroll_selection = true;
                }
            }
            if self.selected.is_some() {
                let max_height = (ui.available_height() - 130.0).max(100.0);
                egui::TopBottomPanel::bottom("fingerprint-detail")
                    .resizable(true).default_height(240.0).height_range(100.0..=max_height)
                    .show_inside(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.strong("Fingerprint / 詳細JSON");
                            ui.small("上の境界をドラッグして高さを変更");
                            if ui.button("コピー").clicked() { ctx.copy_text(self.detail.clone()); }
                            if ui.button("閉じる").clicked() { self.selected = None; }
                        });
                        egui::ScrollArea::both().id_salt("detail")
                            .auto_shrink([false,false])
                            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                            .show(ui, |ui| {
                                ui.add(egui::TextEdit::multiline(&mut self.detail.as_str())
                                    .font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
                            });
                    });
            }
            let table_rect = ui.available_rect_before_wrap();
            if ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !table_rect.contains(p))) {
                self.table_focused = false;
            }
            let mut clicked = None;
            let mut open = None;
            egui::ScrollArea::both().id_salt("results-scroll")
                .auto_shrink([false,false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .show(ui, |ui| {
                    let origin = ui.cursor().min;
                    let mut table = TableBuilder::new(ui).id_salt("fingerprints-v2")
                        .striped(true).resizable(true).vscroll(false)
                        .sense(egui::Sense::click())
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
                    for width in [125.0,60.0,60.0,60.0,230.0,280.0,130.0,160.0,80.0,80.0,80.0,110.0,260.0] {
                        table = table.column(Column::initial(width).at_least(40.0).clip(true));
                    }
                    table.header(28.0, |mut header| {
                        for (i,name) in ["IP","Port","方式","Status","Title","Body","Server","Content-Type","Body bytes","ms","TLS","Icon MMH3","Error"].iter().enumerate() {
                            header.col(|ui| {
                                let text = if self.sort == i { format!("{name} {}",if self.descending { "▼" } else { "▲" }) } else { name.to_string() };
                                if ui.button(text).clicked() {
                                    if self.sort == i { self.descending = !self.descending; }
                                    else { self.sort = i; self.descending = false; }
                                    self.dirty = true;
                                }
                            });
                        }
                    }).body(|body| body.rows(26.0,self.visible.len(),|mut row| {
                        let index = self.visible[row.index()];
                        let r = &self.rows[index];
                        row.set_selected(self.selected == Some(index));
                        let preview = body_preview(&r.body);
                        let values = [r.ip.clone(),r.port.to_string(),r.scheme.clone(),
                            r.status.map(|s|s.to_string()).unwrap_or_else(||"—".into()),
                            r.title.clone(),preview,r.server.clone(),r.content_type.clone(),
                            format!("{}{}",r.body_bytes,if r.body_truncated { "+" } else { "" }),
                            r.elapsed_ms.to_string(),r.tls.as_ref().map(|t|t.version.clone()).unwrap_or_default(),
                            r.favicon_mmh3.map(|h|h.to_string()).unwrap_or_default(),r.error.clone()];
                        for (column,value) in values.iter().enumerate() {
                            let (_,response) = row.col(|ui| {
                                    ui.add(egui::Label::new(value).truncate().selectable(false));
                            });
                            if column == 0 && response.double_clicked() { open = Some(index); }
                            if column == 5 { response.on_hover_text("本文は幅に合わせて省略しています。全文は詳細JSONで確認できます。"); }
                        }
                        if row.response().clicked() { clicked = Some(index); }
                    }));
                    if scroll_selection {
                        if let Some(position) = self.visible.iter().position(|i| Some(*i) == self.selected) {
                            let y = origin.y + 28.0 + ui.spacing().item_spacing.y + position as f32 * (26.0 + ui.spacing().item_spacing.y);
                            ui.scroll_to_rect(egui::Rect::from_min_size(egui::pos2(ui.clip_rect().left(),y),egui::vec2(1.0,26.0)),None);
                        }
                    }
                });
            if let Some(index) = clicked {
                self.select(index);
                self.table_focused = true;
                if let Some(id) = ctx.memory(|m|m.focused()) { ctx.memory_mut(|m|m.surrender_focus(id)); }
            }
            if let Some(index) = open {
                let result = self.rows[index].browser_url().and_then(|url|webbrowser::open(&url).map_err(anyhow::Error::from));
                if let Err(e) = result { self.message = format!("ブラウザを開けません: {e}"); }
            }
        });
    }
}

impl Drop for ScannerApp {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.scan_rx.take();
        if let Some(handle) = self.scan_handle.take() {
            let _ = handle.join();
        }
    }
}

fn body_preview(body: &str) -> String {
    body.chars()
        .take(512)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn next_row(visible: &[usize], selected: Option<usize>, direction: i32) -> usize {
    let position = visible.iter().position(|i| Some(*i) == selected);
    match position {
        Some(i) if direction < 0 => i.saturating_sub(1),
        Some(i) => (i + 1).min(visible.len().saturating_sub(1)),
        None if direction < 0 => visible.len().saturating_sub(1),
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_uses_visible_order_and_clamps() {
        let rows = [8, 3, 9];
        assert_eq!(next_row(&rows, Some(8), 1), 1);
        assert_eq!(next_row(&rows, Some(9), 1), 2);
        assert_eq!(next_row(&rows, Some(8), -1), 0);
        assert_eq!(next_row(&rows, Some(3), -1), 0);
        assert_eq!(next_row(&rows, Some(55), 1), 0);
        assert_eq!(next_row(&rows, None, -1), 2);
        assert_eq!(body_preview("中文\n正文"), "中文 正文");
    }
}
