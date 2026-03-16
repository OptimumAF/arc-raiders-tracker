use std::{
    collections::HashSet,
    env,
    time::{SystemTime, UNIX_EPOCH},
};

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
