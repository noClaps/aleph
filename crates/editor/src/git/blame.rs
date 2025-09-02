use crate::Editor;
use anyhow::Result;
use collections::HashMap;
use git::{
    GitHostingProviderRegistry, GitRemote, Oid,
    blame::{Blame, BlameEntry, ParsedCommitMessage},
    parse_git_remote_url,
};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Hsla, ScrollHandle, Subscription, Task,
    TextStyle, WeakEntity, Window,
};
use itertools::Itertools;
use language::{Bias, BufferSnapshot, Edit};
use markdown::Markdown;
use multi_buffer::{MultiBuffer, RowInfo};
use project::{
    Project, ProjectItem as _,
    git_store::{GitStoreEvent, Repository, RepositoryEvent},
};
use smallvec::SmallVec;
use std::{sync::Arc, time::Duration};
use sum_tree::SumTree;
use text::BufferId;
use workspace::Workspace;

#[derive(Clone, Debug, Default)]
pub struct GitBlameEntry {
    pub rows: u32,
    pub blame: Option<BlameEntry>,
}

#[derive(Clone, Debug, Default)]
pub struct GitBlameEntrySummary {
    rows: u32,
}

impl sum_tree::Item for GitBlameEntry {
    type Summary = GitBlameEntrySummary;

    fn summary(&self, _cx: &()) -> Self::Summary {
        GitBlameEntrySummary { rows: self.rows }
    }
}

impl sum_tree::Summary for GitBlameEntrySummary {
    type Context = ();

    fn zero(_cx: &()) -> Self {
        Default::default()
    }

    fn add_summary(&mut self, summary: &Self, _cx: &()) {
        self.rows += summary.rows;
    }
}

impl<'a> sum_tree::Dimension<'a, GitBlameEntrySummary> for u32 {
    fn zero(_cx: &()) -> Self {
        Default::default()
    }

    fn add_summary(&mut self, summary: &'a GitBlameEntrySummary, _cx: &()) {
        *self += summary.rows;
    }
}

struct GitBlameBuffer {
    entries: SumTree<GitBlameEntry>,
    buffer_snapshot: BufferSnapshot,
    buffer_edits: text::Subscription,
    commit_details: HashMap<Oid, ParsedCommitMessage>,
}

pub struct GitBlame {
    project: Entity<Project>,
    multi_buffer: WeakEntity<MultiBuffer>,
    buffers: HashMap<BufferId, GitBlameBuffer>,
    task: Task<Result<()>>,
    focused: bool,
    changed_while_blurred: bool,
    user_triggered: bool,
    regenerate_on_edit_task: Task<Result<()>>,
    _regenerate_subscriptions: Vec<Subscription>,
}

pub trait BlameRenderer {
    fn max_author_length(&self) -> usize;

    fn render_blame_entry(
        &self,
        _: &TextStyle,
        _: BlameEntry,
        _: Option<ParsedCommitMessage>,
        _: Entity<Repository>,
        _: WeakEntity<Workspace>,
        _: Entity<Editor>,
        _: usize,
        _: Hsla,
        _: &mut App,
    ) -> Option<AnyElement>;

    fn render_inline_blame_entry(
        &self,
        _: &TextStyle,
        _: BlameEntry,
        _: &mut App,
    ) -> Option<AnyElement>;

    fn render_blame_entry_popover(
        &self,
        _: BlameEntry,
        _: ScrollHandle,
        _: Option<ParsedCommitMessage>,
        _: Entity<Markdown>,
        _: Entity<Repository>,
        _: WeakEntity<Workspace>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<AnyElement>;

    fn open_blame_commit(
        &self,
        _: BlameEntry,
        _: Entity<Repository>,
        _: WeakEntity<Workspace>,
        _: &mut Window,
        _: &mut App,
    );
}

impl BlameRenderer for () {
    fn max_author_length(&self) -> usize {
        0
    }

    fn render_blame_entry(
        &self,
        _: &TextStyle,
        _: BlameEntry,
        _: Option<ParsedCommitMessage>,
        _: Entity<Repository>,
        _: WeakEntity<Workspace>,
        _: Entity<Editor>,
        _: usize,
        _: Hsla,
        _: &mut App,
    ) -> Option<AnyElement> {
        None
    }

    fn render_inline_blame_entry(
        &self,
        _: &TextStyle,
        _: BlameEntry,
        _: &mut App,
    ) -> Option<AnyElement> {
        None
    }

    fn render_blame_entry_popover(
        &self,
        _: BlameEntry,
        _: ScrollHandle,
        _: Option<ParsedCommitMessage>,
        _: Entity<Markdown>,
        _: Entity<Repository>,
        _: WeakEntity<Workspace>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<AnyElement> {
        None
    }

    fn open_blame_commit(
        &self,
        _: BlameEntry,
        _: Entity<Repository>,
        _: WeakEntity<Workspace>,
        _: &mut Window,
        _: &mut App,
    ) {
    }
}

pub(crate) struct GlobalBlameRenderer(pub Arc<dyn BlameRenderer>);

impl gpui::Global for GlobalBlameRenderer {}

impl GitBlame {
    pub fn new(
        multi_buffer: Entity<MultiBuffer>,
        project: Entity<Project>,
        user_triggered: bool,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let multi_buffer_subscription = cx.subscribe(
            &multi_buffer,
            |git_blame, multi_buffer, event, cx| match event {
                multi_buffer::Event::DirtyChanged => {
                    if !multi_buffer.read(cx).is_dirty(cx) {
                        git_blame.generate(cx);
                    }
                }
                multi_buffer::Event::ExcerptsAdded { .. }
                | multi_buffer::Event::ExcerptsEdited { .. } => git_blame.regenerate_on_edit(cx),
                _ => {}
            },
        );

        let project_subscription = cx.subscribe(&project, {
            let multi_buffer = multi_buffer.downgrade();

            move |git_blame, _, event, cx| {
                if let project::Event::WorktreeUpdatedEntries(_, updated) = event {
                    let Some(multi_buffer) = multi_buffer.upgrade() else {
                        return;
                    };
                    let project_entry_id = multi_buffer
                        .read(cx)
                        .as_singleton()
                        .and_then(|it| it.read(cx).entry_id(cx));
                    if updated
                        .iter()
                        .any(|(_, entry_id, _)| project_entry_id == Some(*entry_id))
                    {
                        log::debug!("Updated buffers. Regenerating blame data...",);
                        git_blame.generate(cx);
                    }
                }
            }
        });

        let git_store = project.read(cx).git_store().clone();
        let git_store_subscription =
            cx.subscribe(&git_store, move |this, _, event, cx| match event {
                GitStoreEvent::RepositoryUpdated(_, RepositoryEvent::Updated { .. }, _)
                | GitStoreEvent::RepositoryAdded(_)
                | GitStoreEvent::RepositoryRemoved(_) => {
                    log::debug!("Status of git repositories updated. Regenerating blame data...",);
                    this.generate(cx);
                }
                _ => {}
            });

        let mut this = Self {
            project,
            multi_buffer: multi_buffer.downgrade(),
            buffers: HashMap::default(),
            user_triggered,
            focused,
            changed_while_blurred: false,
            task: Task::ready(Ok(())),
            regenerate_on_edit_task: Task::ready(Ok(())),
            _regenerate_subscriptions: vec![
                multi_buffer_subscription,
                project_subscription,
                git_store_subscription,
            ],
        };
        this.generate(cx);
        this
    }

    pub fn repository(&self, cx: &App, id: BufferId) -> Option<Entity<Repository>> {
        self.project
            .read(cx)
            .git_store()
            .read(cx)
            .repository_and_path_for_buffer_id(id, cx)
            .map(|(repo, _)| repo)
    }

    pub fn has_generated_entries(&self) -> bool {
        !self.buffers.is_empty()
    }

    pub fn details_for_entry(
        &self,
        buffer: BufferId,
        entry: &BlameEntry,
    ) -> Option<ParsedCommitMessage> {
        self.buffers
            .get(&buffer)?
            .commit_details
            .get(&entry.sha)
            .cloned()
    }

    pub fn blame_for_rows<'a>(
        &'a mut self,
        rows: &'a [RowInfo],
        cx: &'a mut App,
    ) -> impl Iterator<Item = Option<(BufferId, BlameEntry)>> + use<'a> {
        rows.iter().map(move |info| {
            let buffer_id = info.buffer_id?;
            self.sync(cx, buffer_id);

            let buffer_row = info.buffer_row?;
            let mut cursor = self.buffers.get(&buffer_id)?.entries.cursor::<u32>(&());
            cursor.seek_forward(&buffer_row, Bias::Right);
            Some((buffer_id, cursor.item()?.blame.clone()?))
        })
    }

    pub fn max_author_length(&mut self, cx: &mut App) -> usize {
        let mut max_author_length = 0;
        self.sync_all(cx);

        for buffer in self.buffers.values() {
            for entry in buffer.entries.iter() {
                let author_len = entry
                    .blame
                    .as_ref()
                    .and_then(|entry| entry.author.as_ref())
                    .map(|author| author.len());
                if let Some(author_len) = author_len
                    && author_len > max_author_length
                {
                    max_author_length = author_len;
                }
            }
        }

        max_author_length
    }

    pub fn blur(&mut self, _: &mut Context<Self>) {
        self.focused = false;
    }

    pub fn focus(&mut self, cx: &mut Context<Self>) {
        if self.focused {
            return;
        }
        self.focused = true;
        if self.changed_while_blurred {
            self.changed_while_blurred = false;
            self.generate(cx);
        }
    }

    fn sync_all(&mut self, cx: &mut App) {
        let Some(multi_buffer) = self.multi_buffer.upgrade() else {
            return;
        };
        multi_buffer
            .read(cx)
            .excerpt_buffer_ids()
            .into_iter()
            .for_each(|id| self.sync(cx, id));
    }

    fn sync(&mut self, cx: &mut App, buffer_id: BufferId) {
        let Some(blame_buffer) = self.buffers.get_mut(&buffer_id) else {
            return;
        };
        let Some(buffer) = self
            .multi_buffer
            .upgrade()
            .and_then(|multi_buffer| multi_buffer.read(cx).buffer(buffer_id))
        else {
            return;
        };
        let edits = blame_buffer.buffer_edits.consume();
        let new_snapshot = buffer.read(cx).snapshot();

        let mut row_edits = edits
            .into_iter()
            .map(|edit| {
                let old_point_range = blame_buffer.buffer_snapshot.offset_to_point(edit.old.start)
                    ..blame_buffer.buffer_snapshot.offset_to_point(edit.old.end);
                let new_point_range = new_snapshot.offset_to_point(edit.new.start)
                    ..new_snapshot.offset_to_point(edit.new.end);

                if old_point_range.start.column
                    == blame_buffer
                        .buffer_snapshot
                        .line_len(old_point_range.start.row)
                    && (new_snapshot.chars_at(edit.new.start).next() == Some('\n')
                        || blame_buffer
                            .buffer_snapshot
                            .line_len(old_point_range.end.row)
                            == 0)
                {
                    Edit {
                        old: old_point_range.start.row + 1..old_point_range.end.row + 1,
                        new: new_point_range.start.row + 1..new_point_range.end.row + 1,
                    }
                } else if old_point_range.start.column == 0
                    && old_point_range.end.column == 0
                    && new_point_range.end.column == 0
                {
                    Edit {
                        old: old_point_range.start.row..old_point_range.end.row,
                        new: new_point_range.start.row..new_point_range.end.row,
                    }
                } else {
                    Edit {
                        old: old_point_range.start.row..old_point_range.end.row + 1,
                        new: new_point_range.start.row..new_point_range.end.row + 1,
                    }
                }
            })
            .peekable();

        let mut new_entries = SumTree::default();
        let mut cursor = blame_buffer.entries.cursor::<u32>(&());

        while let Some(mut edit) = row_edits.next() {
            while let Some(next_edit) = row_edits.peek() {
                if edit.old.end >= next_edit.old.start {
                    edit.old.end = next_edit.old.end;
                    edit.new.end = next_edit.new.end;
                    row_edits.next();
                } else {
                    break;
                }
            }

            new_entries.append(cursor.slice(&edit.old.start, Bias::Right), &());

            if edit.new.start > new_entries.summary().rows {
                new_entries.push(
                    GitBlameEntry {
                        rows: edit.new.start - new_entries.summary().rows,
                        blame: cursor.item().and_then(|entry| entry.blame.clone()),
                    },
                    &(),
                );
            }

            cursor.seek(&edit.old.end, Bias::Right);
            if !edit.new.is_empty() {
                new_entries.push(
                    GitBlameEntry {
                        rows: edit.new.len() as u32,
                        blame: None,
                    },
                    &(),
                );
            }

            let old_end = cursor.end();
            if row_edits
                .peek()
                .is_none_or(|next_edit| next_edit.old.start >= old_end)
                && let Some(entry) = cursor.item()
            {
                if old_end > edit.old.end {
                    new_entries.push(
                        GitBlameEntry {
                            rows: cursor.end() - edit.old.end,
                            blame: entry.blame.clone(),
                        },
                        &(),
                    );
                }

                cursor.next();
            }
        }
        new_entries.append(cursor.suffix(), &());
        drop(cursor);

        blame_buffer.buffer_snapshot = new_snapshot;
        blame_buffer.entries = new_entries;
    }

    fn generate(&mut self, cx: &mut Context<Self>) {
        if !self.focused {
            self.changed_while_blurred = true;
            return;
        }
        let blame = self.project.update(cx, |project, cx| {
            let Some(multi_buffer) = self.multi_buffer.upgrade() else {
                return Vec::new();
            };
            multi_buffer
                .read(cx)
                .all_buffer_ids()
                .into_iter()
                .filter_map(|id| {
                    let buffer = multi_buffer.read(cx).buffer(id)?;
                    let snapshot = buffer.read(cx).snapshot();
                    let buffer_edits = buffer.update(cx, |buffer, _| buffer.subscribe());

                    let blame_buffer = project.blame_buffer(&buffer, None, cx);
                    Some((id, snapshot, buffer_edits, blame_buffer))
                })
                .collect::<Vec<_>>()
        });
        let provider_registry = GitHostingProviderRegistry::default_global(cx);

        self.task = cx.spawn(async move |this, cx| {
            let (result, errors) = cx
                .background_spawn({
                    async move {
                        let mut res = vec![];
                        let mut errors = vec![];
                        for (id, snapshot, buffer_edits, blame) in blame {
                            match blame.await {
                                Ok(Some(Blame {
                                    entries,
                                    messages,
                                    remote_url,
                                })) => {
                                    let entries = build_blame_entry_sum_tree(
                                        entries,
                                        snapshot.max_point().row,
                                    );
                                    let commit_details = parse_commit_messages(
                                        messages,
                                        remote_url,
                                        provider_registry.clone(),
                                    )
                                    .await;

                                    res.push((
                                        id,
                                        snapshot,
                                        buffer_edits,
                                        Some(entries),
                                        commit_details,
                                    ));
                                }
                                Ok(None) => {
                                    res.push((id, snapshot, buffer_edits, None, Default::default()))
                                }
                                Err(e) => errors.push(e),
                            }
                        }
                        (res, errors)
                    }
                })
                .await;

            this.update(cx, |this, cx| {
                this.buffers.clear();
                for (id, snapshot, buffer_edits, entries, commit_details) in result {
                    let Some(entries) = entries else {
                        continue;
                    };
                    this.buffers.insert(
                        id,
                        GitBlameBuffer {
                            buffer_edits,
                            buffer_snapshot: snapshot,
                            entries,
                            commit_details,
                        },
                    );
                }
                cx.notify();
                if !errors.is_empty() {
                    this.project.update(cx, |_, cx| {
                        if this.user_triggered {
                            log::error!("failed to get git blame data: {errors:?}");
                            let notification = errors
                                .into_iter()
                                .format_with(",", |e, f| f(&format_args!("{:#}", e)))
                                .to_string();
                            cx.emit(project::Event::Toast {
                                notification_id: "git-blame".into(),
                                message: notification,
                            });
                        } else {
                            // If we weren't triggered by a user, we just log errors in the background, instead of sending
                            // notifications.
                            log::debug!("failed to get git blame data: {errors:?}");
                        }
                    })
                }
            })
        });
    }

    fn regenerate_on_edit(&mut self, cx: &mut Context<Self>) {
        self.regenerate_on_edit_task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(REGENERATE_ON_EDIT_DEBOUNCE_INTERVAL)
                .await;

            this.update(cx, |this, cx| {
                this.generate(cx);
            })
        });
    }
}

const REGENERATE_ON_EDIT_DEBOUNCE_INTERVAL: Duration = Duration::from_secs(2);

fn build_blame_entry_sum_tree(entries: Vec<BlameEntry>, max_row: u32) -> SumTree<GitBlameEntry> {
    let mut current_row = 0;
    let mut entries = SumTree::from_iter(
        entries.into_iter().flat_map(|entry| {
            let mut entries = SmallVec::<[GitBlameEntry; 2]>::new();

            if entry.range.start > current_row {
                let skipped_rows = entry.range.start - current_row;
                entries.push(GitBlameEntry {
                    rows: skipped_rows,
                    blame: None,
                });
            }
            entries.push(GitBlameEntry {
                rows: entry.range.len() as u32,
                blame: Some(entry.clone()),
            });

            current_row = entry.range.end;
            entries
        }),
        &(),
    );

    if max_row >= current_row {
        entries.push(
            GitBlameEntry {
                rows: (max_row + 1) - current_row,
                blame: None,
            },
            &(),
        );
    }

    entries
}

async fn parse_commit_messages(
    messages: impl IntoIterator<Item = (Oid, String)>,
    remote_url: Option<String>,
    provider_registry: Arc<GitHostingProviderRegistry>,
) -> HashMap<Oid, ParsedCommitMessage> {
    let mut commit_details = HashMap::default();

    let parsed_remote_url = remote_url
        .as_deref()
        .and_then(|remote_url| parse_git_remote_url(provider_registry, remote_url));

    for (oid, message) in messages {
        let permalink = if let Some((provider, git_remote)) = parsed_remote_url.as_ref() {
            Some(provider.build_commit_permalink(
                git_remote,
                git::BuildCommitPermalinkParams {
                    sha: oid.to_string().as_str(),
                },
            ))
        } else {
            None
        };

        let remote = parsed_remote_url
            .as_ref()
            .map(|(provider, remote)| GitRemote {
                host: provider.clone(),
                owner: remote.owner.to_string(),
                repo: remote.repo.to_string(),
            });

        let pull_request = parsed_remote_url
            .as_ref()
            .and_then(|(provider, remote)| provider.extract_pull_request(remote, &message));

        commit_details.insert(
            oid,
            ParsedCommitMessage {
                message: message.into(),
                permalink,
                remote,
                pull_request,
            },
        );
    }

    commit_details
}
