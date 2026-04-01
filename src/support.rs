use std::{
    collections::HashSet,
    env,
    sync::{OnceLock, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AppRuntimeSettings {
    #[serde(default = "default_api_min_interval_ms")]
    pub api_min_interval_ms: u64,
    #[serde(default = "default_api_max_retries")]
    pub api_max_retries: usize,
    #[serde(default = "default_api_retry_base_ms")]
    pub api_retry_base_ms: u64,
    #[serde(default = "default_api_retry_max_ms")]
    pub api_retry_max_ms: u64,
    #[serde(default = "default_static_cache_ttl_seconds")]
    pub static_cache_ttl_seconds: u64,
    #[serde(default = "default_startup_user_cache_ttl_seconds")]
    pub startup_user_cache_ttl_seconds: u64,
    #[serde(default = "default_image_prefetch_count")]
    pub image_prefetch_count: usize,
    #[serde(default = "default_screenshot_capture_delay_ms")]
    pub screenshot_capture_delay_ms: u64,
    #[serde(default = "default_screenshot_grid_columns")]
    pub screenshot_grid_columns: u32,
    #[serde(default = "default_screenshot_grid_rows")]
    pub screenshot_grid_rows: u32,
    #[serde(default = "default_screenshot_slot_padding_percent")]
    pub screenshot_slot_padding_percent: u32,
    #[serde(default = "default_screenshot_quantity_ocr_enabled")]
    pub screenshot_quantity_ocr_enabled: bool,
    #[serde(default = "default_sell_exclude_weapons")]
    pub sell_exclude_weapons: bool,
    #[serde(default = "default_sell_exclude_types")]
    pub sell_exclude_types: HashSet<String>,
}

impl Default for AppRuntimeSettings {
    fn default() -> Self {
        Self::from_env()
    }
}

impl AppRuntimeSettings {
    pub(crate) fn from_env() -> Self {
        let api_retry_base_ms = first_non_empty_env(&["ARC_API_RETRY_BASE_MS"])
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(crate::DEFAULT_API_RETRY_BASE_MS);
        let api_retry_max_ms = first_non_empty_env(&["ARC_API_RETRY_MAX_MS"])
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(crate::DEFAULT_API_RETRY_MAX_MS)
            .max(api_retry_base_ms);

        Self {
            api_min_interval_ms: first_non_empty_env(&["ARC_API_MIN_INTERVAL_MS"])
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(crate::DEFAULT_API_MIN_INTERVAL_MS),
            api_max_retries: first_non_empty_env(&["ARC_API_MAX_RETRIES"])
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(crate::DEFAULT_API_MAX_RETRIES),
            api_retry_base_ms,
            api_retry_max_ms,
            static_cache_ttl_seconds: first_non_empty_env(&["ARC_STATIC_CACHE_TTL_SECONDS"])
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(crate::DEFAULT_STATIC_CACHE_TTL_SECONDS),
            startup_user_cache_ttl_seconds: first_non_empty_env(&[
                "ARC_STARTUP_USER_CACHE_TTL_SECONDS",
            ])
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(crate::DEFAULT_STARTUP_USER_CACHE_TTL_SECONDS),
            image_prefetch_count: first_non_empty_env(&["ARC_IMAGE_PREFETCH_COUNT"])
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(crate::DEFAULT_IMAGE_PREFETCH_COUNT),
            screenshot_capture_delay_ms: first_non_empty_env(&["ARC_SCREENSHOT_CAPTURE_DELAY_MS"])
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(default_screenshot_capture_delay_ms()),
            screenshot_grid_columns: first_non_empty_env(&["ARC_SCREENSHOT_GRID_COLUMNS"])
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(default_screenshot_grid_columns()),
            screenshot_grid_rows: first_non_empty_env(&["ARC_SCREENSHOT_GRID_ROWS"])
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(default_screenshot_grid_rows()),
            screenshot_slot_padding_percent: first_non_empty_env(&[
                "ARC_SCREENSHOT_SLOT_PADDING_PERCENT",
            ])
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(default_screenshot_slot_padding_percent()),
            screenshot_quantity_ocr_enabled: first_non_empty_env(&[
                "ARC_SCREENSHOT_QUANTITY_OCR_ENABLED",
            ])
            .and_then(|value| parse_env_bool(&value))
            .unwrap_or(default_screenshot_quantity_ocr_enabled()),
            sell_exclude_weapons: first_non_empty_env(&["ARC_SELL_EXCLUDE_WEAPONS"])
                .and_then(|value| parse_env_bool(&value))
                .unwrap_or(crate::DEFAULT_SELL_EXCLUDE_WEAPONS),
            sell_exclude_types: first_non_empty_env(&["ARC_SELL_EXCLUDE_TYPES"])
                .map(|raw| parse_csv_lower_set(&raw))
                .filter(|set| !set.is_empty())
                .unwrap_or_else(default_sell_exclude_types),
        }
    }
}

fn runtime_settings_lock() -> &'static RwLock<AppRuntimeSettings> {
    static RUNTIME_SETTINGS: OnceLock<RwLock<AppRuntimeSettings>> = OnceLock::new();
    RUNTIME_SETTINGS.get_or_init(|| RwLock::new(AppRuntimeSettings::from_env()))
}

pub(crate) fn runtime_settings_snapshot() -> AppRuntimeSettings {
    runtime_settings_lock()
        .read()
        .expect("runtime settings lock poisoned")
        .clone()
}

pub(crate) fn replace_runtime_settings(settings: AppRuntimeSettings) {
    *runtime_settings_lock()
        .write()
        .expect("runtime settings lock poisoned") = settings;
}

fn default_api_min_interval_ms() -> u64 {
    crate::DEFAULT_API_MIN_INTERVAL_MS
}

fn default_api_max_retries() -> usize {
    crate::DEFAULT_API_MAX_RETRIES
}

fn default_api_retry_base_ms() -> u64 {
    crate::DEFAULT_API_RETRY_BASE_MS
}

fn default_api_retry_max_ms() -> u64 {
    crate::DEFAULT_API_RETRY_MAX_MS
}

fn default_static_cache_ttl_seconds() -> u64 {
    crate::DEFAULT_STATIC_CACHE_TTL_SECONDS
}

fn default_startup_user_cache_ttl_seconds() -> u64 {
    crate::DEFAULT_STARTUP_USER_CACHE_TTL_SECONDS
}

fn default_image_prefetch_count() -> usize {
    crate::DEFAULT_IMAGE_PREFETCH_COUNT
}

fn default_screenshot_capture_delay_ms() -> u64 {
    2500
}

fn default_screenshot_grid_columns() -> u32 {
    10
}

fn default_screenshot_grid_rows() -> u32 {
    7
}

fn default_screenshot_slot_padding_percent() -> u32 {
    12
}

fn default_screenshot_quantity_ocr_enabled() -> bool {
    true
}

fn default_sell_exclude_weapons() -> bool {
    crate::DEFAULT_SELL_EXCLUDE_WEAPONS
}

fn default_sell_exclude_types() -> HashSet<String> {
    crate::DEFAULT_SELL_EXCLUDE_TYPES
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

pub(crate) fn default_theme_preference() -> String {
    "system".to_string()
}

pub(crate) fn default_show_planning_workspace() -> bool {
    true
}

pub(crate) fn mask_key(key: &str) -> String {
    if key.trim().is_empty() {
        return "(not set)".to_string();
    }

    if key.len() <= 10 {
        return "(loaded)".to_string();
    }

    let prefix = &key[..6];
    let suffix = &key[key.len().saturating_sub(4)..];
    format!("{prefix}***{suffix}")
}

pub(crate) fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(crate) fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn first_non_empty_env(keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| env::var(key).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

pub(crate) fn compiled_app_key() -> Option<String> {
    crate::COMPILED_APP_KEY
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(crate) fn normalize_theme_preference(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "dark" => "dark".to_string(),
        "light" => "light".to_string(),
        _ => "system".to_string(),
    }
}

pub(crate) fn next_theme_preference(current: &str) -> String {
    match current {
        "system" => "dark".to_string(),
        "dark" => "light".to_string(),
        _ => "system".to_string(),
    }
}

pub(crate) fn theme_preference_label(theme: &str) -> &'static str {
    match theme {
        "dark" => "Dark",
        "light" => "Light",
        _ => "System",
    }
}

pub(crate) fn parse_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" => Some(true),
        "0" | "false" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

pub(crate) fn parse_csv_lower_set(raw: &str) -> HashSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}
