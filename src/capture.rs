use std::{fs, path::PathBuf, thread, time::Duration};

use anyhow::{Context, Result, anyhow};
use screenshots::Screen;

use crate::{cache::cache_root_dir, now_unix_millis};

pub(crate) fn capture_primary_inventory_screenshot(delay_ms: u64) -> Result<PathBuf> {
    if delay_ms > 0 {
        thread::sleep(Duration::from_millis(delay_ms));
    }

    let screens = Screen::all().context("failed to enumerate displays")?;
    let screen = screens
        .iter()
        .find(|screen| screen.display_info.is_primary)
        .copied()
        .or_else(|| screens.first().copied())
        .ok_or_else(|| anyhow!("no displays are available for capture"))?;

    let capture = screen
        .capture()
        .context("failed to capture the primary display")?;

    let capture_dir = cache_root_dir().join("captures");
    fs::create_dir_all(&capture_dir)
        .with_context(|| format!("failed to create '{}'", capture_dir.display()))?;

    let path = capture_dir.join(format!("capture_{}.png", now_unix_millis()));
    capture
        .save(&path)
        .with_context(|| format!("failed to save '{}'", path.display()))?;
    Ok(path)
}
