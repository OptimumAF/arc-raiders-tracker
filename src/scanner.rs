use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use image::{
    DynamicImage, Rgba, RgbaImage,
    imageops::{self, FilterType},
};
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::{ArcData, runtime_settings_snapshot};

const FINGERPRINT_SIZE: u32 = 32;
const TEMPLATE_BG: [u8; 3] = [28, 34, 42];
const ABSOLUTE_MATCH_THRESHOLD: f32 = 0.080;
const SOFT_MATCH_THRESHOLD: f32 = 0.110;
const MATCH_RATIO_THRESHOLD: f32 = 1.08;

#[derive(Debug, Clone, Default)]
pub(crate) struct ScreenshotInventoryScanResult {
    pub counts: HashMap<String, u32>,
    pub files_scanned: usize,
    pub matched_slots: usize,
    pub unmatched_slots: usize,
    pub quantity_ocr_hits: usize,
    pub quantity_fallbacks: usize,
    pub merged_rows: usize,
    pub merged_slots: usize,
    pub overlap_rows_removed: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct ItemTemplate {
    item_id: String,
    fingerprint: Vec<f32>,
}

#[derive(Debug, Clone, Copy)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct OcrWord {
    text: String,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecognizedSlot {
    item_id: String,
    quantity: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum SlotState {
    #[default]
    Empty,
    Unknown,
    Item(RecognizedSlot),
}

#[derive(Debug, Clone, Default)]
struct ScreenshotGridScan {
    rows: Vec<Vec<SlotState>>,
    matched_slots: usize,
    unmatched_slots: usize,
    quantity_ocr_hits: usize,
    quantity_fallbacks: usize,
    warnings: Vec<String>,
}

pub(crate) fn select_inventory_screenshot_files() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
        .set_title("Select ARC Raiders inventory screenshots")
        .pick_files()
}

pub(crate) fn scan_inventory_screenshots(
    data: &ArcData,
    paths: &[PathBuf],
) -> Result<ScreenshotInventoryScanResult> {
    if paths.is_empty() {
        return Err(anyhow!("No screenshot files were selected."));
    }

    let images_dir = data.local_images_dir.as_ref().ok_or_else(|| {
        anyhow!(
            "Screenshot scanning requires local item images from vendor/arcraiders-data. Load static data from the local repo first."
        )
    })?;

    let (templates, template_warnings) = load_item_templates(images_dir, data);
    if templates.is_empty() {
        return Err(anyhow!(
            "No readable local item templates were found for screenshot matching."
        ));
    }

    let settings = runtime_settings_snapshot();
    let mut result = ScreenshotInventoryScanResult {
        warnings: template_warnings,
        ..Default::default()
    };
    let mut scans = Vec::with_capacity(paths.len());

    info!(
        files = paths.len(),
        templates = templates.len(),
        columns = settings.screenshot_grid_columns,
        rows = settings.screenshot_grid_rows,
        "screenshot_scan: starting inventory screenshot scan"
    );

    for path in paths {
        let image =
            image::open(path).with_context(|| format!("failed to open '{}'", path.display()))?;
        let ocr_words = if settings.screenshot_quantity_ocr_enabled {
            match ocr_words_for_image(path) {
                Ok(words) => words,
                Err(err) => {
                    warn!(
                        path = %path.display(),
                        error = %err,
                        "screenshot_scan: OCR unavailable for screenshot"
                    );
                    result.warnings.push(format!(
                        "{}: quantity OCR unavailable ({err})",
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("screenshot")
                    ));
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let file_result = scan_single_screenshot(&image, &templates, &ocr_words, &settings);
        result.files_scanned += 1;
        result.matched_slots += file_result.matched_slots;
        result.unmatched_slots += file_result.unmatched_slots;
        result.quantity_ocr_hits += file_result.quantity_ocr_hits;
        result.quantity_fallbacks += file_result.quantity_fallbacks;
        result
            .warnings
            .extend(file_result.warnings.iter().map(|warning| {
                format!(
                    "{}: {warning}",
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("screenshot")
                )
            }));
        scans.push(file_result);
    }

    let (merged_rows, overlap_rows_removed, merge_warnings) = merge_capture_rows(&scans);
    result.overlap_rows_removed = overlap_rows_removed;
    result.merged_rows = merged_rows.len();
    result.merged_slots = merged_rows
        .iter()
        .flatten()
        .filter(|slot| matches!(slot, SlotState::Item(_)))
        .count();
    result.counts = counts_from_rows(&merged_rows);
    result.warnings.extend(merge_warnings);

    info!(
        files_scanned = result.files_scanned,
        matched_slots = result.matched_slots,
        unmatched_slots = result.unmatched_slots,
        merged_rows = result.merged_rows,
        merged_slots = result.merged_slots,
        overlap_rows_removed = result.overlap_rows_removed,
        quantity_ocr_hits = result.quantity_ocr_hits,
        quantity_fallbacks = result.quantity_fallbacks,
        unique_items = result.counts.len(),
        "screenshot_scan: completed inventory screenshot scan"
    );

    Ok(result)
}

fn scan_single_screenshot(
    image: &DynamicImage,
    templates: &[ItemTemplate],
    ocr_words: &[OcrWord],
    settings: &crate::support::AppRuntimeSettings,
) -> ScreenshotGridScan {
    let columns = settings.screenshot_grid_columns.max(1) as usize;
    let rows = settings.screenshot_grid_rows.max(1) as usize;
    let mut result = ScreenshotGridScan {
        rows: vec![vec![SlotState::Empty; columns]; rows],
        ..Default::default()
    };

    for row in 0..rows {
        for column in 0..columns {
            let slot = slot_rect(
                image.width(),
                image.height(),
                columns as u32,
                rows as u32,
                column as u32,
                row as u32,
            );
            let icon_rect = icon_rect(slot, settings.screenshot_slot_padding_percent);
            let icon_image = crop_image(image, icon_rect);

            if looks_like_empty_slot(&icon_image) {
                continue;
            }

            let Some((item_id, best_score, second_score)) = match_item(&icon_image, templates)
            else {
                result.unmatched_slots += 1;
                result.rows[row][column] = SlotState::Unknown;
                continue;
            };

            let accepted = best_score <= ABSOLUTE_MATCH_THRESHOLD
                || (best_score <= SOFT_MATCH_THRESHOLD
                    && second_score / best_score.max(0.0001) >= MATCH_RATIO_THRESHOLD);

            if !accepted {
                debug!(
                    item_id,
                    best_score,
                    second_score,
                    row,
                    column,
                    "screenshot_scan: slot match below confidence threshold"
                );
                result.unmatched_slots += 1;
                result.rows[row][column] = SlotState::Unknown;
                continue;
            }

            let quantity = quantity_for_slot(ocr_words, slot).unwrap_or(1);
            if quantity > 1 {
                result.quantity_ocr_hits += 1;
            } else {
                result.quantity_fallbacks += 1;
            }

            result.rows[row][column] = SlotState::Item(RecognizedSlot {
                item_id: item_id.to_string(),
                quantity,
            });
            result.matched_slots += 1;
        }
    }

    if result.matched_slots == 0 {
        result.warnings.push(
            "No item slots were confidently matched. Check that captures are tightly cropped to the inventory grid and that grid rows/columns plus capture crop settings are configured correctly."
                .to_string(),
        );
    } else if result.unmatched_slots > 0 {
        result.warnings.push(format!(
            "{} occupied slot(s) could not be matched confidently.",
            result.unmatched_slots
        ));
    }

    result
}

fn load_item_templates(images_dir: &Path, data: &ArcData) -> (Vec<ItemTemplate>, Vec<String>) {
    let mut templates = Vec::new();
    let mut warnings = Vec::new();

    for item_id in data.items_by_id.keys() {
        let image_path = images_dir.join(format!("{item_id}.png"));
        if !image_path.exists() {
            continue;
        }

        match image::open(&image_path) {
            Ok(image) => {
                templates.push(ItemTemplate {
                    item_id: item_id.clone(),
                    fingerprint: image_fingerprint(&composite_template_image(&image)),
                });
            }
            Err(err) => {
                warn!(
                    path = %image_path.display(),
                    error = %err,
                    "screenshot_scan: skipping unreadable local template"
                );
                warnings.push(format!(
                    "Skipped unreadable local item template '{}'.",
                    image_path.display()
                ));
            }
        }
    }

    (templates, warnings)
}

fn counts_from_rows(rows: &[Vec<SlotState>]) -> HashMap<String, u32> {
    let mut counts = HashMap::<String, u32>::new();
    for row in rows {
        for slot in row {
            if let SlotState::Item(item) = slot {
                let entry = counts.entry(item.item_id.clone()).or_insert(0);
                *entry = (*entry).saturating_add(item.quantity);
            }
        }
    }
    counts
}

fn merge_capture_rows(scans: &[ScreenshotGridScan]) -> (Vec<Vec<SlotState>>, usize, Vec<String>) {
    let mut merged_rows = Vec::new();
    let mut warnings = Vec::new();
    let mut overlap_rows_removed = 0usize;

    for (index, scan) in scans.iter().enumerate() {
        if index == 0 {
            merged_rows.extend(scan.rows.clone());
            continue;
        }

        let overlap = find_row_overlap(&merged_rows, &scan.rows);
        if overlap == 0 {
            warnings.push(format!(
                "No overlapping inventory rows were detected between capture {} and {}. Scroll more gradually or increase session captures to reduce duplicate counting risk.",
                index,
                index + 1
            ));
            merged_rows.extend(scan.rows.clone());
            continue;
        }

        if overlap == 1 {
            warnings.push(format!(
                "Only 1 overlapping row was detected between capture {} and {}. Counts may be slightly noisy if recognition drifted.",
                index,
                index + 1
            ));
        }

        overlap_rows_removed = overlap_rows_removed.saturating_add(overlap);
        merged_rows.extend(scan.rows.iter().skip(overlap).cloned());
    }

    (merged_rows, overlap_rows_removed, warnings)
}

fn find_row_overlap(existing: &[Vec<SlotState>], next: &[Vec<SlotState>]) -> usize {
    let max_overlap = existing.len().min(next.len());
    for overlap in (1..=max_overlap).rev() {
        let mut useful_rows = 0usize;
        let overlap_valid = existing[existing.len() - overlap..]
            .iter()
            .zip(next.iter().take(overlap))
            .all(|(left, right)| {
                let (compatible, useful) = rows_are_compatible(left, right);
                if useful {
                    useful_rows += 1;
                }
                compatible
            });

        if overlap_valid && useful_rows > 0 {
            return overlap;
        }
    }

    0
}

fn rows_are_compatible(left: &[SlotState], right: &[SlotState]) -> (bool, bool) {
    if left.len() != right.len() {
        return (false, false);
    }

    let mut useful = false;
    for (left_slot, right_slot) in left.iter().zip(right) {
        match (left_slot, right_slot) {
            (SlotState::Empty, SlotState::Empty) => {}
            (SlotState::Unknown, SlotState::Unknown) => useful = true,
            (SlotState::Item(left_item), SlotState::Item(right_item))
                if left_item.item_id == right_item.item_id =>
            {
                useful = true;
            }
            _ => return (false, useful),
        }
    }

    (true, useful)
}

fn composite_template_image(image: &DynamicImage) -> RgbaImage {
    let rgba = image.to_rgba8();
    let mut composed = RgbaImage::new(rgba.width(), rgba.height());

    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = pixel[3] as f32 / 255.0;
        let out = if alpha <= 0.0 {
            Rgba([TEMPLATE_BG[0], TEMPLATE_BG[1], TEMPLATE_BG[2], 255])
        } else {
            let blend = |channel: u8, background: u8| -> u8 {
                ((channel as f32 * alpha) + (background as f32 * (1.0 - alpha))).round() as u8
            };
            Rgba([
                blend(pixel[0], TEMPLATE_BG[0]),
                blend(pixel[1], TEMPLATE_BG[1]),
                blend(pixel[2], TEMPLATE_BG[2]),
                255,
            ])
        };
        composed.put_pixel(x, y, out);
    }

    composed
}

fn image_fingerprint(image: &RgbaImage) -> Vec<f32> {
    let resized = imageops::resize(
        image,
        FINGERPRINT_SIZE,
        FINGERPRINT_SIZE,
        FilterType::Triangle,
    );
    let gray = DynamicImage::ImageRgba8(resized).grayscale().to_luma8();
    let mut values: Vec<f32> = gray.pixels().map(|pixel| pixel[0] as f32 / 255.0).collect();

    let mean = values.iter().copied().sum::<f32>() / values.len().max(1) as f32;
    for value in &mut values {
        *value -= mean;
    }

    values
}

fn looks_like_empty_slot(image: &RgbaImage) -> bool {
    let gray = DynamicImage::ImageRgba8(image.clone())
        .grayscale()
        .to_luma8();
    let values: Vec<f32> = gray.pixels().map(|pixel| pixel[0] as f32 / 255.0).collect();

    let mean = values.iter().copied().sum::<f32>() / values.len().max(1) as f32;
    let variance = values
        .iter()
        .map(|value| {
            let diff = *value - mean;
            diff * diff
        })
        .sum::<f32>()
        / values.len().max(1) as f32;

    mean < 0.14 && variance < 0.003
}

fn match_item<'a>(
    slot_image: &RgbaImage,
    templates: &'a [ItemTemplate],
) -> Option<(&'a str, f32, f32)> {
    let fingerprint = image_fingerprint(slot_image);
    let mut best_item = None;
    let mut best_score = f32::MAX;
    let mut second_score = f32::MAX;

    for template in templates {
        let score = mean_squared_error(&fingerprint, &template.fingerprint);
        if score < best_score {
            second_score = best_score;
            best_score = score;
            best_item = Some(template.item_id.as_str());
        } else if score < second_score {
            second_score = score;
        }
    }

    best_item.map(|item_id| (item_id, best_score, second_score))
}

fn mean_squared_error(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len()).max(1);
    let sum = a
        .iter()
        .zip(b.iter())
        .map(|(left, right)| {
            let diff = left - right;
            diff * diff
        })
        .sum::<f32>();
    sum / len as f32
}

fn crop_image(image: &DynamicImage, rect: Rect) -> RgbaImage {
    image
        .crop_imm(rect.x, rect.y, rect.width.max(1), rect.height.max(1))
        .to_rgba8()
}

fn slot_rect(
    image_width: u32,
    image_height: u32,
    columns: u32,
    rows: u32,
    column: u32,
    row: u32,
) -> Rect {
    let left = ((image_width as f32) * (column as f32 / columns as f32)).round() as u32;
    let right = ((image_width as f32) * ((column + 1) as f32 / columns as f32)).round() as u32;
    let top = ((image_height as f32) * (row as f32 / rows as f32)).round() as u32;
    let bottom = ((image_height as f32) * ((row + 1) as f32 / rows as f32)).round() as u32;

    Rect {
        x: left.min(image_width.saturating_sub(1)),
        y: top.min(image_height.saturating_sub(1)),
        width: right.saturating_sub(left).max(1),
        height: bottom.saturating_sub(top).max(1),
    }
}

fn icon_rect(slot: Rect, padding_percent: u32) -> Rect {
    let padding =
        (slot.width.min(slot.height) as f32 * (padding_percent as f32 / 100.0)).round() as u32;
    let bottom_trim = (slot.height as f32 * 0.18).round() as u32;
    let x = slot.x.saturating_add(padding);
    let y = slot.y.saturating_add(padding);
    let width = slot.width.saturating_sub(padding.saturating_mul(2)).max(1);
    let height = slot
        .height
        .saturating_sub(padding.saturating_mul(2))
        .saturating_sub(bottom_trim)
        .max(1);

    Rect {
        x,
        y,
        width,
        height,
    }
}

fn quantity_for_slot(words: &[OcrWord], slot: Rect) -> Option<u32> {
    let region_left = slot.x as f32 + slot.width as f32 * 0.52;
    let region_top = slot.y as f32 + slot.height as f32 * 0.52;
    let region_right = slot.x as f32 + slot.width as f32;
    let region_bottom = slot.y as f32 + slot.height as f32;

    let mut candidates: Vec<&OcrWord> = words
        .iter()
        .filter(|word| {
            let center_x = word.left + word.width / 2.0;
            let center_y = word.top + word.height / 2.0;
            center_x >= region_left
                && center_x <= region_right
                && center_y >= region_top
                && center_y <= region_bottom
                && word.text.chars().any(|ch| ch.is_ascii_digit())
        })
        .collect();

    candidates.sort_by(|left, right| {
        left.left
            .partial_cmp(&right.left)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let digits: String = candidates
        .iter()
        .flat_map(|word| word.text.chars())
        .filter(|ch| ch.is_ascii_digit())
        .collect();

    digits.parse::<u32>().ok().filter(|value| *value > 0)
}

#[cfg(windows)]
fn ocr_words_for_image(path: &Path) -> Result<Vec<OcrWord>> {
    let script = r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Runtime.WindowsRuntime
[Windows.Storage.StorageFile, Windows.Storage, ContentType = WindowsRuntime] | Out-Null
[Windows.Graphics.Imaging.BitmapDecoder, Windows.Graphics.Imaging, ContentType = WindowsRuntime] | Out-Null
[Windows.Media.Ocr.OcrEngine, Windows.Media.Ocr, ContentType = WindowsRuntime] | Out-Null

$asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.IsGenericMethod -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
} | Select-Object -First 1)

function AwaitWinRt($Operation) {
    $resultType = $Operation.GetType().GenericTypeArguments[0]
    $task = $asTask.MakeGenericMethod($resultType).Invoke($null, @($Operation))
    $task.Wait(-1) | Out-Null
    $task.Result
}

$path = $env:ARC_CLEANER_OCR_IMAGE
$file = AwaitWinRt([Windows.Storage.StorageFile]::GetFileFromPathAsync($path))
$stream = AwaitWinRt($file.OpenAsync([Windows.Storage.FileAccessMode]::Read))
$decoder = AwaitWinRt([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream))
$bitmap = AwaitWinRt($decoder.GetSoftwareBitmapAsync())
$engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages()
if ($null -eq $engine) {
    throw 'No Windows OCR language is available for the current user profile.'
}
$result = AwaitWinRt($engine.RecognizeAsync($bitmap))
$words = foreach ($line in $result.Lines) {
    foreach ($word in $line.Words) {
        [pscustomobject]@{
            text = $word.Text
            left = $word.BoundingRect.X
            top = $word.BoundingRect.Y
            width = $word.BoundingRect.Width
            height = $word.BoundingRect.Height
        }
    }
}
@($words) | ConvertTo-Json -Compress
"#;

    let mut utf16 = Vec::new();
    for unit in script.encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }

    let output = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-EncodedCommand")
        .arg(BASE64.encode(utf16))
        .env("ARC_CLEANER_OCR_IMAGE", path)
        .output()
        .context("failed to launch PowerShell for Windows OCR")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "Windows OCR failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Ok(Vec::new());
    }

    serde_json::from_str(&stdout).context("failed to parse Windows OCR JSON output")
}

#[cfg(not(windows))]
fn ocr_words_for_image(_path: &Path) -> Result<Vec<OcrWord>> {
    Err(anyhow!(
        "Windows OCR is only available on Windows builds at the moment."
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        RecognizedSlot, ScreenshotGridScan, SlotState, counts_from_rows, merge_capture_rows,
    };

    fn item(id: &str, quantity: u32) -> SlotState {
        SlotState::Item(RecognizedSlot {
            item_id: id.to_string(),
            quantity,
        })
    }

    #[test]
    fn merge_capture_rows_removes_overlapping_rows() {
        let first = ScreenshotGridScan {
            rows: vec![
                vec![item("a", 1), item("b", 1)],
                vec![item("c", 1), item("d", 1)],
                vec![item("e", 1), item("f", 1)],
            ],
            ..Default::default()
        };
        let second = ScreenshotGridScan {
            rows: vec![
                vec![item("c", 1), item("d", 1)],
                vec![item("e", 1), item("f", 1)],
                vec![item("g", 1), item("h", 1)],
            ],
            ..Default::default()
        };

        let (rows, overlap_removed, warnings) = merge_capture_rows(&[first, second]);
        assert_eq!(overlap_removed, 2);
        assert!(warnings.is_empty());
        assert_eq!(rows.len(), 4);

        let counts = counts_from_rows(&rows);
        assert_eq!(counts.get("a"), Some(&1));
        assert_eq!(counts.get("h"), Some(&1));
        assert_eq!(counts.values().sum::<u32>(), 8);
    }

    #[test]
    fn merge_capture_rows_warns_when_no_overlap_exists() {
        let first = ScreenshotGridScan {
            rows: vec![vec![item("a", 1)], vec![item("b", 1)]],
            ..Default::default()
        };
        let second = ScreenshotGridScan {
            rows: vec![vec![item("x", 1)], vec![item("y", 1)]],
            ..Default::default()
        };

        let (rows, overlap_removed, warnings) = merge_capture_rows(&[first, second]);
        assert_eq!(overlap_removed, 0);
        assert_eq!(rows.len(), 4);
        assert_eq!(warnings.len(), 1);
    }
}
