use std::sync::Arc;

use anyhow::{Context, Result};
use gio::prelude::*;
use tokio::runtime::Runtime;

const APP_ID: &str = "com.example.gemini-lite";

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    env_logger::init();

    let rt = Arc::new(Runtime::new().context("failed to build Tokio runtime")?);

    let app = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::empty());
    app.connect_activate(move |app| {
        gemini_lite::ui::build_ui(app, rt.clone());
    });

    let code: i32 = app.run().into();
    if code != 0 {
        anyhow::bail!("application exited with code {code}");
    }
    Ok(())
}
