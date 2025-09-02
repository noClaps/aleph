use super::{HoverTarget, HoveredWord, TerminalView};
use anyhow::{Context as _, Result};
use editor::Editor;
use gpui::{App, AppContext, Context, Task, WeakEntity, Window};
use itertools::Itertools;
use project::{Entry, Metadata};
use std::path::PathBuf;
use terminal::PathLikeTarget;
use util::{ResultExt, debug_panic, paths::PathWithPosition};
use workspace::{OpenOptions, OpenVisible, Workspace};

#[derive(Debug, Clone)]
enum OpenTarget {
    Worktree(PathWithPosition, Entry),
    File(PathWithPosition, Metadata),
}

impl OpenTarget {
    fn is_file(&self) -> bool {
        match self {
            OpenTarget::Worktree(_, entry) => entry.is_file(),
            OpenTarget::File(_, metadata) => !metadata.is_dir,
        }
    }

    fn is_dir(&self) -> bool {
        match self {
            OpenTarget::Worktree(_, entry) => entry.is_dir(),
            OpenTarget::File(_, metadata) => metadata.is_dir,
        }
    }

    fn path(&self) -> &PathWithPosition {
        match self {
            OpenTarget::Worktree(path, _) => path,
            OpenTarget::File(path, _) => path,
        }
    }
}

pub(super) fn hover_path_like_target(
    workspace: &WeakEntity<Workspace>,
    hovered_word: HoveredWord,
    path_like_target: &PathLikeTarget,
    cx: &mut Context<TerminalView>,
) -> Task<()> {
    let file_to_open_task = possible_open_target(workspace, path_like_target, cx);
    cx.spawn(async move |terminal_view, cx| {
        let file_to_open = file_to_open_task.await;
        terminal_view
            .update(cx, |terminal_view, _| match file_to_open {
                Some(OpenTarget::File(path, _) | OpenTarget::Worktree(path, _)) => {
                    terminal_view.hover = Some(HoverTarget {
                        tooltip: path.to_string(|path| path.to_string_lossy().to_string()),
                        hovered_word,
                    });
                }
                None => {
                    terminal_view.hover = None;
                }
            })
            .ok();
    })
}

fn possible_open_target(
    workspace: &WeakEntity<Workspace>,
    path_like_target: &PathLikeTarget,
    cx: &App,
) -> Task<Option<OpenTarget>> {
    let Some(workspace) = workspace.upgrade() else {
        return Task::ready(None);
    };
    // We have to check for both paths, as on Unix, certain paths with positions are valid file paths too.
    // We can be on FS remote part, without real FS, so cannot canonicalize or check for existence the path right away.
    let mut potential_paths = Vec::new();
    let cwd = path_like_target.terminal_dir.as_ref();
    let maybe_path = &path_like_target.maybe_path;
    let original_path = PathWithPosition::from_path(PathBuf::from(maybe_path));
    let path_with_position = PathWithPosition::parse_str(maybe_path);
    let worktree_candidates = workspace
        .read(cx)
        .worktrees(cx)
        .sorted_by_key(|worktree| {
            let worktree_root = worktree.read(cx).abs_path();
            match cwd.and_then(|cwd| worktree_root.strip_prefix(cwd).ok()) {
                Some(cwd_child) => cwd_child.components().count(),
                None => usize::MAX,
            }
        })
        .collect::<Vec<_>>();
    // Since we do not check paths via FS and joining, we need to strip off potential `./`, `a/`, `b/` prefixes out of it.
    const GIT_DIFF_PATH_PREFIXES: &[&str] = &["a", "b"];
    for prefix_str in GIT_DIFF_PATH_PREFIXES.iter().chain(std::iter::once(&".")) {
        if let Some(stripped) = original_path.path.strip_prefix(prefix_str).ok() {
            potential_paths.push(PathWithPosition {
                path: stripped.to_owned(),
                row: original_path.row,
                column: original_path.column,
            });
        }
        if let Some(stripped) = path_with_position.path.strip_prefix(prefix_str).ok() {
            potential_paths.push(PathWithPosition {
                path: stripped.to_owned(),
                row: path_with_position.row,
                column: path_with_position.column,
            });
        }
    }

    let insert_both_paths = original_path != path_with_position;
    potential_paths.insert(0, original_path);
    if insert_both_paths {
        potential_paths.insert(1, path_with_position);
    }

    // If we won't find paths "easily", we can traverse the entire worktree to look what ends with the potential path suffix.
    // That will be slow, though, so do the fast checks first.
    let mut worktree_paths_to_check = Vec::new();
    for worktree in &worktree_candidates {
        let worktree_root = worktree.read(cx).abs_path();
        let mut paths_to_check = Vec::with_capacity(potential_paths.len());

        for path_with_position in &potential_paths {
            let path_to_check = if worktree_root.ends_with(&path_with_position.path) {
                let root_path_with_position = PathWithPosition {
                    path: worktree_root.to_path_buf(),
                    row: path_with_position.row,
                    column: path_with_position.column,
                };
                match worktree.read(cx).root_entry() {
                    Some(root_entry) => {
                        return Task::ready(Some(OpenTarget::Worktree(
                            root_path_with_position,
                            root_entry.clone(),
                        )));
                    }
                    None => root_path_with_position,
                }
            } else {
                PathWithPosition {
                    path: path_with_position
                        .path
                        .strip_prefix(&worktree_root)
                        .unwrap_or(&path_with_position.path)
                        .to_owned(),
                    row: path_with_position.row,
                    column: path_with_position.column,
                }
            };

            if path_to_check.path.is_relative()
                && let Some(entry) = worktree.read(cx).entry_for_path(&path_to_check.path)
            {
                return Task::ready(Some(OpenTarget::Worktree(
                    PathWithPosition {
                        path: worktree_root.join(&entry.path),
                        row: path_to_check.row,
                        column: path_to_check.column,
                    },
                    entry.clone(),
                )));
            }

            paths_to_check.push(path_to_check);
        }

        if !paths_to_check.is_empty() {
            worktree_paths_to_check.push((worktree.clone(), paths_to_check));
        }
    }

    // Before entire worktree traversal(s), make an attempt to do FS checks if available.
    let fs_paths_to_check = if workspace.read(cx).project().read(cx).is_local() {
        potential_paths
            .into_iter()
            .flat_map(|path_to_check| {
                let mut paths_to_check = Vec::new();
                let maybe_path = &path_to_check.path;
                if maybe_path.starts_with("~") {
                    if let Some(home_path) =
                        maybe_path
                            .strip_prefix("~")
                            .ok()
                            .and_then(|stripped_maybe_path| {
                                Some(dirs::home_dir()?.join(stripped_maybe_path))
                            })
                    {
                        paths_to_check.push(PathWithPosition {
                            path: home_path,
                            row: path_to_check.row,
                            column: path_to_check.column,
                        });
                    }
                } else {
                    paths_to_check.push(PathWithPosition {
                        path: maybe_path.clone(),
                        row: path_to_check.row,
                        column: path_to_check.column,
                    });
                    if maybe_path.is_relative() {
                        if let Some(cwd) = &cwd {
                            paths_to_check.push(PathWithPosition {
                                path: cwd.join(maybe_path),
                                row: path_to_check.row,
                                column: path_to_check.column,
                            });
                        }
                        for worktree in &worktree_candidates {
                            paths_to_check.push(PathWithPosition {
                                path: worktree.read(cx).abs_path().join(maybe_path),
                                row: path_to_check.row,
                                column: path_to_check.column,
                            });
                        }
                    }
                }
                paths_to_check
            })
            .collect()
    } else {
        Vec::new()
    };

    let worktree_check_task = cx.spawn(async move |cx| {
        for (worktree, worktree_paths_to_check) in worktree_paths_to_check {
            let found_entry = worktree
                .update(cx, |worktree, _| {
                    let worktree_root = worktree.abs_path();
                    let traversal = worktree.traverse_from_path(true, true, false, "".as_ref());
                    for entry in traversal {
                        if let Some(path_in_worktree) = worktree_paths_to_check
                            .iter()
                            .find(|path_to_check| entry.path.ends_with(&path_to_check.path))
                        {
                            return Some(OpenTarget::Worktree(
                                PathWithPosition {
                                    path: worktree_root.join(&entry.path),
                                    row: path_in_worktree.row,
                                    column: path_in_worktree.column,
                                },
                                entry.clone(),
                            ));
                        }
                    }
                    None
                })
                .ok()?;
            if let Some(found_entry) = found_entry {
                return Some(found_entry);
            }
        }
        None
    });

    let fs = workspace.read(cx).project().read(cx).fs().clone();
    cx.background_spawn(async move {
        for mut path_to_check in fs_paths_to_check {
            if let Some(fs_path_to_check) = fs.canonicalize(&path_to_check.path).await.ok()
                && let Some(metadata) = fs.metadata(&fs_path_to_check).await.ok().flatten()
            {
                path_to_check.path = fs_path_to_check;
                return Some(OpenTarget::File(path_to_check, metadata));
            }
        }

        worktree_check_task.await
    })
}

pub(super) fn open_path_like_target(
    workspace: &WeakEntity<Workspace>,
    terminal_view: &mut TerminalView,
    path_like_target: &PathLikeTarget,
    window: &mut Window,
    cx: &mut Context<TerminalView>,
) {
    possibly_open_target(workspace, terminal_view, path_like_target, window, cx)
        .detach_and_log_err(cx)
}

fn possibly_open_target(
    workspace: &WeakEntity<Workspace>,
    terminal_view: &mut TerminalView,
    path_like_target: &PathLikeTarget,
    window: &mut Window,
    cx: &mut Context<TerminalView>,
) -> Task<Result<Option<OpenTarget>>> {
    if terminal_view.hover.is_none() {
        return Task::ready(Ok(None));
    }
    let workspace = workspace.clone();
    let path_like_target = path_like_target.clone();
    cx.spawn_in(window, async move |terminal_view, cx| {
        let Some(open_target) = terminal_view
            .update(cx, |_, cx| {
                possible_open_target(&workspace, &path_like_target, cx)
            })?
            .await
        else {
            return Ok(None);
        };

        let path_to_open = open_target.path();
        let opened_items = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_paths(
                    vec![path_to_open.path.clone()],
                    OpenOptions {
                        visible: Some(OpenVisible::OnlyDirectories),
                        ..Default::default()
                    },
                    None,
                    window,
                    cx,
                )
            })
            .context("workspace update")?
            .await;
        if opened_items.len() != 1 {
            debug_panic!(
                "Received {} items for one path {path_to_open:?}",
                opened_items.len(),
            );
        }

        if let Some(opened_item) = opened_items.first() {
            if open_target.is_file() {
                if let Some(Ok(opened_item)) = opened_item {
                    if let Some(row) = path_to_open.row {
                        let col = path_to_open.column.unwrap_or(0);
                        if let Some(active_editor) = opened_item.downcast::<Editor>() {
                            active_editor
                                .downgrade()
                                .update_in(cx, |editor, window, cx| {
                                    editor.go_to_singleton_buffer_point(
                                        language::Point::new(
                                            row.saturating_sub(1),
                                            col.saturating_sub(1),
                                        ),
                                        window,
                                        cx,
                                    )
                                })
                                .log_err();
                        }
                    }
                    return Ok(Some(open_target));
                }
            } else if open_target.is_dir() {
                workspace.update(cx, |workspace, cx| {
                    workspace.project().update(cx, |_, cx| {
                        cx.emit(project::Event::ActivateProjectPanel);
                    })
                })?;
                return Ok(Some(open_target));
            }
        }
        Ok(None)
    })
}
