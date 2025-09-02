use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::Result;
use assistant_tool::{Tool, ToolResult};
use gpui::{AnyWindowHandle, App, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{fmt::Write, sync::Arc};
use ui::IconName;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProjectUpdatesToolInput {}

pub struct ProjectNotificationsTool;

impl Tool for ProjectNotificationsTool {
    fn name(&self) -> String {
        "project_notifications".to_string()
    }

    fn needs_confirmation(&self, _: &serde_json::Value, _: &Entity<Project>, _: &App) -> bool {
        false
    }
    fn may_perform_edits(&self) -> bool {
        false
    }
    fn description(&self) -> String {
        include_str!("./project_notifications_tool/description.md").to_string()
    }

    fn icon(&self) -> IconName {
        IconName::ToolNotification
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<ProjectUpdatesToolInput>(format)
    }

    fn ui_text(&self, _input: &serde_json::Value) -> String {
        "Check project notifications".into()
    }

    fn run(
        self: Arc<Self>,
        _input: serde_json::Value,
        _request: Arc<LanguageModelRequest>,
        _project: Entity<Project>,
        action_log: Entity<ActionLog>,
        _model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        let Some(user_edits_diff) =
            action_log.update(cx, |log, cx| log.flush_unnotified_user_edits(cx))
        else {
            return result("No new notifications");
        };

        // NOTE: Changes to this prompt require a symmetric update in the LLM Worker
        const HEADER: &str = include_str!("./project_notifications_tool/prompt_header.txt");
        const MAX_BYTES: usize = 8000;
        let diff = fit_patch_to_size(&user_edits_diff, MAX_BYTES);
        result(&format!("{HEADER}\n\n```diff\n{diff}\n```\n").replace("\r\n", "\n"))
    }
}

fn result(response: &str) -> ToolResult {
    Task::ready(Ok(response.to_string().into())).into()
}

/// Make sure that the patch fits into the size limit (in bytes).
/// Compress the patch by omitting some parts if needed.
/// Unified diff format is assumed.
fn fit_patch_to_size(patch: &str, max_size: usize) -> String {
    if patch.len() <= max_size {
        return patch.to_string();
    }

    // Compression level 1: remove context lines in diff bodies, but
    // leave the counts and positions of inserted/deleted lines
    let mut current_size = patch.len();
    let mut file_patches = split_patch(patch);
    file_patches.sort_by_key(|patch| patch.len());
    let compressed_patches = file_patches
        .iter()
        .rev()
        .map(|patch| {
            if current_size > max_size {
                let compressed = compress_patch(patch).unwrap_or_else(|_| patch.to_string());
                current_size -= patch.len() - compressed.len();
                compressed
            } else {
                patch.to_string()
            }
        })
        .collect::<Vec<_>>();

    if current_size <= max_size {
        return compressed_patches.join("\n\n");
    }

    // Compression level 2: list paths of the changed files only
    let filenames = file_patches
        .iter()
        .map(|patch| {
            let patch = diffy::Patch::from_str(patch).unwrap();
            let path = patch
                .modified()
                .and_then(|path| path.strip_prefix("b/"))
                .unwrap_or_default();
            format!("- {path}\n")
        })
        .collect::<Vec<_>>();

    filenames.join("")
}

/// Split a potentially multi-file patch into multiple single-file patches
fn split_patch(patch: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current_patch = String::new();

    for line in patch.lines() {
        if line.starts_with("---") && !current_patch.is_empty() {
            result.push(current_patch.trim_end_matches('\n').into());
            current_patch = String::new();
        }
        current_patch.push_str(line);
        current_patch.push('\n');
    }

    if !current_patch.is_empty() {
        result.push(current_patch.trim_end_matches('\n').into());
    }

    result
}

fn compress_patch(patch: &str) -> anyhow::Result<String> {
    let patch = diffy::Patch::from_str(patch)?;
    let mut out = String::new();

    writeln!(out, "--- {}", patch.original().unwrap_or("a"))?;
    writeln!(out, "+++ {}", patch.modified().unwrap_or("b"))?;

    for hunk in patch.hunks() {
        writeln!(out, "@@ -{} +{} @@", hunk.old_range(), hunk.new_range())?;
        writeln!(out, "[...skipped...]")?;
    }

    Ok(out)
}
