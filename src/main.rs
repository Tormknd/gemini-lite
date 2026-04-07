use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use gio::prelude::*;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

const APP_ID: &str = "com.example.gemini-lite";
const APP_TITLE: &str = "Gemini Lite";
const API_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent";
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
struct GenerateRequest<'a> {
    contents: &'a [Content],
}

async fn call_gemini(
    client: &reqwest::Client,
    api_key: &str,
    history: &[Content],
) -> Result<String> {
    let body = GenerateRequest { contents: history };

    let resp = client
        .post(format!("{API_URL}?key={api_key}"))
        .json(&body)
        .send()
        .await
        .context("HTTP request failed")?;

    let status = resp.status();
    let json: serde_json::Value = resp.json().await.context("failed to parse API response")?;

    if !status.is_success() {
        let msg = json["error"]["message"]
            .as_str()
            .unwrap_or("unknown API error");
        anyhow::bail!("API {status}: {msg}");
    }

    json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("unexpected response shape: {json}"))
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
    let send_btn = gtk::Button::with_label("Send");
    input_row.pack_start(&msg_input, true, true, 0);
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
    let (tx, rx) = async_channel::unbounded::<Result<String>>();
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

        Rc::new(move || {
            let text = msg_input.text().trim().to_string();
            if text.is_empty() {
                return;
            }

            let api_key = api_key_holder.borrow().clone();
            if api_key.is_empty() {
                return;
            }

            msg_input.set_text("");
            send_btn.set_sensitive(false);
            append_message(&chat_view, &end_mark, "user", &text);

            let mut hist = history.lock().unwrap();
            hist.push(Content {
                role: "user".to_string(),
                parts: vec![Part { text }],
            });
            let snapshot = hist.clone();
            drop(hist);

            let tx = tx.clone();
            let client = http_client.clone();
            rt.spawn(async move {
                let result = call_gemini(&client, &api_key, &snapshot).await;
                tx.try_send(result).ok();
            });
        })
    };

    let s = Rc::clone(&send);
    send_btn.connect_clicked(move |_| s());
    msg_input.connect_activate(move |_| send());

    // ── Async result handler ───────────────────────────────────────────────
    {
        let chat_view = chat_view.clone();
        let end_mark = end_mark.clone();
        let send_btn_rx = send_btn.clone();

        glib::MainContext::default().spawn_local(async move {
            while let Ok(result) = rx.recv().await {
                send_btn_rx.set_sensitive(true);
                match result {
                    Ok(reply) => {
                        history.lock().unwrap().push(Content {
                            role: "model".to_string(),
                            parts: vec![Part {
                                text: reply.clone(),
                            }],
                        });
                        append_message(&chat_view, &end_mark, "model", &reply);
                    }
                    Err(e) => {
                        let buf = chat_view.buffer().expect("no buffer");
                        let mut iter = buf.end_iter();
                        buf.insert(&mut iter, &format!("\n⚠  Error: {e:#}\n"));
                        let end = buf.end_iter();
                        buf.move_mark(&end_mark, &end);
                        chat_view.scroll_to_mark(&end_mark, 0.0, false, 0.0, 1.0);
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

    window.connect_key_press_event(move |win, event| {
        if event.state().contains(gdk::ModifierType::CONTROL_MASK)
            && event.keyval() == gdk::keys::constants::q
        {
            win.close();
            return glib::Propagation::Stop;
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
