use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use gio::prelude::*;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

const APP_ID: &str = "com.example.gemini-lite";
const APP_TITLE: &str = "Gemini Lite";
const API_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const DEFAULT_WIDTH: i32 = 900;
const DEFAULT_HEIGHT: i32 = 700;
const KEYRING_SERVICE: &str = "gemini-lite";
const KEYRING_USER: &str = "api-key";

// ── Persistent window state ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WindowState {
    width: i32,
    height: i32,
    pos_x: Option<i32>,
    pos_y: Option<i32>,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            pos_x: None,
            pos_y: None,
        }
    }
}

fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return Path::new(&dir).join("gemini-lite");
    }
    std::env::var("HOME")
        .map(|h| Path::new(&h).join(".config").join("gemini-lite"))
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn state_path() -> PathBuf {
    config_dir().join("window-state.json")
}

fn load_window_state() -> WindowState {
    match fs::read_to_string(state_path()).and_then(|s| Ok(serde_json::from_str(&s)?)) {
        Ok(state) => state,
        Err(e) => {
            log::debug!("no persisted window state ({e}), using defaults");
            WindowState::default()
        }
    }
}

fn save_window_state(window: &gtk::ApplicationWindow) -> Result<()> {
    let (w, h) = window.size();
    let (x, y) = window.position();

    let state = WindowState {
        width: w.max(640),
        height: h.max(480),
        pos_x: Some(x),
        pos_y: Some(y),
    };

    let dir = config_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create config dir {}", dir.display()))?;

    let json = serde_json::to_string_pretty(&state).context("serialization failed")?;
    fs::write(state_path(), json).context("cannot write window-state.json")?;
    log::debug!("window state saved: {}x{} @ ({x},{y})", w, h);
    Ok(())
}

// ── API key — Secret Service primary, file fallback ───────────────────────────

fn key_file_path() -> PathBuf {
    config_dir().join("api-key")
}

/// Load order: env var → GNOME Keyring → plain file in XDG_CONFIG_HOME.
fn load_api_key() -> Option<String> {
    if let Ok(k) = std::env::var("GEMINI_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            log::debug!("API key loaded from environment");
            return Some(k);
        }
    }

    if let Some(k) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .ok()
        .and_then(|e| e.get_password().ok())
        .filter(|k| !k.trim().is_empty())
    {
        log::debug!("API key loaded from system keyring");
        return Some(k.trim().to_string());
    }

    if let Some(k) = fs::read_to_string(key_file_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|k| !k.is_empty())
    {
        log::debug!("API key loaded from config file (fallback)");
        return Some(k);
    }

    None
}

/// Attempt Secret Service first; if the daemon is absent, fall back to a
/// mode-0600 plain-text file under XDG_CONFIG_HOME.
fn save_api_key(key: &str) -> Result<()> {
    match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .context("cannot construct keyring entry")?
        .set_password(key)
    {
        Ok(()) => {
            log::info!("API key saved to system keyring");
            Ok(())
        }
        Err(e) => {
            log::warn!("keyring unavailable ({e:#}), falling back to config file");
            persist_key_to_file(key)
        }
    }
}

fn persist_key_to_file(key: &str) -> Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create config dir {}", dir.display()))?;

    let path = key_file_path();
    fs::write(&path, key).with_context(|| format!("cannot write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .context("cannot chmod api-key file")?;
    }

    log::info!("API key saved to {}", path.display());
    Ok(())
}

// ── Gemini API types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Part {
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Content {
    role: String,
    parts: Vec<Part>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    contents: &'a [Content],
}

#[derive(Debug)]
enum UiEvent {
    Delta(String),
    Done(String, u32),
    Error(String),
}

fn extract_text_and_tokens(payload: &serde_json::Value) -> (String, Option<u32>) {
    let mut out = String::new();
    if let Some(candidates) = payload["candidates"].as_array() {
        for candidate in candidates {
            if let Some(parts) = candidate["content"]["parts"].as_array() {
                for part in parts {
                    if let Some(text) = part["text"].as_str() {
                        out.push_str(text);
                    }
                }
            }
        }
    }
    let tokens = payload["usageMetadata"]["totalTokenCount"]
        .as_u64()
        .map(|v| v as u32);
    (out, tokens)
}

fn extract_sse_events(buffer: &mut String) -> Vec<String> {
    let mut events = Vec::new();
    while let Some(idx) = buffer.find("\n\n") {
        let raw_event = buffer[..idx].to_string();
        buffer.drain(..idx + 2);

        let mut data_lines = Vec::new();
        for line in raw_event.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.trim_start().to_string());
            }
        }
        if !data_lines.is_empty() {
            events.push(data_lines.join("\n"));
        }
    }
    events
}

async fn stream_gemini(
    client: &reqwest::Client,
    api_key: &str,
    model_id: &str,
    history: &[Content],
    tx: &async_channel::Sender<UiEvent>,
) -> Result<(String, u32)> {
    let system_instruction = Some(Content {
        role: "user".to_string(),
        parts: vec![Part {
            text: "You are a highly efficient, expert AI assistant. Be direct, concise, and prioritize accuracy. If the user asks a coding question, provide senior-level answers without unnecessary fluff. Respond in French unless asked otherwise.".to_string(),
        }],
    });

    let body = GenerateRequest {
        system_instruction,
        contents: history,
    };

    let resp = client
        .post(format!(
            "{API_BASE_URL}/{model_id}:streamGenerateContent?alt=sse&key={api_key}"
        ))
        .json(&body)
        .send()
        .await
        .context("HTTP request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let json: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse API error response")?;
        let msg = json["error"]["message"]
            .as_str()
            .unwrap_or("unknown API error");
        anyhow::bail!("API {status}: {msg}");
    }

    let mut stream = resp.bytes_stream();
    let mut sse_buffer = String::new();
    let mut full_text = String::new();
    let mut last_snapshot = String::new();
    let mut final_tokens = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("failed to read stream chunk")?;
        sse_buffer.push_str(&String::from_utf8_lossy(&chunk));
        sse_buffer = sse_buffer.replace("\r\n", "\n");

        let events = extract_sse_events(&mut sse_buffer);
        for event in events {
            if event == "[DONE]" {
                continue;
            }
            let payload: serde_json::Value =
                serde_json::from_str(&event).context("invalid SSE JSON payload")?;
            let (fragment, tokens) = extract_text_and_tokens(&payload);
            if let Some(t) = tokens {
                final_tokens = t;
            }
            if !fragment.is_empty() {
                let delta = if fragment.starts_with(&last_snapshot) {
                    fragment[last_snapshot.len()..].to_string()
                } else {
                    fragment.clone()
                };
                last_snapshot = fragment;
                if delta.is_empty() {
                    continue;
                }
                full_text.push_str(&delta);
                tx.send(UiEvent::Delta(delta))
                    .await
                    .context("failed to send UI delta")?;
            }
        }
    }

    if !sse_buffer.trim().is_empty() {
        sse_buffer.push_str("\n\n");
        for event in extract_sse_events(&mut sse_buffer) {
            if event == "[DONE]" {
                continue;
            }
            let payload: serde_json::Value =
                serde_json::from_str(&event).context("invalid trailing SSE JSON payload")?;
            let (fragment, tokens) = extract_text_and_tokens(&payload);
            if let Some(t) = tokens {
                final_tokens = t;
            }
            if !fragment.is_empty() {
                let delta = if fragment.starts_with(&last_snapshot) {
                    fragment[last_snapshot.len()..].to_string()
                } else {
                    fragment.clone()
                };
                last_snapshot = fragment;
                if delta.is_empty() {
                    continue;
                }
                full_text.push_str(&delta);
                tx.send(UiEvent::Delta(delta))
                    .await
                    .context("failed to send trailing UI delta")?;
            }
        }
    }

    if full_text.is_empty() {
        anyhow::bail!("stream ended without model text");
    }

    Ok((full_text, final_tokens))
}

// ── UI helpers ─────────────────────────────────────────────────────────────────

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

// ── Setup page ─────────────────────────────────────────────────────────────────

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

// ── Application ────────────────────────────────────────────────────────────────

fn build_ui(app: &gtk::Application, rt: Arc<Runtime>) {
    apply_dark_theme();

    // Set the application icon
    if Path::new("assets/Logo.png").exists() {
        gtk::Window::set_default_icon_from_file("assets/Logo.png").ok();
    } else {
        gtk::Window::set_default_icon_name("com.example.gemini-lite");
    }

    let state = load_window_state();
    let window = gtk::ApplicationWindow::new(app);
    window.set_title(APP_TITLE);
    window.set_default_size(state.width, state.height);
    if let (Some(x), Some(y)) = (state.pos_x, state.pos_y) {
        window.move_(x, y);
    }

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(150);

    // ── Setup page ─────────────────────────────────────────────────────────
    let (setup_page, key_entry, save_btn_setup, error_label) = build_setup_page();
    stack.add_named(&setup_page, "setup");

    // ── Chat page ──────────────────────────────────────────────────────────
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

    // ── Shared state ───────────────────────────────────────────────────────
    let buffer = chat_view.buffer().expect("no buffer");
    let end_mark = buffer
        .create_mark(Some("end"), &buffer.end_iter(), false)
        .expect("text buffer must accept end mark");

    let api_key_holder: Rc<RefCell<String>> =
        Rc::new(RefCell::new(load_api_key().unwrap_or_default()));

    let history: Arc<Mutex<Vec<Content>>> = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = async_channel::unbounded::<UiEvent>();
    let http_client = Arc::new(reqwest::Client::new());

    // ── Chat send logic ────────────────────────────────────────────────────
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

            // Context pruning: Keep only the last 10 messages to save tokens.
            if hist.len() > 10 {
                let mut start = hist.len() - 10;
                // Gemini API requires the first message in the history to be from 'user'.
                if hist[start].role != "user" {
                    start += 1;
                }
                *hist = hist[start..].to_vec();
            }

            let snapshot = hist.clone();
            drop(hist);

            let tx = tx.clone();
            let client = http_client.clone();
            rt.spawn(async move {
                match stream_gemini(&client, &api_key, &selected_model, &snapshot, &tx).await {
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

    // ── Clear logic ────────────────────────────────────────────────────────
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

    // ── Async result handler ───────────────────────────────────────────────
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

    // ── Setup page submit ──────────────────────────────────────────────────
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
            match save_api_key(&raw) {
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

    // ── Initial page ───────────────────────────────────────────────────────
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
        if let Err(e) = save_window_state(&win_ref) {
            log::warn!("could not persist window state: {e:#}");
        }
        glib::Propagation::Proceed
    });

    window.show_all();
    log::info!("Gemini Lite started");
}

fn main() -> Result<()> {
    // Silent .env loading — convenience for `cargo run` dev sessions.
    dotenvy::dotenv().ok();
    env_logger::init();

    let rt = Arc::new(Runtime::new().context("failed to build Tokio runtime")?);

    let app = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::empty());
    app.connect_activate(move |app| {
        build_ui(app, rt.clone());
    });

    let code: i32 = app.run().into();
    if code != 0 {
        anyhow::bail!("application exited with code {code}");
    }
    Ok(())
}
