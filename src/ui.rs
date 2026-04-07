use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gio::prelude::*;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use tokio::runtime::Runtime;

use crate::api::{self, Content, Part, UiEvent};
use crate::config::{self, WindowState};
use crate::history;

const APP_TITLE: &str = "Gemini Lite";

// ── UI helpers ──────────────────────────────────────────────────────────────

fn apply_dark_theme() {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_property("gtk-application-prefer-dark-theme", true);
    }
}

fn append_message(view: &gtk::TextView, end_mark: &gtk::TextMark, role: &str, text: &str) {
    let buffer = view.buffer().expect("no buffer");
    let prefix = if role == "user" {
        "▶  You"
    } else {
        "◆  Gemini"
    };
    let mut iter = buffer.end_iter();
    buffer.insert(&mut iter, &format!("\n{prefix}\n{text}\n"));
    let end = buffer.end_iter();
    buffer.move_mark(end_mark, &end);
    view.scroll_to_mark(end_mark, 0.0, false, 0.0, 1.0);
}

fn append_model_header(view: &gtk::TextView, end_mark: &gtk::TextMark) {
    let buffer = view.buffer().expect("no buffer");
    let mut iter = buffer.end_iter();
    buffer.insert(&mut iter, "\n◆  Gemini\n");
    let end = buffer.end_iter();
    buffer.move_mark(end_mark, &end);
    view.scroll_to_mark(end_mark, 0.0, false, 0.0, 1.0);
}

fn append_text_fragment(view: &gtk::TextView, end_mark: &gtk::TextMark, text: &str) {
    let buffer = view.buffer().expect("no buffer");
    let mut iter = buffer.end_iter();
    buffer.insert(&mut iter, text);
    let end = buffer.end_iter();
    buffer.move_mark(end_mark, &end);
    view.scroll_to_mark(end_mark, 0.0, false, 0.0, 1.0);
}

// ── Setup page ──────────────────────────────────────────────────────────────

fn build_setup_page() -> (gtk::Box, gtk::Entry, gtk::Button, gtk::Label) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.set_valign(gtk::Align::Center);
    page.set_halign(gtk::Align::Center);
    page.set_margin_start(48);
    page.set_margin_end(48);

    let title = gtk::Label::new(None);
    title.set_markup("<span size='xx-large' weight='bold'>Gemini Lite</span>");

    let subtitle = gtk::Label::new(Some(
        "Enter your API key from Google AI Studio to get started.",
    ));
    subtitle.set_line_wrap(true);
    subtitle.set_justify(gtk::Justification::Center);

    let key_entry = gtk::Entry::new();
    key_entry.set_visibility(false);
    key_entry.set_placeholder_text(Some("AIza…"));
    key_entry.set_width_chars(52);
    key_entry.set_input_purpose(gtk::InputPurpose::Password);

    let save_btn = gtk::Button::with_label("Save & Connect");

    let error_label = gtk::Label::new(None);
    error_label.set_line_wrap(true);

    let link = gtk::LinkButton::with_label(
        "https://aistudio.google.com",
        "Get a free key at Google AI Studio →",
    );

    page.pack_start(&title, false, false, 0);
    page.pack_start(&subtitle, false, false, 0);
    page.pack_start(&key_entry, false, false, 0);
    page.pack_start(&save_btn, false, false, 0);
    page.pack_start(&error_label, false, false, 0);
    page.pack_start(&link, false, false, 0);

    (page, key_entry, save_btn, error_label)
}

// ── Application ─────────────────────────────────────────────────────────────

pub fn build_ui(app: &gtk::Application, rt: Arc<Runtime>) {
    apply_dark_theme();

    if Path::new("assets/Logo.png").exists() {
        gtk::Window::set_default_icon_from_file("assets/Logo.png").ok();
    } else {
        gtk::Window::set_default_icon_name("com.example.gemini-lite");
    }

    let state = config::load_window_state();
    let window = gtk::ApplicationWindow::new(app);
    window.set_title(APP_TITLE);
    window.set_default_size(state.width, state.height);
    if let (Some(x), Some(y)) = (state.pos_x, state.pos_y) {
        window.move_(x, y);
    }

    if Path::new("assets/Logo.png").exists() {
        window.set_icon_from_file("assets/Logo.png").ok();
    }

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(150);

    // ── Setup page ──────────────────────────────────────────────────────
    let (setup_page, key_entry, save_btn_setup, error_label) = build_setup_page();
    stack.add_named(&setup_page, "setup");

    // ── Chat page ───────────────────────────────────────────────────────
    let chat_root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_vexpand(true);

    let chat_view = gtk::TextView::new();
    chat_view.set_editable(false);
    chat_view.set_cursor_visible(false);
    chat_view.set_wrap_mode(gtk::WrapMode::WordChar);
    chat_view.set_left_margin(14);
    chat_view.set_right_margin(14);
    chat_view.set_top_margin(10);
    chat_view.set_bottom_margin(10);
    scroll.add(&chat_view);
    chat_root.pack_start(&scroll, true, true, 0);
    chat_root.pack_start(
        &gtk::Separator::new(gtk::Orientation::Horizontal),
        false,
        false,
        0,
    );

    let input_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    input_row.set_margin_top(10);
    input_row.set_margin_bottom(10);
    input_row.set_margin_start(14);
    input_row.set_margin_end(14);

    let msg_input = gtk::Entry::new();
    msg_input.set_hexpand(true);
    msg_input.set_placeholder_text(Some("Message Gemini…"));

    let token_label = gtk::Label::new(Some("Context: 0 tokens"));
    token_label.set_margin_end(10);
    token_label.set_opacity(0.6);

    let clear_btn = gtk::Button::with_label("Clear");
    clear_btn.set_tooltip_text(Some("Clear conversation context (Ctrl+K)"));

    let model_selector = gtk::ComboBoxText::new();
    model_selector.append(
        Some("gemini-2.5-flash"),
        "Gemini 2.5 Flash (Fast & Default)",
    );
    model_selector.append(
        Some("gemini-3.1-pro-preview"),
        "Gemini 3.1 Pro (Preview Reasoning)",
    );
    model_selector.append(Some("gemini-2.5-pro"), "Gemini 2.5 Pro (Stable Reasoning)");
    model_selector.set_active_id(Some("gemini-2.5-flash"));
    model_selector.set_tooltip_text(Some("Select the Gemini model to use"));

    let send_btn = gtk::Button::with_label("Send");
    input_row.pack_start(&msg_input, true, true, 0);
    input_row.pack_start(&token_label, false, false, 0);
    input_row.pack_start(&clear_btn, false, false, 0);
    input_row.pack_start(&model_selector, false, false, 0);
    input_row.pack_start(&send_btn, false, false, 0);
    chat_root.pack_start(&input_row, false, false, 0);

    stack.add_named(&chat_root, "chat");
    window.add(&stack);

    // ── Shared state ────────────────────────────────────────────────────
    let buffer = chat_view.buffer().expect("no buffer");
    let end_mark = buffer
        .create_mark(Some("end"), &buffer.end_iter(), false)
        .expect("text buffer must accept end mark");

    let api_key_holder: Rc<RefCell<String>> =
        Rc::new(RefCell::new(config::load_api_key().unwrap_or_default()));

    let history: Arc<Mutex<Vec<Content>>> = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = async_channel::unbounded::<UiEvent>();
    let http_client = Arc::new(reqwest::Client::new());

    // ── Chat send logic ─────────────────────────────────────────────────
    let send: Rc<dyn Fn()> = {
        let msg_input = msg_input.clone();
        let chat_view = chat_view.clone();
        let end_mark = end_mark.clone();
        let history = history.clone();
        let api_key_holder = api_key_holder.clone();
        let tx = tx.clone();
        let rt = rt.clone();
        let send_btn = send_btn.clone();
        let http_client = http_client.clone();
        let model_selector = model_selector.clone();

        Rc::new(move || {
            let text = msg_input.text().trim().to_string();
            if text.is_empty() {
                return;
            }

            let api_key = api_key_holder.borrow().clone();
            if api_key.is_empty() {
                return;
            }

            let selected_model = model_selector
                .active_id()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "gemini-2.5-flash".to_string());

            msg_input.set_text("");
            send_btn.set_sensitive(false);
            model_selector.set_sensitive(false);
            append_message(&chat_view, &end_mark, "user", &text);

            let mut hist = history.lock().unwrap();
            hist.push(Content {
                role: "user".to_string(),
                parts: vec![Part { text }],
            });
            history::prune(&mut hist);
            let snapshot = hist.clone();
            drop(hist);

            let tx = tx.clone();
            let client = http_client.clone();
            let base_url = api::API_BASE_URL.to_string();
            rt.spawn(async move {
                match api::stream_gemini(
                    &client,
                    &base_url,
                    &api_key,
                    &selected_model,
                    &snapshot,
                    &tx,
                )
                .await
                {
                    Ok((reply, tokens)) => {
                        tx.try_send(UiEvent::Done(reply, tokens)).ok();
                    }
                    Err(e) => {
                        tx.try_send(UiEvent::Error(format!("{e:#}"))).ok();
                    }
                }
            });
        })
    };

    let s = Rc::clone(&send);
    send_btn.connect_clicked(move |_| s());
    msg_input.connect_activate(move |_| send());

    // ── Clear logic ─────────────────────────────────────────────────────
    let clear: Rc<dyn Fn()> = {
        let history = history.clone();
        let chat_view = chat_view.clone();
        let token_label = token_label.clone();
        Rc::new(move || {
            history.lock().unwrap().clear();
            let buffer = chat_view.buffer().expect("no buffer");
            buffer.set_text("");
            token_label.set_text("Context: 0 tokens");
        })
    };

    let c_btn = Rc::clone(&clear);
    clear_btn.connect_clicked(move |_| c_btn());

    // ── Async result handler ────────────────────────────────────────────
    {
        let chat_view = chat_view.clone();
        let end_mark = end_mark.clone();
        let send_btn_rx = send_btn.clone();
        let model_selector_rx = model_selector.clone();
        let token_label_rx = token_label.clone();

        let mut model_message_open = false;
        glib::MainContext::default().spawn_local(async move {
            while let Ok(result) = rx.recv().await {
                match result {
                    UiEvent::Delta(fragment) => {
                        if !model_message_open {
                            append_model_header(&chat_view, &end_mark);
                            model_message_open = true;
                        }
                        append_text_fragment(&chat_view, &end_mark, &fragment);
                    }
                    UiEvent::Done(reply, tokens) => {
                        append_text_fragment(&chat_view, &end_mark, "\n");
                        history.lock().unwrap().push(Content {
                            role: "model".to_string(),
                            parts: vec![Part {
                                text: reply.clone(),
                            }],
                        });
                        token_label_rx.set_text(&format!("Context: {tokens} tokens"));
                        send_btn_rx.set_sensitive(true);
                        model_selector_rx.set_sensitive(true);
                        model_message_open = false;
                    }
                    UiEvent::Error(e) => {
                        let buf = chat_view.buffer().expect("no buffer");
                        let mut iter = buf.end_iter();
                        buf.insert(&mut iter, &format!("\n⚠  Error: {e}\n"));
                        let end = buf.end_iter();
                        buf.move_mark(&end_mark, &end);
                        chat_view.scroll_to_mark(&end_mark, 0.0, false, 0.0, 1.0);
                        send_btn_rx.set_sensitive(true);
                        model_selector_rx.set_sensitive(true);
                        model_message_open = false;
                    }
                }
            }
        });
    }

    // ── Setup page submit ───────────────────────────────────────────────
    let submit: Rc<dyn Fn()> = {
        let key_entry = key_entry.clone();
        let error_label = error_label.clone();
        let stack = stack.clone();
        let api_key_holder = api_key_holder.clone();
        let msg_input = msg_input.clone();
        let send_btn = send_btn.clone();

        Rc::new(move || {
            let raw = key_entry.text().trim().to_string();
            if raw.is_empty() {
                error_label.set_text("Key cannot be empty.");
                return;
            }
            match config::save_api_key(&raw) {
                Ok(()) => {
                    *api_key_holder.borrow_mut() = raw;
                    msg_input.set_sensitive(true);
                    send_btn.set_sensitive(true);
                    stack.set_visible_child_name("chat");
                }
                Err(e) => {
                    log::error!("failed to persist API key: {e:#}");
                    error_label.set_text(&format!("Could not save key: {e:#}"));
                }
            }
        })
    };

    let s = Rc::clone(&submit);
    save_btn_setup.connect_clicked(move |_| s());
    key_entry.connect_activate(move |_| submit());

    // ── Initial page ────────────────────────────────────────────────────
    if api_key_holder.borrow().is_empty() {
        stack.set_visible_child_name("setup");
        msg_input.set_sensitive(false);
        send_btn.set_sensitive(false);
    } else {
        stack.set_visible_child_name("chat");
    }

    let c_key = Rc::clone(&clear);
    window.connect_key_press_event(move |win, event| {
        if event.state().contains(gdk::ModifierType::CONTROL_MASK) {
            match event.keyval() {
                gdk::keys::constants::q => {
                    win.close();
                    return glib::Propagation::Stop;
                }
                gdk::keys::constants::k => {
                    c_key();
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
        }
        glib::Propagation::Proceed
    });

    let win_ref = window.clone();
    window.connect_delete_event(move |_, _| {
        let (w, h) = win_ref.size();
        let (x, y) = win_ref.position();
        let state = WindowState {
            width: w.max(640),
            height: h.max(480),
            pos_x: Some(x),
            pos_y: Some(y),
        };
        if let Err(e) = config::save_window_state(&state) {
            log::warn!("could not persist window state: {e:#}");
        }
        glib::Propagation::Proceed
    });

    window.show_all();
    log::info!("Gemini Lite started");
}
