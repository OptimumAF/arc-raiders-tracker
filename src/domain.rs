use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArcData {
    pub items_by_id: HashMap<String, Item>,
    pub craftable_items: Vec<Item>,
    pub quests: Vec<Quest>,
    pub hideout_modules: Vec<HideoutModule>,
    pub projects: Vec<Project>,
    pub local_images_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    #[serde(default, rename = "type")]
    pub item_type: Option<String>,
    #[serde(default)]
    pub is_weapon: bool,
    #[serde(default)]
    pub name: HashMap<String, String>,
    #[serde(default)]
    pub recipe: Option<HashMap<String, u32>>,
    #[serde(default)]
    pub craft_quantity: Option<u32>,
    #[serde(default)]
    pub image_filename: Option<String>,
    #[serde(default)]
    pub value: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ItemRequirement {
    pub item_id: String,
    pub quantity: u32,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Quest {
    pub id: String,
    #[serde(default)]
    pub name: HashMap<String, String>,
    #[serde(default)]
    pub required_item_ids: Vec<ItemRequirement>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HideoutLevel {
    pub level: u32,
    #[serde(default)]
    pub requirement_item_ids: Vec<ItemRequirement>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HideoutModule {
    pub id: String,
    #[serde(default)]
    pub name: HashMap<String, String>,
    #[serde(default)]
    pub max_level: u32,
    #[serde(default)]
    pub levels: Vec<HideoutLevel>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPhase {
    pub phase: u32,
    #[serde(default)]
    pub requirement_item_ids: Vec<ItemRequirement>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    #[serde(default)]
    pub name: HashMap<String, String>,
    #[serde(default)]
    pub phases: Vec<ProjectPhase>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackedCraft {
    pub item_id: String,
    pub quantity: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackedHideout {
    pub module_id: String,
    pub target_level: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackedProject {
    pub project_id: String,
    #[serde(default = "default_start_phase")]
    pub start_phase: u32,
    pub target_phase: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NeedRow {
    pub name: String,
    pub image_src: String,
    pub required: u32,
    pub have: u32,
    pub missing: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SellRow {
    pub name: String,
    pub image_src: String,
    pub quantity: u32,
    pub total_value: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dashboard {
    pub needs: Vec<NeedRow>,
    pub keep: Vec<NeedRow>,
    pub sell: Vec<SellRow>,
}

pub fn aggregate_requirements(
    data: &ArcData,
    tracked_crafts: &[TrackedCraft],
    tracked_quests: &[String],
    tracked_hideout: &[TrackedHideout],
    tracked_projects: &[TrackedProject],
) -> HashMap<String, u32> {
    let mut totals = HashMap::new();

    for craft in tracked_crafts {
        if let Some(item) = data.items_by_id.get(&craft.item_id)
            && let Some(recipe) = item.recipe.as_ref()
            && !recipe.is_empty()
        {
            let output_qty = item.craft_quantity.unwrap_or(1).max(1);
            let runs = craft.quantity.div_ceil(output_qty);

            for (ingredient_id, qty) in recipe {
                add_requirement(&mut totals, ingredient_id, qty.saturating_mul(runs));
            }
            continue;
        }

        add_requirement(&mut totals, &craft.item_id, craft.quantity);
    }

    for quest_id in tracked_quests {
        if let Some(quest) = data.quests.iter().find(|quest| quest.id == *quest_id) {
            for req in &quest.required_item_ids {
                add_requirement(&mut totals, &req.item_id, req.quantity);
            }
        }
    }

    for hideout_entry in tracked_hideout {
        if let Some(module) = data
            .hideout_modules
            .iter()
            .find(|module| module.id == hideout_entry.module_id)
        {
            for level in module
                .levels
                .iter()
                .filter(|level| level.level <= hideout_entry.target_level)
            {
                for req in &level.requirement_item_ids {
                    add_requirement(&mut totals, &req.item_id, req.quantity);
                }
            }
        }
    }

    for project_entry in tracked_projects {
        if let Some(project) = data
            .projects
            .iter()
            .find(|project| project.id == project_entry.project_id)
        {
            for phase in project.phases.iter().filter(|phase| {
                phase.phase >= project_entry.start_phase
                    && phase.phase <= project_entry.target_phase
            }) {
                for req in &phase.requirement_item_ids {
                    add_requirement(&mut totals, &req.item_id, req.quantity);
                }
            }
        }
    }

    totals
}

pub fn build_dashboard<FImage, FExclude>(
    data: &ArcData,
    required_items: &HashMap<String, u32>,
    inventory: &HashMap<String, u32>,
    loadout_counts: &HashMap<String, u32>,
    allow_sell_recommendations: bool,
    item_image_src: FImage,
    is_excluded_from_sell: FExclude,
) -> Dashboard
where
    FImage: Fn(&ArcData, &str) -> String,
    FExclude: Fn(&ArcData, &str) -> bool,
{
    let mut needs = Vec::new();
    let mut keep = Vec::new();
    let mut sell = Vec::new();

    let mut keep_targets: HashMap<String, u32> = required_items.clone();
    for (item_id, loadout_qty) in loadout_counts {
        let entry = keep_targets.entry(item_id.clone()).or_insert(0);
        *entry = entry.saturating_add(*loadout_qty);
    }

    for (item_id, keep_qty) in &keep_targets {
        let have = *inventory.get(item_id).unwrap_or(&0);
        let row = NeedRow {
            name: item_name(data, item_id),
            image_src: item_image_src(data, item_id),
            required: *keep_qty,
            have,
            missing: keep_qty.saturating_sub(have),
        };

        if required_items.contains_key(item_id) && row.missing > 0 {
            needs.push(row.clone());
        }
        keep.push(row);
    }

    if allow_sell_recommendations {
        for (item_id, quantity) in inventory {
            if is_excluded_from_sell(data, item_id) {
                continue;
            }

            let keep_qty = *keep_targets.get(item_id).unwrap_or(&0);
            if *quantity <= keep_qty {
                continue;
            }

            let unit_value = data
                .items_by_id
                .get(item_id)
                .and_then(|item| item.value)
                .unwrap_or(0);
            let sell_qty = quantity.saturating_sub(keep_qty);

            sell.push(SellRow {
                name: item_name(data, item_id),
                image_src: item_image_src(data, item_id),
                quantity: sell_qty,
                total_value: (sell_qty as u64) * (unit_value as u64),
            });
        }
    }

    needs.sort_by(|a, b| b.missing.cmp(&a.missing).then(a.name.cmp(&b.name)));
    keep.sort_by(|a, b| a.name.cmp(&b.name));
    sell.sort_by(|a, b| b.total_value.cmp(&a.total_value).then(a.name.cmp(&b.name)));

    Dashboard { needs, keep, sell }
}

pub fn add_requirement(totals: &mut HashMap<String, u32>, item_id: &str, qty: u32) {
    let entry = totals.entry(item_id.to_string()).or_insert(0);
    *entry = entry.saturating_add(qty);
}

pub fn merge_counts(base: &mut HashMap<String, u32>, other: HashMap<String, u32>) {
    for (item_id, qty) in other {
        let entry = base.entry(item_id).or_insert(0);
        *entry = entry.saturating_add(qty);
    }
}

pub fn item_name(data: &ArcData, item_id: &str) -> String {
    data.items_by_id
        .get(item_id)
        .map(|item| localized_en(&item.name))
        .unwrap_or_else(|| item_id.to_string())
}

pub fn quest_name(data: &ArcData, quest_id: &str) -> String {
    data.quests
        .iter()
        .find(|quest| quest.id == quest_id)
        .map(|quest| localized_en(&quest.name))
        .unwrap_or_else(|| quest_id.to_string())
}

pub fn hideout_name(data: &ArcData, module_id: &str) -> String {
    data.hideout_modules
        .iter()
        .find(|module| module.id == module_id)
        .map(|module| localized_en(&module.name))
        .unwrap_or_else(|| module_id.to_string())
}

pub fn project_name(data: &ArcData, project_id: &str) -> String {
    data.projects
        .iter()
        .find(|project| project.id == project_id)
        .map(|project| localized_en(&project.name))
        .unwrap_or_else(|| project_id.to_string())
}

pub fn module_max_level(data: &ArcData, module_id: &str) -> Option<u32> {
    data.hideout_modules
        .iter()
        .find(|module| module.id == module_id)
        .map(|module| {
            let level_max = module
                .levels
                .iter()
                .map(|level| level.level)
                .max()
                .unwrap_or(0);
            level_max.max(module.max_level)
        })
}

pub fn project_max_phase(data: &ArcData, project_id: &str) -> Option<u32> {
    data.projects
        .iter()
        .find(|project| project.id == project_id)
        .map(|project| {
            project
                .phases
                .iter()
                .map(|phase| phase.phase)
                .max()
                .unwrap_or(1)
        })
}

pub fn localized_en(value: &HashMap<String, String>) -> String {
    value
        .get("en")
        .cloned()
        .or_else(|| value.values().next().cloned())
        .unwrap_or_else(|| "Unknown".to_string())
}

pub fn parse_u32_or_default(raw: &str, default: u32) -> u32 {
    raw.trim().parse::<u32>().unwrap_or(default)
}

fn default_start_phase() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn localized_name(name: &str) -> HashMap<String, String> {
        HashMap::from([("en".to_string(), name.to_string())])
    }

    fn item(id: &str, name: &str, value: u32) -> Item {
        Item {
            id: id.to_string(),
            item_type: Some("Material".to_string()),
            is_weapon: false,
            name: localized_name(name),
            recipe: None,
            craft_quantity: None,
            image_filename: None,
            value: Some(value),
        }
    }

    fn requirement(item_id: &str, quantity: u32) -> ItemRequirement {
        ItemRequirement {
            item_id: item_id.to_string(),
            quantity,
        }
    }

    #[test]
    fn aggregate_requirements_expands_craft_recipe_by_output_quantity() {
        let mut crafted = item("crafted", "Crafted Item", 100);
        crafted.recipe = Some(HashMap::from([("wire".to_string(), 3)]));
        crafted.craft_quantity = Some(2);

        let data = ArcData {
            items_by_id: HashMap::from([
                ("crafted".to_string(), crafted),
                ("wire".to_string(), item("wire", "Wire", 10)),
            ]),
            ..ArcData::default()
        };

        let totals = aggregate_requirements(
            &data,
            &[TrackedCraft {
                item_id: "crafted".to_string(),
                quantity: 5,
            }],
            &[],
            &[],
            &[],
        );

        assert_eq!(totals.get("wire"), Some(&9));
        assert!(!totals.contains_key("crafted"));
    }

    #[test]
    fn aggregate_requirements_accumulates_quest_hideout_and_project_needs() {
        let data = ArcData {
            items_by_id: HashMap::from([("scrap".to_string(), item("scrap", "Scrap", 5))]),
            quests: vec![Quest {
                id: "quest-1".to_string(),
                name: localized_name("Quest 1"),
                required_item_ids: vec![requirement("scrap", 2)],
            }],
            hideout_modules: vec![HideoutModule {
                id: "hideout-1".to_string(),
                name: localized_name("Hideout 1"),
                max_level: 2,
                levels: vec![
                    HideoutLevel {
                        level: 1,
                        requirement_item_ids: vec![requirement("scrap", 1)],
                    },
                    HideoutLevel {
                        level: 2,
                        requirement_item_ids: vec![requirement("scrap", 4)],
                    },
                ],
            }],
            projects: vec![Project {
                id: "project-1".to_string(),
                name: localized_name("Project 1"),
                phases: vec![
                    ProjectPhase {
                        phase: 1,
                        requirement_item_ids: vec![requirement("scrap", 3)],
                    },
                    ProjectPhase {
                        phase: 2,
                        requirement_item_ids: vec![requirement("scrap", 6)],
                    },
                ],
            }],
            ..ArcData::default()
        };

        let totals = aggregate_requirements(
            &data,
            &[],
            &["quest-1".to_string()],
            &[TrackedHideout {
                module_id: "hideout-1".to_string(),
                target_level: 2,
            }],
            &[TrackedProject {
                project_id: "project-1".to_string(),
                start_phase: 2,
                target_phase: 2,
            }],
        );

        assert_eq!(totals.get("scrap"), Some(&13));
    }

    #[test]
    fn build_dashboard_respects_keep_targets_and_sell_suggestions() {
        let data = ArcData {
            items_by_id: HashMap::from([
                ("scrap".to_string(), item("scrap", "Scrap", 5)),
                ("battery".to_string(), item("battery", "Battery", 20)),
            ]),
            ..ArcData::default()
        };
        let required = HashMap::from([("scrap".to_string(), 4), ("battery".to_string(), 2)]);
        let inventory = HashMap::from([("scrap".to_string(), 10), ("battery".to_string(), 1)]);
        let loadout = HashMap::from([("scrap".to_string(), 1)]);

        let dashboard = build_dashboard(
            &data,
            &required,
            &inventory,
            &loadout,
            true,
            |_, _| String::new(),
            |_, _| false,
        );

        assert_eq!(dashboard.needs.len(), 1);
        assert_eq!(dashboard.needs[0].name, "Battery");
        assert_eq!(dashboard.needs[0].missing, 1);

        assert_eq!(dashboard.keep.len(), 2);
        assert_eq!(dashboard.sell.len(), 1);
        assert_eq!(dashboard.sell[0].name, "Scrap");
        assert_eq!(dashboard.sell[0].quantity, 5);
        assert_eq!(dashboard.sell[0].total_value, 25);
    }
}
