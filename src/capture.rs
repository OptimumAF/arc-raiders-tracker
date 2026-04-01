use std::{fs, path::PathBuf, thread, time::Duration};

use anyhow::{Context, Result, anyhow};
use screenshots::Screen;

use crate::{cache::cache_root_dir, now_unix_millis};

#[derive(Debug, Clone, Copy)]
pub(crate) struct CaptureRegionPercent {
    pub left_percent: u32,
    pub top_percent: u32,
    pub width_percent: u32,
    pub height_percent: u32,
}

pub(crate) fn capture_primary_inventory_screenshot(
    delay_ms: u64,
    region: CaptureRegionPercent,
) -> Result<PathBuf> {
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

    let capture = capture_screen_region(screen, region)?;

    let capture_dir = cache_root_dir().join("captures");
    fs::create_dir_all(&capture_dir)
        .with_context(|| format!("failed to create '{}'", capture_dir.display()))?;

    let path = capture_dir.join(format!("capture_{}.png", now_unix_millis()));
    capture
        .save(&path)
        .with_context(|| format!("failed to save '{}'", path.display()))?;
    Ok(path)
}

fn capture_screen_region(
    screen: Screen,
    region: CaptureRegionPercent,
) -> Result<screenshots::image::RgbaImage> {
    let screen_width = screen.display_info.width.max(1);
    let screen_height = screen.display_info.height.max(1);
    let left_percent = region.left_percent.min(99);
    let top_percent = region.top_percent.min(99);
    let max_width_percent = 100u32.saturating_sub(left_percent).max(1);
    let max_height_percent = 100u32.saturating_sub(top_percent).max(1);
    let width_percent = region.width_percent.clamp(1, max_width_percent);
    let height_percent = region.height_percent.clamp(1, max_height_percent);

    if left_percent == 0 && top_percent == 0 && width_percent == 100 && height_percent == 100 {
        return screen
            .capture()
            .context("failed to capture the primary display");
    }

    let left = ((screen_width as u64 * left_percent as u64) / 100) as i32;
    let top = ((screen_height as u64 * top_percent as u64) / 100) as i32;
    let width = ((screen_width as u64 * width_percent as u64) / 100)
        .max(1)
        .min(screen_width as u64) as u32;
    let height = ((screen_height as u64 * height_percent as u64) / 100)
        .max(1)
        .min(screen_height as u64) as u32;

    screen
        .capture_area(left, top, width, height)
        .context("failed to capture the configured inventory region on the primary display")
}
