use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::domain::{HideoutModule, Item, Project, Quest};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemsResponse {
    pub items: Vec<Item>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestsResponse {
    pub quests: HashMap<String, Quest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HideoutResponse {
    pub hideout_modules: HashMap<String, HideoutModule>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectsResponse {
    pub projects: HashMap<String, Project>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserProfileInfo {
    pub username: String,
    pub level: Option<u32>,
    pub member_since: Option<String>,
}

pub fn unwrap_data_ref(value: &Value) -> &Value {
    value.get("data").unwrap_or(value)
}

pub fn parse_user_profile(value: &Value) -> Option<UserProfileInfo> {
    let root = value
        .get("profile")
        .or_else(|| value.get("user"))
        .unwrap_or(value);

    let username = root
        .get("username")
        .or_else(|| root.get("name"))
        .and_then(value_as_string)
        .or_else(|| {
            root.get("user")
                .and_then(|user| user.get("username"))
                .and_then(value_as_string)
        })?;

    let level = root
        .get("level")
        .and_then(value_as_u32)
        .or_else(|| root.get("playerLevel").and_then(value_as_u32));

    let member_since = root
        .get("memberSince")
        .or_else(|| root.get("createdAt"))
        .or_else(|| root.get("member_since"))
        .and_then(value_as_string);

    Some(UserProfileInfo {
        username,
        level,
        member_since,
    })
}

pub fn extract_known_ids(
    value: &Value,
    known_ids: &HashSet<String>,
    preferred_keys: &[&str],
) -> HashSet<String> {
    fn walk(
        value: &Value,
        known_ids: &HashSet<String>,
        preferred_keys: &[&str],
        out: &mut HashSet<String>,
    ) {
        match value {
            Value::Array(items) => {
                for item in items {
                    walk(item, known_ids, preferred_keys, out);
                }
            }
            Value::Object(map) => {
                for key in preferred_keys {
                    if let Some(raw_id) = map.get(*key).and_then(value_as_string)
                        && let Some(mapped) = map_to_known_id(&raw_id, known_ids)
                    {
                        out.insert(mapped);
                    }
                }

                if let Some(raw_id) = map.get("id").and_then(value_as_string)
                    && let Some(mapped) = map_to_known_id(&raw_id, known_ids)
                {
                    out.insert(mapped);
                }

                for (key, child) in map {
                    if let Some(mapped) = map_to_known_id(key, known_ids)
                        && (child.is_object() || child.is_array())
                    {
                        out.insert(mapped);
                    }

                    if key.to_ascii_lowercase().contains("quest")
                        && let Some(ids) = child.as_array()
                    {
                        for id in ids {
                            if let Some(raw) = value_as_string(id)
                                && let Some(mapped) = map_to_known_id(&raw, known_ids)
                            {
                                out.insert(mapped);
                            }
                        }
                    }

                    walk(child, known_ids, preferred_keys, out);
                }
            }
            _ => {}
        }
    }

    let mut out = HashSet::new();
    walk(value, known_ids, preferred_keys, &mut out);
    out
}

pub fn extract_progress_level_map(
    value: &Value,
    known_ids: &HashSet<String>,
    preferred_id_keys: &[&str],
    preferred_level_keys: &[&str],
    completed_level_array_keys: &[&str],
) -> HashMap<String, u32> {
    fn parse_level_from_object(
        map: &Map<String, Value>,
        preferred_level_keys: &[&str],
        completed_level_array_keys: &[&str],
    ) -> Option<u32> {
        for key in preferred_level_keys {
            if let Some(level) = map.get(*key).and_then(value_as_u32) {
                return Some(level);
            }
        }

        for key in completed_level_array_keys {
            if let Some(values) = map.get(*key).and_then(|v| v.as_array()) {
                let mut max_level = 0u32;
                for value in values {
                    if let Some(level) = value_as_u32(value) {
                        max_level = max_level.max(level);
                    }
                }
                if max_level > 0 {
                    return Some(max_level);
                }
            }
        }

        None
    }

    fn walk(
        value: &Value,
        known_ids: &HashSet<String>,
        preferred_id_keys: &[&str],
        preferred_level_keys: &[&str],
        completed_level_array_keys: &[&str],
        out: &mut HashMap<String, u32>,
    ) {
        match value {
            Value::Array(items) => {
                for item in items {
                    walk(
                        item,
                        known_ids,
                        preferred_id_keys,
                        preferred_level_keys,
                        completed_level_array_keys,
                        out,
                    );
                }
            }
            Value::Object(map) => {
                let mut matched_id: Option<String> = None;

                for key in preferred_id_keys {
                    if let Some(raw_id) = map.get(*key).and_then(value_as_string)
                        && let Some(mapped) = map_to_known_id(&raw_id, known_ids)
                    {
                        matched_id = Some(mapped);
                        break;
                    }
                }

                if matched_id.is_none()
                    && let Some(raw_id) = map.get("id").and_then(value_as_string)
                {
                    matched_id = map_to_known_id(&raw_id, known_ids);
                }

                if let Some(id) = matched_id
                    && let Some(level) = parse_level_from_object(
                        map,
                        preferred_level_keys,
                        completed_level_array_keys,
                    )
                {
                    let entry = out.entry(id).or_insert(0);
                    *entry = (*entry).max(level);
                }

                for (key, child) in map {
                    if let Some(mapped) = map_to_known_id(key, known_ids)
                        && let Some(child_map) = child.as_object()
                        && let Some(level) = parse_level_from_object(
                            child_map,
                            preferred_level_keys,
                            completed_level_array_keys,
                        )
                    {
                        let entry = out.entry(mapped).or_insert(0);
                        *entry = (*entry).max(level);
                    }

                    walk(
                        child,
                        known_ids,
                        preferred_id_keys,
                        preferred_level_keys,
                        completed_level_array_keys,
                        out,
                    );
                }
            }
            _ => {}
        }
    }

    let mut out = HashMap::new();
    walk(
        value,
        known_ids,
        preferred_id_keys,
        preferred_level_keys,
        completed_level_array_keys,
        &mut out,
    );
    out
}

pub fn map_to_known_id(raw: &str, known_ids: &HashSet<String>) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    if known_ids.contains(raw) {
        return Some(raw.to_string());
    }

    if let Some((_, suffix)) = raw.split_once('_')
        && known_ids.contains(suffix)
    {
        return Some(suffix.to_string());
    }

    None
}

pub fn has_next_page(payload: &Value, current_page: u32) -> bool {
    let Some(pagination) = payload.get("meta").or_else(|| payload.get("pagination")) else {
        return false;
    };

    if let Some(has_next) = pagination
        .get("hasNextPage")
        .or_else(|| pagination.get("has_next_page"))
        .and_then(value_as_bool)
    {
        return has_next;
    }

    if let Some(total_pages) = pagination
        .get("totalPages")
        .or_else(|| pagination.get("total_pages"))
        .and_then(value_as_u32)
    {
        return current_page < total_pages;
    }

    if let (Some(total), Some(per_page)) = (
        pagination
            .get("total")
            .or_else(|| pagination.get("totalItems"))
            .and_then(value_as_u32),
        pagination
            .get("perPage")
            .or_else(|| pagination.get("per_page"))
            .and_then(value_as_u32),
    ) {
        return current_page.saturating_mul(per_page) < total;
    }

    false
}

pub fn extract_inventory_counts(root: &Value) -> HashMap<String, u32> {
    let mut counts = HashMap::new();
    walk_inventory_json(root, &mut counts);
    counts
}

fn walk_inventory_json(value: &Value, counts: &mut HashMap<String, u32>) {
    match value {
        Value::Object(map) => {
            if let Some((item_id, quantity)) = parse_inventory_entry(map) {
                let entry = counts.entry(item_id).or_insert(0);
                *entry = entry.saturating_add(quantity);
            }

            for child in map.values() {
                walk_inventory_json(child, counts);
            }
        }
        Value::Array(items) => {
            for child in items {
                walk_inventory_json(child, counts);
            }
        }
        _ => {}
    }
}

fn parse_inventory_entry(map: &Map<String, Value>) -> Option<(String, u32)> {
    let quantity = ["quantity", "count", "amount", "stack", "stackSize"]
        .iter()
        .find_map(|key| map.get(*key).and_then(value_as_u32))?;

    let item_id = map
        .get("itemId")
        .and_then(value_as_string)
        .or_else(|| map.get("item_id").and_then(value_as_string))
        .or_else(|| {
            map.get("item").and_then(|item| {
                value_as_string(item)
                    .or_else(|| item.get("id").and_then(value_as_string))
                    .or_else(|| item.get("itemId").and_then(value_as_string))
            })
        })
        .or_else(|| {
            if map.contains_key("quantity")
                || map.contains_key("count")
                || map.contains_key("amount")
            {
                map.get("id").and_then(value_as_string)
            } else {
                None
            }
        })?;

    Some((item_id, quantity))
}

fn value_as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn value_as_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(num) => num.as_u64().and_then(|v| u32::try_from(v).ok()),
        Value::String(text) => text.parse::<u32>().ok(),
        _ => None,
    }
}

fn value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        _ => None,
    }
}
