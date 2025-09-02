use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Result, anyhow};
use assistant_tool::{Tool, ToolResult};
use futures::StreamExt;
use gpui::{AnyWindowHandle, App, Entity, Task};
use language::{OffsetRangeExt, ParseStatus, Point};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use project::{
    Project, WorktreeSettings,
    search::{SearchQuery, SearchResult},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings;
use std::{cmp, fmt::Write, sync::Arc};
use ui::IconName;
use util::RangeExt;
use util::markdown::MarkdownInlineCode;
use util::paths::PathMatcher;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GrepToolInput {
    /// A regex pattern to search for in the entire project. Note that the regex
    /// will be parsed by the Rust `regex` crate.
    ///
    /// Do NOT specify a path here! This will only be matched against the code **content**.
    pub regex: String,

    /// A glob pattern for the paths of files to include in the search.
    /// Supports standard glob patterns like "**/*.rs" or "src/**/*.ts".
    /// If omitted, all files in the project will be searched.
    pub include_pattern: Option<String>,

    /// Optional starting position for paginated results (0-based).
    /// When not provided, starts from the beginning.
    #[serde(default)]
    pub offset: u32,

    /// Whether the regex is case-sensitive. Defaults to false (case-insensitive).
    #[serde(default)]
    pub case_sensitive: bool,
}

impl GrepToolInput {
    /// Which page of search results this is.
    pub fn page(&self) -> u32 {
        1 + (self.offset / RESULTS_PER_PAGE)
    }
}

const RESULTS_PER_PAGE: u32 = 20;

pub struct GrepTool;

impl Tool for GrepTool {
    fn name(&self) -> String {
        "grep".into()
    }

    fn needs_confirmation(&self, _: &serde_json::Value, _: &Entity<Project>, _: &App) -> bool {
        false
    }

    fn may_perform_edits(&self) -> bool {
        false
    }

    fn description(&self) -> String {
        include_str!("./grep_tool/description.md").into()
    }

    fn icon(&self) -> IconName {
        IconName::ToolRegex
    }

    fn input_schema(&self, format: LanguageModelToolSchemaFormat) -> Result<serde_json::Value> {
        json_schema_for::<GrepToolInput>(format)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        match serde_json::from_value::<GrepToolInput>(input.clone()) {
            Ok(input) => {
                let page = input.page();
                let regex_str = MarkdownInlineCode(&input.regex);
                let case_info = if input.case_sensitive {
                    " (case-sensitive)"
                } else {
                    ""
                };

                if page > 1 {
                    format!("Get page {page} of search results for regex {regex_str}{case_info}")
                } else {
                    format!("Search files for regex {regex_str}{case_info}")
                }
            }
            Err(_) => "Search with regex".to_string(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        _request: Arc<LanguageModelRequest>,
        project: Entity<Project>,
        _action_log: Entity<ActionLog>,
        _model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        const CONTEXT_LINES: u32 = 2;
        const MAX_ANCESTOR_LINES: u32 = 10;

        let input = match serde_json::from_value::<GrepToolInput>(input) {
            Ok(input) => input,
            Err(error) => {
                return Task::ready(Err(anyhow!("Failed to parse input: {error}"))).into();
            }
        };

        let include_matcher = match PathMatcher::new(
            input
                .include_pattern
                .as_ref()
                .into_iter()
                .collect::<Vec<_>>(),
        ) {
            Ok(matcher) => matcher,
            Err(error) => {
                return Task::ready(Err(anyhow!("invalid include glob pattern: {error}"))).into();
            }
        };

        // Exclude global file_scan_exclusions and private_files settings
        let exclude_matcher = {
            let global_settings = WorktreeSettings::get_global(cx);
            let exclude_patterns = global_settings
                .file_scan_exclusions
                .sources()
                .iter()
                .chain(global_settings.private_files.sources().iter());

            match PathMatcher::new(exclude_patterns) {
                Ok(matcher) => matcher,
                Err(error) => {
                    return Task::ready(Err(anyhow!("invalid exclude pattern: {error}"))).into();
                }
            }
        };

        let query = match SearchQuery::regex(
            &input.regex,
            false,
            input.case_sensitive,
            false,
            false,
            include_matcher,
            exclude_matcher,
            true, // Always match file include pattern against *full project paths* that start with a project root.
            None,
        ) {
            Ok(query) => query,
            Err(error) => return Task::ready(Err(error)).into(),
        };

        let results = project.update(cx, |project, cx| project.search(query, cx));

        cx.spawn(async move |cx|  {
            futures::pin_mut!(results);

            let mut output = String::new();
            let mut skips_remaining = input.offset;
            let mut matches_found = 0;
            let mut has_more_matches = false;

            'outer: while let Some(SearchResult::Buffer { buffer, ranges }) = results.next().await {
                if ranges.is_empty() {
                    continue;
                }

                let Ok((Some(path), mut parse_status)) = buffer.read_with(cx, |buffer, cx| {
                    (buffer.file().map(|file| file.full_path(cx)), buffer.parse_status())
                }) else {
                    continue;
                };

                // Check if this file should be excluded based on its worktree settings
                if let Ok(Some(project_path)) = project.read_with(cx, |project, cx| {
                    project.find_project_path(&path, cx)
                })
                    && cx.update(|cx| {
                        let worktree_settings = WorktreeSettings::get(Some((&project_path).into()), cx);
                        worktree_settings.is_path_excluded(&project_path.path)
                            || worktree_settings.is_path_private(&project_path.path)
                    }).unwrap_or(false) {
                        continue;
                    }

                while *parse_status.borrow() != ParseStatus::Idle {
                    parse_status.changed().await?;
                }

                let snapshot = buffer.read_with(cx, |buffer, _cx| buffer.snapshot())?;

                let mut ranges = ranges
                    .into_iter()
                    .map(|range| {
                        let matched = range.to_point(&snapshot);
                        let matched_end_line_len = snapshot.line_len(matched.end.row);
                        let full_lines = Point::new(matched.start.row, 0)..Point::new(matched.end.row, matched_end_line_len);
                        let symbols = snapshot.symbols_containing(matched.start, None);

                        if let Some(ancestor_node) = snapshot.syntax_ancestor(full_lines.clone()) {
                            let full_ancestor_range = ancestor_node.byte_range().to_point(&snapshot);
                            let end_row = full_ancestor_range.end.row.min(full_ancestor_range.start.row + MAX_ANCESTOR_LINES);
                            let end_col = snapshot.line_len(end_row);
                            let capped_ancestor_range = Point::new(full_ancestor_range.start.row, 0)..Point::new(end_row, end_col);

                            if capped_ancestor_range.contains_inclusive(&full_lines) {
                                return (capped_ancestor_range, Some(full_ancestor_range), symbols)
                            }
                        }

                        let mut matched = matched;
                        matched.start.column = 0;
                        matched.start.row =
                            matched.start.row.saturating_sub(CONTEXT_LINES);
                        matched.end.row = cmp::min(
                            snapshot.max_point().row,
                            matched.end.row + CONTEXT_LINES,
                        );
                        matched.end.column = snapshot.line_len(matched.end.row);

                        (matched, None, symbols)
                    })
                    .peekable();

                let mut file_header_written = false;

                while let Some((mut range, ancestor_range, parent_symbols)) = ranges.next(){
                    if skips_remaining > 0 {
                        skips_remaining -= 1;
                        continue;
                    }

                    // We'd already found a full page of matches, and we just found one more.
                    if matches_found >= RESULTS_PER_PAGE {
                        has_more_matches = true;
                        break 'outer;
                    }

                    while let Some((next_range, _, _)) = ranges.peek() {
                        if range.end.row >= next_range.start.row {
                            range.end = next_range.end;
                            ranges.next();
                        } else {
                            break;
                        }
                    }

                    if !file_header_written {
                        writeln!(output, "\n## Matches in {}", path.display())?;
                        file_header_written = true;
                    }

                    let end_row = range.end.row;
                    output.push_str("\n### ");

                    if let Some(parent_symbols) = &parent_symbols {
                        for symbol in parent_symbols {
                            write!(output, "{} › ", symbol.text)?;
                        }
                    }

                    if range.start.row == end_row {
                        writeln!(output, "L{}", range.start.row + 1)?;
                    } else {
                        writeln!(output, "L{}-{}", range.start.row + 1, end_row + 1)?;
                    }

                    output.push_str("```\n");
                    output.extend(snapshot.text_for_range(range));
                    output.push_str("\n```\n");

                    if let Some(ancestor_range) = ancestor_range
                        && end_row < ancestor_range.end.row {
                            let remaining_lines = ancestor_range.end.row - end_row;
                            writeln!(output, "\n{} lines remaining in ancestor node. Read the file to see all.", remaining_lines)?;
                        }

                    matches_found += 1;
                }
            }

            if matches_found == 0 {
                Ok("No matches found".to_string().into())
            } else if has_more_matches {
                Ok(format!(
                    "Showing matches {}-{} (there were more matches found; use offset: {} to see next page):\n{output}",
                    input.offset + 1,
                    input.offset + matches_found,
                    input.offset + RESULTS_PER_PAGE,
                ).into())
            } else {
                Ok(format!("Found {matches_found} matches:\n{output}").into())
            }
        }).into()
    }
}
