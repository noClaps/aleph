mod chunking;
mod embedding;
mod embedding_index;
mod indexing;
mod project_index;
mod project_index_debug_view;
mod summary_backlog;
mod summary_index;
mod worktree_index;

use anyhow::{Context as _, Result};
use collections::HashMap;
use fs::Fs;
use gpui::{App, AppContext as _, AsyncApp, BorrowAppContext, Context, Entity, Global, WeakEntity};
use language::LineEnding;
use project::{Project, Worktree};
use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
    sync::Arc,
};
use util::ResultExt as _;
use workspace::Workspace;

pub use embedding::*;
pub use project_index::{LoadedSearchResult, ProjectIndex, SearchResult, Status};
pub use project_index_debug_view::ProjectIndexDebugView;
pub use summary_index::FileSummary;

pub struct SemanticDb {
    embedding_provider: Arc<dyn EmbeddingProvider>,
    db_connection: Option<heed::Env>,
    project_indices: HashMap<WeakEntity<Project>, Entity<ProjectIndex>>,
}

impl Global for SemanticDb {}

impl SemanticDb {
    pub async fn new(
        db_path: PathBuf,
        embedding_provider: Arc<dyn EmbeddingProvider>,
        cx: &mut AsyncApp,
    ) -> Result<Self> {
        let db_connection = cx
            .background_spawn(async move {
                std::fs::create_dir_all(&db_path)?;
                unsafe {
                    heed::EnvOpenOptions::new()
                        .map_size(1024 * 1024 * 1024)
                        .max_dbs(3000)
                        .open(db_path)
                }
            })
            .await
            .context("opening database connection")?;

        cx.update(|cx| {
            cx.observe_new(
                |workspace: &mut Workspace, _window, cx: &mut Context<Workspace>| {
                    let project = workspace.project().clone();

                    if cx.has_global::<SemanticDb>() {
                        cx.update_global::<SemanticDb, _>(|this, cx| {
                            this.create_project_index(project, cx);
                        })
                    } else {
                        log::info!("No SemanticDb, skipping project index")
                    }
                },
            )
            .detach();
        })
        .ok();

        Ok(SemanticDb {
            db_connection: Some(db_connection),
            embedding_provider,
            project_indices: HashMap::default(),
        })
    }

    pub async fn load_results(
        mut results: Vec<SearchResult>,
        fs: &Arc<dyn Fs>,
        cx: &AsyncApp,
    ) -> Result<Vec<LoadedSearchResult>> {
        let mut max_scores_by_path = HashMap::<_, (f32, usize)>::default();
        for result in &results {
            let (score, query_index) = max_scores_by_path
                .entry((result.worktree.clone(), result.path.clone()))
                .or_default();
            if result.score > *score {
                *score = result.score;
                *query_index = result.query_index;
            }
        }

        results.sort_by(|a, b| {
            let max_score_a = max_scores_by_path[&(a.worktree.clone(), a.path.clone())].0;
            let max_score_b = max_scores_by_path[&(b.worktree.clone(), b.path.clone())].0;
            max_score_b
                .partial_cmp(&max_score_a)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.worktree.entity_id().cmp(&b.worktree.entity_id()))
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.range.start.cmp(&b.range.start))
        });

        let mut last_loaded_file: Option<(Entity<Worktree>, Arc<Path>, PathBuf, String)> = None;
        let mut loaded_results = Vec::<LoadedSearchResult>::new();
        for result in results {
            let full_path;
            let file_content;
            if let Some(last_loaded_file) =
                last_loaded_file
                    .as_ref()
                    .filter(|(last_worktree, last_path, _, _)| {
                        last_worktree == &result.worktree && last_path == &result.path
                    })
            {
                full_path = last_loaded_file.2.clone();
                file_content = &last_loaded_file.3;
            } else {
                let output = result.worktree.read_with(cx, |worktree, _cx| {
                    let entry_abs_path = worktree.abs_path().join(&result.path);
                    let mut entry_full_path = PathBuf::from(worktree.root_name());
                    entry_full_path.push(&result.path);
                    let file_content = async {
                        let entry_abs_path = entry_abs_path;
                        fs.load(&entry_abs_path).await
                    };
                    (entry_full_path, file_content)
                })?;
                full_path = output.0;
                let Some(content) = output.1.await.log_err() else {
                    continue;
                };
                last_loaded_file = Some((
                    result.worktree.clone(),
                    result.path.clone(),
                    full_path.clone(),
                    content,
                ));
                file_content = &last_loaded_file.as_ref().unwrap().3;
            };

            let query_index = max_scores_by_path[&(result.worktree.clone(), result.path.clone())].1;

            let mut range_start = result.range.start.min(file_content.len());
            let mut range_end = result.range.end.min(file_content.len());
            while !file_content.is_char_boundary(range_start) {
                range_start += 1;
            }
            while !file_content.is_char_boundary(range_end) {
                range_end += 1;
            }

            let start_row = file_content[0..range_start].matches('\n').count() as u32;
            let mut end_row = file_content[0..range_end].matches('\n').count() as u32;
            let start_line_byte_offset = file_content[0..range_start]
                .rfind('\n')
                .map(|pos| pos + 1)
                .unwrap_or_default();
            let mut end_line_byte_offset = range_end;
            if file_content[..end_line_byte_offset].ends_with('\n') {
                end_row -= 1;
            } else {
                end_line_byte_offset = file_content[range_end..]
                    .find('\n')
                    .map(|pos| range_end + pos + 1)
                    .unwrap_or_else(|| file_content.len());
            }
            let mut excerpt_content =
                file_content[start_line_byte_offset..end_line_byte_offset].to_string();
            LineEnding::normalize(&mut excerpt_content);

            if let Some(prev_result) = loaded_results.last_mut()
                && prev_result.full_path == full_path
                && *prev_result.row_range.end() + 1 == start_row
            {
                prev_result.row_range = *prev_result.row_range.start()..=end_row;
                prev_result.excerpt_content.push_str(&excerpt_content);
                continue;
            }

            loaded_results.push(LoadedSearchResult {
                path: result.path,
                full_path,
                excerpt_content,
                row_range: start_row..=end_row,
                query_index,
            });
        }

        for result in &mut loaded_results {
            while result.excerpt_content.ends_with("\n\n") {
                result.excerpt_content.pop();
                result.row_range =
                    *result.row_range.start()..=result.row_range.end().saturating_sub(1)
            }
        }

        Ok(loaded_results)
    }

    pub fn project_index(
        &mut self,
        project: Entity<Project>,
        _cx: &mut App,
    ) -> Option<Entity<ProjectIndex>> {
        self.project_indices.get(&project.downgrade()).cloned()
    }

    pub fn remaining_summaries(
        &self,
        project: &WeakEntity<Project>,
        cx: &mut App,
    ) -> Option<usize> {
        self.project_indices.get(project).map(|project_index| {
            project_index.update(cx, |project_index, cx| {
                project_index.remaining_summaries(cx)
            })
        })
    }

    pub fn create_project_index(
        &mut self,
        project: Entity<Project>,
        cx: &mut App,
    ) -> Entity<ProjectIndex> {
        let project_index = cx.new(|cx| {
            ProjectIndex::new(
                project.clone(),
                self.db_connection.clone().unwrap(),
                self.embedding_provider.clone(),
                cx,
            )
        });

        let project_weak = project.downgrade();
        self.project_indices
            .insert(project_weak.clone(), project_index.clone());

        cx.observe_release(&project, move |_, cx| {
            if cx.has_global::<SemanticDb>() {
                cx.update_global::<SemanticDb, _>(|this, _| {
                    this.project_indices.remove(&project_weak);
                })
            }
        })
        .detach();

        project_index
    }
}

impl Drop for SemanticDb {
    fn drop(&mut self) {
        self.db_connection.take().unwrap().prepare_for_closing();
    }
}
