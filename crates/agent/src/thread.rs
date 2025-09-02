use crate::{
    agent_profile::AgentProfile,
    context::{AgentContext, AgentContextHandle, ContextLoadResult, LoadedContext},
    thread_store::{
        SerializedCrease, SerializedLanguageModel, SerializedMessage, SerializedMessageSegment,
        SerializedThread, SerializedToolResult, SerializedToolUse, SharedProjectContext,
        ThreadStore,
    },
    tool_use::{PendingToolUse, ToolUse, ToolUseMetadata, ToolUseState},
};
use action_log::ActionLog;
use agent_settings::{
    AgentProfileId, AgentSettings, CompletionMode, SUMMARIZE_THREAD_DETAILED_PROMPT,
    SUMMARIZE_THREAD_PROMPT,
};
use anyhow::{Result, anyhow};
use assistant_tool::{AnyToolCard, Tool, ToolWorkingSet};
use chrono::{DateTime, Utc};
use client::{ModelRequestUsage, RequestUsage};
use cloud_llm_client::{CompletionIntent, CompletionRequestStatus, Plan, UsageLimit};
use collections::HashMap;
use futures::{FutureExt, StreamExt as _, future::Shared};
use git::repository::DiffType;
use gpui::{
    AnyWindowHandle, App, AppContext, AsyncApp, Context, Entity, EventEmitter, SharedString, Task,
    WeakEntity, Window,
};
use http_client::StatusCode;
use language_model::{
    ConfiguredModel, LanguageModel, LanguageModelCompletionError, LanguageModelCompletionEvent,
    LanguageModelExt as _, LanguageModelId, LanguageModelRegistry, LanguageModelRequest,
    LanguageModelRequestMessage, LanguageModelRequestTool, LanguageModelToolResult,
    LanguageModelToolResultContent, LanguageModelToolUse, LanguageModelToolUseId, MessageContent,
    ModelRequestLimitReachedError, PaymentRequiredError, Role, SelectedModel, StopReason,
    TokenUsage,
};
use postage::stream::Stream as _;
use project::{
    Project,
    git_store::{GitStore, GitStoreCheckpoint, RepositoryState},
};
use prompt_store::{ModelContext, PromptBuilder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings;
use std::{
    io::Write,
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;
use util::{ResultExt as _, post_inc};
use uuid::Uuid;

const MAX_RETRY_ATTEMPTS: u8 = 4;
const BASE_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
enum RetryStrategy {
    ExponentialBackoff {
        initial_delay: Duration,
        max_attempts: u8,
    },
    Fixed {
        delay: Duration,
        max_attempts: u8,
    },
}

#[derive(
    Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Serialize, Deserialize, JsonSchema,
)]
pub struct ThreadId(Arc<str>);

impl ThreadId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string().into())
    }
}

impl std::fmt::Display for ThreadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for ThreadId {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

/// The ID of the user prompt that initiated a request.
///
/// This equates to the user physically submitting a message to the model (e.g., by pressing the Enter key).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Serialize, Deserialize)]
pub struct PromptId(Arc<str>);

impl PromptId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string().into())
    }
}

impl std::fmt::Display for PromptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, Serialize, Deserialize)]
pub struct MessageId(pub usize);

impl MessageId {
    fn post_inc(&mut self) -> Self {
        Self(post_inc(&mut self.0))
    }

    pub fn as_usize(&self) -> usize {
        self.0
    }
}

/// Stored information that can be used to resurrect a context crease when creating an editor for a past message.
#[derive(Clone, Debug)]
pub struct MessageCrease {
    pub range: Range<usize>,
    pub icon_path: SharedString,
    pub label: SharedString,
    /// None for a deserialized message, Some otherwise.
    pub context: Option<AgentContextHandle>,
}

/// A message in a [`Thread`].
#[derive(Debug, Clone)]
pub struct Message {
    pub id: MessageId,
    pub role: Role,
    pub segments: Vec<MessageSegment>,
    pub loaded_context: LoadedContext,
    pub creases: Vec<MessageCrease>,
    pub is_hidden: bool,
    pub ui_only: bool,
}

impl Message {
    /// Returns whether the message contains any meaningful text that should be displayed
    /// The model sometimes runs tool without producing any text or just a marker ([`USING_TOOL_MARKER`])
    pub fn should_display_content(&self) -> bool {
        self.segments.iter().all(|segment| segment.should_display())
    }

    pub fn push_thinking(&mut self, text: &str, signature: Option<String>) {
        if let Some(MessageSegment::Thinking {
            text: segment,
            signature: current_signature,
        }) = self.segments.last_mut()
        {
            if let Some(signature) = signature {
                *current_signature = Some(signature);
            }
            segment.push_str(text);
        } else {
            self.segments.push(MessageSegment::Thinking {
                text: text.to_string(),
                signature,
            });
        }
    }

    pub fn push_redacted_thinking(&mut self, data: String) {
        self.segments.push(MessageSegment::RedactedThinking(data));
    }

    pub fn push_text(&mut self, text: &str) {
        if let Some(MessageSegment::Text(segment)) = self.segments.last_mut() {
            segment.push_str(text);
        } else {
            self.segments.push(MessageSegment::Text(text.to_string()));
        }
    }

    pub fn to_message_content(&self) -> String {
        let mut result = String::new();

        if !self.loaded_context.text.is_empty() {
            result.push_str(&self.loaded_context.text);
        }

        for segment in &self.segments {
            match segment {
                MessageSegment::Text(text) => result.push_str(text),
                MessageSegment::Thinking { text, .. } => {
                    result.push_str("<think>\n");
                    result.push_str(text);
                    result.push_str("\n</think>");
                }
                MessageSegment::RedactedThinking(_) => {}
            }
        }

        result
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageSegment {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    RedactedThinking(String),
}

impl MessageSegment {
    pub fn should_display(&self) -> bool {
        match self {
            Self::Text(text) => text.is_empty(),
            Self::Thinking { text, .. } => text.is_empty(),
            Self::RedactedThinking(_) => false,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            MessageSegment::Text(text) => Some(text),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectSnapshot {
    pub worktree_snapshots: Vec<WorktreeSnapshot>,
    pub unsaved_buffer_paths: Vec<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorktreeSnapshot {
    pub worktree_path: String,
    pub git_state: Option<GitState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitState {
    pub remote_url: Option<String>,
    pub head_sha: Option<String>,
    pub current_branch: Option<String>,
    pub diff: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ThreadCheckpoint {
    message_id: MessageId,
    git_checkpoint: GitStoreCheckpoint,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ThreadFeedback {
    Positive,
    Negative,
}

pub enum LastRestoreCheckpoint {
    Pending {
        message_id: MessageId,
    },
    Error {
        message_id: MessageId,
        error: String,
    },
}

impl LastRestoreCheckpoint {
    pub fn message_id(&self) -> MessageId {
        match self {
            LastRestoreCheckpoint::Pending { message_id } => *message_id,
            LastRestoreCheckpoint::Error { message_id, .. } => *message_id,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum DetailedSummaryState {
    #[default]
    NotGenerated,
    Generating {
        message_id: MessageId,
    },
    Generated {
        text: SharedString,
        message_id: MessageId,
    },
}

impl DetailedSummaryState {
    fn text(&self) -> Option<SharedString> {
        if let Self::Generated { text, .. } = self {
            Some(text.clone())
        } else {
            None
        }
    }
}

#[derive(Default, Debug)]
pub struct TotalTokenUsage {
    pub total: u64,
    pub max: u64,
}

impl TotalTokenUsage {
    pub fn ratio(&self) -> TokenUsageRatio {
        let warning_threshold: f32 = 0.8;

        // When the maximum is unknown because there is no selected model,
        // avoid showing the token limit warning.
        if self.max == 0 {
            TokenUsageRatio::Normal
        } else if self.total >= self.max {
            TokenUsageRatio::Exceeded
        } else if self.total as f32 / self.max as f32 >= warning_threshold {
            TokenUsageRatio::Warning
        } else {
            TokenUsageRatio::Normal
        }
    }

    pub fn add(&self, tokens: u64) -> TotalTokenUsage {
        TotalTokenUsage {
            total: self.total + tokens,
            max: self.max,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub enum TokenUsageRatio {
    #[default]
    Normal,
    Warning,
    Exceeded,
}

#[derive(Debug, Clone, Copy)]
pub enum QueueState {
    Sending,
    Queued { position: usize },
    Started,
}

/// A thread of conversation with the LLM.
pub struct Thread {
    id: ThreadId,
    updated_at: DateTime<Utc>,
    summary: ThreadSummary,
    pending_summary: Task<Option<()>>,
    detailed_summary_task: Task<Option<()>>,
    detailed_summary_tx: postage::watch::Sender<DetailedSummaryState>,
    detailed_summary_rx: postage::watch::Receiver<DetailedSummaryState>,
    completion_mode: agent_settings::CompletionMode,
    messages: Vec<Message>,
    next_message_id: MessageId,
    last_prompt_id: PromptId,
    project_context: SharedProjectContext,
    checkpoints_by_message: HashMap<MessageId, ThreadCheckpoint>,
    completion_count: usize,
    pending_completions: Vec<PendingCompletion>,
    project: Entity<Project>,
    prompt_builder: Arc<PromptBuilder>,
    tools: Entity<ToolWorkingSet>,
    tool_use: ToolUseState,
    action_log: Entity<ActionLog>,
    last_restore_checkpoint: Option<LastRestoreCheckpoint>,
    pending_checkpoint: Option<ThreadCheckpoint>,
    initial_project_snapshot: Shared<Task<Option<Arc<ProjectSnapshot>>>>,
    request_token_usage: Vec<TokenUsage>,
    cumulative_token_usage: TokenUsage,
    exceeded_window_error: Option<ExceededWindowError>,
    tool_use_limit_reached: bool,
    retry_state: Option<RetryState>,
    message_feedback: HashMap<MessageId, ThreadFeedback>,
    last_received_chunk_at: Option<Instant>,
    request_callback: Option<
        Box<dyn FnMut(&LanguageModelRequest, &[Result<LanguageModelCompletionEvent, String>])>,
    >,
    remaining_turns: u32,
    configured_model: Option<ConfiguredModel>,
    profile: AgentProfile,
    last_error_context: Option<(Arc<dyn LanguageModel>, CompletionIntent)>,
}

#[derive(Clone, Debug)]
struct RetryState {
    attempt: u8,
    max_attempts: u8,
    intent: CompletionIntent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadSummary {
    Pending,
    Generating,
    Ready(SharedString),
    Error,
}

impl ThreadSummary {
    pub const DEFAULT: SharedString = SharedString::new_static("New Thread");

    pub fn or_default(&self) -> SharedString {
        self.unwrap_or(Self::DEFAULT)
    }

    pub fn unwrap_or(&self, message: impl Into<SharedString>) -> SharedString {
        self.ready().unwrap_or_else(|| message.into())
    }

    pub fn ready(&self) -> Option<SharedString> {
        match self {
            ThreadSummary::Ready(summary) => Some(summary.clone()),
            ThreadSummary::Pending | ThreadSummary::Generating | ThreadSummary::Error => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExceededWindowError {
    /// Model used when last message exceeded context window
    model_id: LanguageModelId,
    /// Token count including last message
    token_count: u64,
}

impl Thread {
    pub fn new(
        project: Entity<Project>,
        tools: Entity<ToolWorkingSet>,
        prompt_builder: Arc<PromptBuilder>,
        system_prompt: SharedProjectContext,
        cx: &mut Context<Self>,
    ) -> Self {
        let (detailed_summary_tx, detailed_summary_rx) = postage::watch::channel();
        let configured_model = LanguageModelRegistry::read_global(cx).default_model();
        let profile_id = AgentSettings::get_global(cx).default_profile.clone();

        Self {
            id: ThreadId::new(),
            updated_at: Utc::now(),
            summary: ThreadSummary::Pending,
            pending_summary: Task::ready(None),
            detailed_summary_task: Task::ready(None),
            detailed_summary_tx,
            detailed_summary_rx,
            completion_mode: AgentSettings::get_global(cx).preferred_completion_mode,
            messages: Vec::new(),
            next_message_id: MessageId(0),
            last_prompt_id: PromptId::new(),
            project_context: system_prompt,
            checkpoints_by_message: HashMap::default(),
            completion_count: 0,
            pending_completions: Vec::new(),
            project: project.clone(),
            prompt_builder,
            tools: tools.clone(),
            last_restore_checkpoint: None,
            pending_checkpoint: None,
            tool_use: ToolUseState::new(tools.clone()),
            action_log: cx.new(|_| ActionLog::new(project.clone())),
            initial_project_snapshot: {
                let project_snapshot = Self::project_snapshot(project, cx);
                cx.foreground_executor()
                    .spawn(async move { Some(project_snapshot.await) })
                    .shared()
            },
            request_token_usage: Vec::new(),
            cumulative_token_usage: TokenUsage::default(),
            exceeded_window_error: None,
            tool_use_limit_reached: false,
            retry_state: None,
            message_feedback: HashMap::default(),
            last_error_context: None,
            last_received_chunk_at: None,
            request_callback: None,
            remaining_turns: u32::MAX,
            configured_model,
            profile: AgentProfile::new(profile_id, tools),
        }
    }

    pub fn deserialize(
        id: ThreadId,
        serialized: SerializedThread,
        project: Entity<Project>,
        tools: Entity<ToolWorkingSet>,
        prompt_builder: Arc<PromptBuilder>,
        project_context: SharedProjectContext,
        window: Option<&mut Window>, // None in headless mode
        cx: &mut Context<Self>,
    ) -> Self {
        let next_message_id = MessageId(
            serialized
                .messages
                .last()
                .map(|message| message.id.0 + 1)
                .unwrap_or(0),
        );
        let tool_use = ToolUseState::from_serialized_messages(
            tools.clone(),
            &serialized.messages,
            project.clone(),
            window,
            cx,
        );
        let (detailed_summary_tx, detailed_summary_rx) =
            postage::watch::channel_with(serialized.detailed_summary_state);

        let configured_model = LanguageModelRegistry::global(cx).update(cx, |registry, cx| {
            serialized
                .model
                .and_then(|model| {
                    let model = SelectedModel {
                        provider: model.provider.clone().into(),
                        model: model.model.into(),
                    };
                    registry.select_model(&model, cx)
                })
                .or_else(|| registry.default_model())
        });

        let completion_mode = serialized
            .completion_mode
            .unwrap_or_else(|| AgentSettings::get_global(cx).preferred_completion_mode);
        let profile_id = serialized
            .profile
            .unwrap_or_else(|| AgentSettings::get_global(cx).default_profile.clone());

        Self {
            id,
            updated_at: serialized.updated_at,
            summary: ThreadSummary::Ready(serialized.summary),
            pending_summary: Task::ready(None),
            detailed_summary_task: Task::ready(None),
            detailed_summary_tx,
            detailed_summary_rx,
            completion_mode,
            retry_state: None,
            messages: serialized
                .messages
                .into_iter()
                .map(|message| Message {
                    id: message.id,
                    role: message.role,
                    segments: message
                        .segments
                        .into_iter()
                        .map(|segment| match segment {
                            SerializedMessageSegment::Text { text } => MessageSegment::Text(text),
                            SerializedMessageSegment::Thinking { text, signature } => {
                                MessageSegment::Thinking { text, signature }
                            }
                            SerializedMessageSegment::RedactedThinking { data } => {
                                MessageSegment::RedactedThinking(data)
                            }
                        })
                        .collect(),
                    loaded_context: LoadedContext {
                        contexts: Vec::new(),
                        text: message.context,
                        images: Vec::new(),
                    },
                    creases: message
                        .creases
                        .into_iter()
                        .map(|crease| MessageCrease {
                            range: crease.start..crease.end,
                            icon_path: crease.icon_path,
                            label: crease.label,
                            context: None,
                        })
                        .collect(),
                    is_hidden: message.is_hidden,
                    ui_only: false, // UI-only messages are not persisted
                })
                .collect(),
            next_message_id,
            last_prompt_id: PromptId::new(),
            project_context,
            checkpoints_by_message: HashMap::default(),
            completion_count: 0,
            pending_completions: Vec::new(),
            last_restore_checkpoint: None,
            pending_checkpoint: None,
            project: project.clone(),
            prompt_builder,
            tools: tools.clone(),
            tool_use,
            action_log: cx.new(|_| ActionLog::new(project)),
            initial_project_snapshot: Task::ready(serialized.initial_project_snapshot).shared(),
            request_token_usage: serialized.request_token_usage,
            cumulative_token_usage: serialized.cumulative_token_usage,
            exceeded_window_error: None,
            tool_use_limit_reached: serialized.tool_use_limit_reached,
            message_feedback: HashMap::default(),
            last_error_context: None,
            last_received_chunk_at: None,
            request_callback: None,
            remaining_turns: u32::MAX,
            configured_model,
            profile: AgentProfile::new(profile_id, tools),
        }
    }

    pub fn set_request_callback(
        &mut self,
        callback: impl 'static
        + FnMut(&LanguageModelRequest, &[Result<LanguageModelCompletionEvent, String>]),
    ) {
        self.request_callback = Some(Box::new(callback));
    }

    pub fn id(&self) -> &ThreadId {
        &self.id
    }

    pub fn profile(&self) -> &AgentProfile {
        &self.profile
    }

    pub fn set_profile(&mut self, id: AgentProfileId, cx: &mut Context<Self>) {
        if &id != self.profile.id() {
            self.profile = AgentProfile::new(id, self.tools.clone());
            cx.emit(ThreadEvent::ProfileChanged);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }

    pub fn touch_updated_at(&mut self) {
        self.updated_at = Utc::now();
    }

    pub fn advance_prompt_id(&mut self) {
        self.last_prompt_id = PromptId::new();
    }

    pub fn project_context(&self) -> SharedProjectContext {
        self.project_context.clone()
    }

    pub fn get_or_init_configured_model(&mut self, cx: &App) -> Option<ConfiguredModel> {
        if self.configured_model.is_none() {
            self.configured_model = LanguageModelRegistry::read_global(cx).default_model();
        }
        self.configured_model.clone()
    }

    pub fn configured_model(&self) -> Option<ConfiguredModel> {
        self.configured_model.clone()
    }

    pub fn set_configured_model(&mut self, model: Option<ConfiguredModel>, cx: &mut Context<Self>) {
        self.configured_model = model;
        cx.notify();
    }

    pub fn summary(&self) -> &ThreadSummary {
        &self.summary
    }

    pub fn set_summary(&mut self, new_summary: impl Into<SharedString>, cx: &mut Context<Self>) {
        let current_summary = match &self.summary {
            ThreadSummary::Pending | ThreadSummary::Generating => return,
            ThreadSummary::Ready(summary) => summary,
            ThreadSummary::Error => &ThreadSummary::DEFAULT,
        };

        let mut new_summary = new_summary.into();

        if new_summary.is_empty() {
            new_summary = ThreadSummary::DEFAULT;
        }

        if current_summary != &new_summary {
            self.summary = ThreadSummary::Ready(new_summary);
            cx.emit(ThreadEvent::SummaryChanged);
        }
    }

    pub fn completion_mode(&self) -> CompletionMode {
        self.completion_mode
    }

    pub fn set_completion_mode(&mut self, mode: CompletionMode) {
        self.completion_mode = mode;
    }

    pub fn message(&self, id: MessageId) -> Option<&Message> {
        let index = self
            .messages
            .binary_search_by(|message| message.id.cmp(&id))
            .ok()?;

        self.messages.get(index)
    }

    pub fn messages(&self) -> impl ExactSizeIterator<Item = &Message> {
        self.messages.iter()
    }

    pub fn is_generating(&self) -> bool {
        !self.pending_completions.is_empty() || !self.all_tools_finished()
    }

    /// Indicates whether streaming of language model events is stale.
    /// When `is_generating()` is false, this method returns `None`.
    pub fn is_generation_stale(&self) -> Option<bool> {
        const STALE_THRESHOLD: u128 = 250;

        self.last_received_chunk_at
            .map(|instant| instant.elapsed().as_millis() > STALE_THRESHOLD)
    }

    fn received_chunk(&mut self) {
        self.last_received_chunk_at = Some(Instant::now());
    }

    pub fn queue_state(&self) -> Option<QueueState> {
        self.pending_completions
            .first()
            .map(|pending_completion| pending_completion.queue_state)
    }

    pub fn tools(&self) -> &Entity<ToolWorkingSet> {
        &self.tools
    }

    pub fn pending_tool(&self, id: &LanguageModelToolUseId) -> Option<&PendingToolUse> {
        self.tool_use
            .pending_tool_uses()
            .into_iter()
            .find(|tool_use| &tool_use.id == id)
    }

    pub fn tools_needing_confirmation(&self) -> impl Iterator<Item = &PendingToolUse> {
        self.tool_use
            .pending_tool_uses()
            .into_iter()
            .filter(|tool_use| tool_use.status.needs_confirmation())
    }

    pub fn has_pending_tool_uses(&self) -> bool {
        !self.tool_use.pending_tool_uses().is_empty()
    }

    pub fn checkpoint_for_message(&self, id: MessageId) -> Option<ThreadCheckpoint> {
        self.checkpoints_by_message.get(&id).cloned()
    }

    pub fn restore_checkpoint(
        &mut self,
        checkpoint: ThreadCheckpoint,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.last_restore_checkpoint = Some(LastRestoreCheckpoint::Pending {
            message_id: checkpoint.message_id,
        });
        cx.emit(ThreadEvent::CheckpointChanged);
        cx.notify();

        let git_store = self.project().read(cx).git_store().clone();
        let restore = git_store.update(cx, |git_store, cx| {
            git_store.restore_checkpoint(checkpoint.git_checkpoint.clone(), cx)
        });

        cx.spawn(async move |this, cx| {
            let result = restore.await;
            this.update(cx, |this, cx| {
                if let Err(err) = result.as_ref() {
                    this.last_restore_checkpoint = Some(LastRestoreCheckpoint::Error {
                        message_id: checkpoint.message_id,
                        error: err.to_string(),
                    });
                } else {
                    this.truncate(checkpoint.message_id, cx);
                    this.last_restore_checkpoint = None;
                }
                this.pending_checkpoint = None;
                cx.emit(ThreadEvent::CheckpointChanged);
                cx.notify();
            })?;
            result
        })
    }

    fn finalize_pending_checkpoint(&mut self, cx: &mut Context<Self>) {
        let pending_checkpoint = if self.is_generating() {
            return;
        } else if let Some(checkpoint) = self.pending_checkpoint.take() {
            checkpoint
        } else {
            return;
        };

        self.finalize_checkpoint(pending_checkpoint, cx);
    }

    fn finalize_checkpoint(
        &mut self,
        pending_checkpoint: ThreadCheckpoint,
        cx: &mut Context<Self>,
    ) {
        let git_store = self.project.read(cx).git_store().clone();
        let final_checkpoint = git_store.update(cx, |git_store, cx| git_store.checkpoint(cx));
        cx.spawn(async move |this, cx| match final_checkpoint.await {
            Ok(final_checkpoint) => {
                let equal = git_store
                    .update(cx, |store, cx| {
                        store.compare_checkpoints(
                            pending_checkpoint.git_checkpoint.clone(),
                            final_checkpoint.clone(),
                            cx,
                        )
                    })?
                    .await
                    .unwrap_or(false);

                this.update(cx, |this, cx| {
                    this.pending_checkpoint = if equal {
                        Some(pending_checkpoint)
                    } else {
                        this.insert_checkpoint(pending_checkpoint, cx);
                        Some(ThreadCheckpoint {
                            message_id: this.next_message_id,
                            git_checkpoint: final_checkpoint,
                        })
                    }
                })?;

                Ok(())
            }
            Err(_) => this.update(cx, |this, cx| {
                this.insert_checkpoint(pending_checkpoint, cx)
            }),
        })
        .detach();
    }

    fn insert_checkpoint(&mut self, checkpoint: ThreadCheckpoint, cx: &mut Context<Self>) {
        self.checkpoints_by_message
            .insert(checkpoint.message_id, checkpoint);
        cx.emit(ThreadEvent::CheckpointChanged);
        cx.notify();
    }

    pub fn last_restore_checkpoint(&self) -> Option<&LastRestoreCheckpoint> {
        self.last_restore_checkpoint.as_ref()
    }

    pub fn truncate(&mut self, message_id: MessageId, cx: &mut Context<Self>) {
        let Some(message_ix) = self
            .messages
            .iter()
            .rposition(|message| message.id == message_id)
        else {
            return;
        };
        for deleted_message in self.messages.drain(message_ix..) {
            self.checkpoints_by_message.remove(&deleted_message.id);
        }
        cx.notify();
    }

    pub fn context_for_message(&self, id: MessageId) -> impl Iterator<Item = &AgentContext> {
        self.messages
            .iter()
            .find(|message| message.id == id)
            .into_iter()
            .flat_map(|message| message.loaded_context.contexts.iter())
    }

    pub fn is_turn_end(&self, ix: usize) -> bool {
        if self.messages.is_empty() {
            return false;
        }

        if !self.is_generating() && ix == self.messages.len() - 1 {
            return true;
        }

        let Some(message) = self.messages.get(ix) else {
            return false;
        };

        if message.role != Role::Assistant {
            return false;
        }

        self.messages
            .get(ix + 1)
            .and_then(|message| {
                self.message(message.id)
                    .map(|next_message| next_message.role == Role::User && !next_message.is_hidden)
            })
            .unwrap_or(false)
    }

    pub fn tool_use_limit_reached(&self) -> bool {
        self.tool_use_limit_reached
    }

    /// Returns whether all of the tool uses have finished running.
    pub fn all_tools_finished(&self) -> bool {
        // If the only pending tool uses left are the ones with errors, then
        // that means that we've finished running all of the pending tools.
        self.tool_use
            .pending_tool_uses()
            .iter()
            .all(|pending_tool_use| pending_tool_use.status.is_error())
    }

    /// Returns whether any pending tool uses may perform edits
    pub fn has_pending_edit_tool_uses(&self) -> bool {
        self.tool_use
            .pending_tool_uses()
            .iter()
            .filter(|pending_tool_use| !pending_tool_use.status.is_error())
            .any(|pending_tool_use| pending_tool_use.may_perform_edits)
    }

    pub fn tool_uses_for_message(&self, id: MessageId, cx: &App) -> Vec<ToolUse> {
        self.tool_use.tool_uses_for_message(id, &self.project, cx)
    }

    pub fn tool_results_for_message(
        &self,
        assistant_message_id: MessageId,
    ) -> Vec<&LanguageModelToolResult> {
        self.tool_use.tool_results_for_message(assistant_message_id)
    }

    pub fn tool_result(&self, id: &LanguageModelToolUseId) -> Option<&LanguageModelToolResult> {
        self.tool_use.tool_result(id)
    }

    pub fn output_for_tool(&self, id: &LanguageModelToolUseId) -> Option<&Arc<str>> {
        match &self.tool_use.tool_result(id)?.content {
            LanguageModelToolResultContent::Text(text) => Some(text),
            LanguageModelToolResultContent::Image(_) => {
                // TODO: We should display image
                None
            }
        }
    }

    pub fn card_for_tool(&self, id: &LanguageModelToolUseId) -> Option<AnyToolCard> {
        self.tool_use.tool_result_card(id).cloned()
    }

    /// Return tools that are both enabled and supported by the model
    pub fn available_tools(
        &self,
        cx: &App,
        model: Arc<dyn LanguageModel>,
    ) -> Vec<LanguageModelRequestTool> {
        if model.supports_tools() {
            self.profile
                .enabled_tools(cx)
                .into_iter()
                .filter_map(|(name, tool)| {
                    // Skip tools that cannot be supported
                    let input_schema = tool.input_schema(model.tool_input_format()).ok()?;
                    Some(LanguageModelRequestTool {
                        name: name.into(),
                        description: tool.description(),
                        input_schema,
                    })
                })
                .collect()
        } else {
            Vec::default()
        }
    }

    pub fn insert_user_message(
        &mut self,
        text: impl Into<String>,
        loaded_context: ContextLoadResult,
        git_checkpoint: Option<GitStoreCheckpoint>,
        creases: Vec<MessageCrease>,
        cx: &mut Context<Self>,
    ) -> MessageId {
        if !loaded_context.referenced_buffers.is_empty() {
            self.action_log.update(cx, |log, cx| {
                for buffer in loaded_context.referenced_buffers {
                    log.buffer_read(buffer, cx);
                }
            });
        }

        let message_id = self.insert_message(
            Role::User,
            vec![MessageSegment::Text(text.into())],
            loaded_context.loaded_context,
            creases,
            false,
            cx,
        );

        if let Some(git_checkpoint) = git_checkpoint {
            self.pending_checkpoint = Some(ThreadCheckpoint {
                message_id,
                git_checkpoint,
            });
        }

        message_id
    }

    pub fn insert_invisible_continue_message(&mut self, cx: &mut Context<Self>) -> MessageId {
        let id = self.insert_message(
            Role::User,
            vec![MessageSegment::Text("Continue where you left off".into())],
            LoadedContext::default(),
            vec![],
            true,
            cx,
        );
        self.pending_checkpoint = None;

        id
    }

    pub fn insert_assistant_message(
        &mut self,
        segments: Vec<MessageSegment>,
        cx: &mut Context<Self>,
    ) -> MessageId {
        self.insert_message(
            Role::Assistant,
            segments,
            LoadedContext::default(),
            Vec::new(),
            false,
            cx,
        )
    }

    pub fn insert_message(
        &mut self,
        role: Role,
        segments: Vec<MessageSegment>,
        loaded_context: LoadedContext,
        creases: Vec<MessageCrease>,
        is_hidden: bool,
        cx: &mut Context<Self>,
    ) -> MessageId {
        let id = self.next_message_id.post_inc();
        self.messages.push(Message {
            id,
            role,
            segments,
            loaded_context,
            creases,
            is_hidden,
            ui_only: false,
        });
        self.touch_updated_at();
        cx.emit(ThreadEvent::MessageAdded(id));
        id
    }

    pub fn edit_message(
        &mut self,
        id: MessageId,
        new_role: Role,
        new_segments: Vec<MessageSegment>,
        creases: Vec<MessageCrease>,
        loaded_context: Option<LoadedContext>,
        checkpoint: Option<GitStoreCheckpoint>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(message) = self.messages.iter_mut().find(|message| message.id == id) else {
            return false;
        };
        message.role = new_role;
        message.segments = new_segments;
        message.creases = creases;
        if let Some(context) = loaded_context {
            message.loaded_context = context;
        }
        if let Some(git_checkpoint) = checkpoint {
            self.checkpoints_by_message.insert(
                id,
                ThreadCheckpoint {
                    message_id: id,
                    git_checkpoint,
                },
            );
        }
        self.touch_updated_at();
        cx.emit(ThreadEvent::MessageEdited(id));
        true
    }

    pub fn delete_message(&mut self, id: MessageId, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.messages.iter().position(|message| message.id == id) else {
            return false;
        };
        self.messages.remove(index);
        self.touch_updated_at();
        cx.emit(ThreadEvent::MessageDeleted(id));
        true
    }

    /// Returns the representation of this [`Thread`] in a textual form.
    ///
    /// This is the representation we use when attaching a thread as context to another thread.
    pub fn text(&self) -> String {
        let mut text = String::new();

        for message in &self.messages {
            text.push_str(match message.role {
                language_model::Role::User => "User:",
                language_model::Role::Assistant => "Agent:",
                language_model::Role::System => "System:",
            });
            text.push('\n');

            for segment in &message.segments {
                match segment {
                    MessageSegment::Text(content) => text.push_str(content),
                    MessageSegment::Thinking { text: content, .. } => {
                        text.push_str(&format!("<think>{}</think>", content))
                    }
                    MessageSegment::RedactedThinking(_) => {}
                }
            }
            text.push('\n');
        }

        text
    }

    /// Serializes this thread into a format for storage or telemetry.
    pub fn serialize(&self, cx: &mut Context<Self>) -> Task<Result<SerializedThread>> {
        let initial_project_snapshot = self.initial_project_snapshot.clone();
        cx.spawn(async move |this, cx| {
            let initial_project_snapshot = initial_project_snapshot.await;
            this.read_with(cx, |this, cx| SerializedThread {
                version: SerializedThread::VERSION.to_string(),
                summary: this.summary().or_default(),
                updated_at: this.updated_at(),
                messages: this
                    .messages()
                    .filter(|message| !message.ui_only)
                    .map(|message| SerializedMessage {
                        id: message.id,
                        role: message.role,
                        segments: message
                            .segments
                            .iter()
                            .map(|segment| match segment {
                                MessageSegment::Text(text) => {
                                    SerializedMessageSegment::Text { text: text.clone() }
                                }
                                MessageSegment::Thinking { text, signature } => {
                                    SerializedMessageSegment::Thinking {
                                        text: text.clone(),
                                        signature: signature.clone(),
                                    }
                                }
                                MessageSegment::RedactedThinking(data) => {
                                    SerializedMessageSegment::RedactedThinking {
                                        data: data.clone(),
                                    }
                                }
                            })
                            .collect(),
                        tool_uses: this
                            .tool_uses_for_message(message.id, cx)
                            .into_iter()
                            .map(|tool_use| SerializedToolUse {
                                id: tool_use.id,
                                name: tool_use.name,
                                input: tool_use.input,
                            })
                            .collect(),
                        tool_results: this
                            .tool_results_for_message(message.id)
                            .into_iter()
                            .map(|tool_result| SerializedToolResult {
                                tool_use_id: tool_result.tool_use_id.clone(),
                                is_error: tool_result.is_error,
                                content: tool_result.content.clone(),
                                output: tool_result.output.clone(),
                            })
                            .collect(),
                        context: message.loaded_context.text.clone(),
                        creases: message
                            .creases
                            .iter()
                            .map(|crease| SerializedCrease {
                                start: crease.range.start,
                                end: crease.range.end,
                                icon_path: crease.icon_path.clone(),
                                label: crease.label.clone(),
                            })
                            .collect(),
                        is_hidden: message.is_hidden,
                    })
                    .collect(),
                initial_project_snapshot,
                cumulative_token_usage: this.cumulative_token_usage,
                request_token_usage: this.request_token_usage.clone(),
                detailed_summary_state: this.detailed_summary_rx.borrow().clone(),
                exceeded_window_error: this.exceeded_window_error.clone(),
                model: this
                    .configured_model
                    .as_ref()
                    .map(|model| SerializedLanguageModel {
                        provider: model.provider.id().0.to_string(),
                        model: model.model.id().0.to_string(),
                    }),
                completion_mode: Some(this.completion_mode),
                tool_use_limit_reached: this.tool_use_limit_reached,
                profile: Some(this.profile.id().clone()),
            })
        })
    }

    pub fn remaining_turns(&self) -> u32 {
        self.remaining_turns
    }

    pub fn set_remaining_turns(&mut self, remaining_turns: u32) {
        self.remaining_turns = remaining_turns;
    }

    pub fn send_to_model(
        &mut self,
        model: Arc<dyn LanguageModel>,
        intent: CompletionIntent,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        if self.remaining_turns == 0 {
            return;
        }

        self.remaining_turns -= 1;

        self.flush_notifications(model.clone(), intent, cx);

        let _checkpoint = self.finalize_pending_checkpoint(cx);
        self.stream_completion(
            self.to_completion_request(model.clone(), intent, cx),
            model,
            intent,
            window,
            cx,
        );
    }

    pub fn retry_last_completion(
        &mut self,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        // Clear any existing error state
        self.retry_state = None;

        // Use the last error context if available, otherwise fall back to configured model
        let (model, intent) = if let Some((model, intent)) = self.last_error_context.take() {
            (model, intent)
        } else if let Some(configured_model) = self.configured_model.as_ref() {
            let model = configured_model.model.clone();
            let intent = if self.has_pending_tool_uses() {
                CompletionIntent::ToolResults
            } else {
                CompletionIntent::UserPrompt
            };
            (model, intent)
        } else if let Some(configured_model) = self.get_or_init_configured_model(cx) {
            let model = configured_model.model.clone();
            let intent = if self.has_pending_tool_uses() {
                CompletionIntent::ToolResults
            } else {
                CompletionIntent::UserPrompt
            };
            (model, intent)
        } else {
            return;
        };

        self.send_to_model(model, intent, window, cx);
    }

    pub fn enable_burn_mode_and_retry(
        &mut self,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        self.completion_mode = CompletionMode::Burn;
        cx.emit(ThreadEvent::ProfileChanged);
        self.retry_last_completion(window, cx);
    }

    pub fn used_tools_since_last_user_message(&self) -> bool {
        for message in self.messages.iter().rev() {
            if self.tool_use.message_has_tool_results(message.id) {
                return true;
            } else if message.role == Role::User {
                return false;
            }
        }

        false
    }

    pub fn to_completion_request(
        &self,
        model: Arc<dyn LanguageModel>,
        intent: CompletionIntent,
        cx: &mut Context<Self>,
    ) -> LanguageModelRequest {
        let mut request = LanguageModelRequest {
            thread_id: Some(self.id.to_string()),
            prompt_id: Some(self.last_prompt_id.to_string()),
            intent: Some(intent),
            mode: None,
            messages: vec![],
            tools: Vec::new(),
            tool_choice: None,
            stop: Vec::new(),
            temperature: AgentSettings::temperature_for_model(&model, cx),
            thinking_allowed: true,
        };

        let available_tools = self.available_tools(cx, model.clone());
        let available_tool_names = available_tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect();

        let model_context = &ModelContext {
            available_tools: available_tool_names,
        };

        if let Some(project_context) = self.project_context.borrow().as_ref() {
            match self
                .prompt_builder
                .generate_assistant_system_prompt(project_context, model_context)
            {
                Err(err) => {
                    let message = format!("{err:?}").into();
                    log::error!("{message}");
                    cx.emit(ThreadEvent::ShowError(ThreadError::Message {
                        header: "Error generating system prompt".into(),
                        message,
                    }));
                }
                Ok(system_prompt) => {
                    request.messages.push(LanguageModelRequestMessage {
                        role: Role::System,
                        content: vec![MessageContent::Text(system_prompt)],
                        cache: true,
                    });
                }
            }
        } else {
            let message = "Context for system prompt unexpectedly not ready.".into();
            log::error!("{message}");
            cx.emit(ThreadEvent::ShowError(ThreadError::Message {
                header: "Error generating system prompt".into(),
                message,
            }));
        }

        let mut message_ix_to_cache = None;
        for message in &self.messages {
            // ui_only messages are for the UI only, not for the model
            if message.ui_only {
                continue;
            }

            let mut request_message = LanguageModelRequestMessage {
                role: message.role,
                content: Vec::new(),
                cache: false,
            };

            message
                .loaded_context
                .add_to_request_message(&mut request_message);

            for segment in &message.segments {
                match segment {
                    MessageSegment::Text(text) => {
                        let text = text.trim_end();
                        if !text.is_empty() {
                            request_message
                                .content
                                .push(MessageContent::Text(text.into()));
                        }
                    }
                    MessageSegment::Thinking { text, signature } => {
                        if !text.is_empty() {
                            request_message.content.push(MessageContent::Thinking {
                                text: text.into(),
                                signature: signature.clone(),
                            });
                        }
                    }
                    MessageSegment::RedactedThinking(data) => {
                        request_message
                            .content
                            .push(MessageContent::RedactedThinking(data.clone()));
                    }
                };
            }

            let mut cache_message = true;
            let mut tool_results_message = LanguageModelRequestMessage {
                role: Role::User,
                content: Vec::new(),
                cache: false,
            };
            for (tool_use, tool_result) in self.tool_use.tool_results(message.id) {
                if let Some(tool_result) = tool_result {
                    request_message
                        .content
                        .push(MessageContent::ToolUse(tool_use.clone()));
                    tool_results_message
                        .content
                        .push(MessageContent::ToolResult(LanguageModelToolResult {
                            tool_use_id: tool_use.id.clone(),
                            tool_name: tool_result.tool_name.clone(),
                            is_error: tool_result.is_error,
                            content: if tool_result.content.is_empty() {
                                // Surprisingly, the API fails if we return an empty string here.
                                // It thinks we are sending a tool use without a tool result.
                                "<Tool returned an empty string>".into()
                            } else {
                                tool_result.content.clone()
                            },
                            output: None,
                        }));
                } else {
                    cache_message = false;
                    log::debug!(
                        "skipped tool use {:?} because it is still pending",
                        tool_use
                    );
                }
            }

            if cache_message {
                message_ix_to_cache = Some(request.messages.len());
            }
            request.messages.push(request_message);

            if !tool_results_message.content.is_empty() {
                if cache_message {
                    message_ix_to_cache = Some(request.messages.len());
                }
                request.messages.push(tool_results_message);
            }
        }

        // https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching
        if let Some(message_ix_to_cache) = message_ix_to_cache {
            request.messages[message_ix_to_cache].cache = true;
        }

        request.tools = available_tools;
        request.mode = if model.supports_burn_mode() {
            Some(self.completion_mode.into())
        } else {
            Some(CompletionMode::Normal.into())
        };

        request
    }

    fn to_summarize_request(
        &self,
        model: &Arc<dyn LanguageModel>,
        intent: CompletionIntent,
        added_user_message: String,
        cx: &App,
    ) -> LanguageModelRequest {
        let mut request = LanguageModelRequest {
            thread_id: None,
            prompt_id: None,
            intent: Some(intent),
            mode: None,
            messages: vec![],
            tools: Vec::new(),
            tool_choice: None,
            stop: Vec::new(),
            temperature: AgentSettings::temperature_for_model(model, cx),
            thinking_allowed: false,
        };

        for message in &self.messages {
            let mut request_message = LanguageModelRequestMessage {
                role: message.role,
                content: Vec::new(),
                cache: false,
            };

            for segment in &message.segments {
                match segment {
                    MessageSegment::Text(text) => request_message
                        .content
                        .push(MessageContent::Text(text.clone())),
                    MessageSegment::Thinking { .. } => {}
                    MessageSegment::RedactedThinking(_) => {}
                }
            }

            if request_message.content.is_empty() {
                continue;
            }

            request.messages.push(request_message);
        }

        request.messages.push(LanguageModelRequestMessage {
            role: Role::User,
            content: vec![MessageContent::Text(added_user_message)],
            cache: false,
        });

        request
    }

    /// Insert auto-generated notifications (if any) to the thread
    fn flush_notifications(
        &mut self,
        model: Arc<dyn LanguageModel>,
        intent: CompletionIntent,
        cx: &mut Context<Self>,
    ) {
        match intent {
            CompletionIntent::UserPrompt | CompletionIntent::ToolResults => {
                if let Some(pending_tool_use) = self.attach_tracked_files_state(model, cx) {
                    cx.emit(ThreadEvent::ToolFinished {
                        tool_use_id: pending_tool_use.id.clone(),
                        pending_tool_use: Some(pending_tool_use),
                    });
                }
            }
            CompletionIntent::ThreadSummarization
            | CompletionIntent::ThreadContextSummarization
            | CompletionIntent::CreateFile
            | CompletionIntent::EditFile
            | CompletionIntent::InlineAssist
            | CompletionIntent::TerminalInlineAssist
            | CompletionIntent::GenerateGitCommitMessage => {}
        };
    }

    fn attach_tracked_files_state(
        &mut self,
        model: Arc<dyn LanguageModel>,
        cx: &mut App,
    ) -> Option<PendingToolUse> {
        // Represent notification as a simulated `project_notifications` tool call
        let tool_name = Arc::from("project_notifications");
        let tool = self.tools.read(cx).tool(&tool_name, cx)?;

        if !self.profile.is_tool_enabled(tool.source(), tool.name(), cx) {
            return None;
        }

        if self
            .action_log
            .update(cx, |log, cx| log.unnotified_user_edits(cx).is_none())
        {
            return None;
        }

        let input = serde_json::json!({});
        let request = Arc::new(LanguageModelRequest::default()); // unused
        let window = None;
        let tool_result = tool.run(
            input,
            request,
            self.project.clone(),
            self.action_log.clone(),
            model.clone(),
            window,
            cx,
        );

        let tool_use_id =
            LanguageModelToolUseId::from(format!("project_notifications_{}", self.messages.len()));

        let tool_use = LanguageModelToolUse {
            id: tool_use_id.clone(),
            name: tool_name.clone(),
            raw_input: "{}".to_string(),
            input: serde_json::json!({}),
            is_input_complete: true,
        };

        let tool_output = cx.background_executor().block(tool_result.output);

        // Attach a project_notification tool call to the latest existing
        // Assistant message. We cannot create a new Assistant message
        // because thinking models require a `thinking` block that we
        // cannot mock. We cannot send a notification as a normal
        // (non-tool-use) User message because this distracts Agent
        // too much.
        let tool_message_id = self
            .messages
            .iter()
            .enumerate()
            .rfind(|(_, message)| message.role == Role::Assistant)
            .map(|(_, message)| message.id)?;

        let tool_use_metadata = ToolUseMetadata {
            model: model.clone(),
            thread_id: self.id.clone(),
            prompt_id: self.last_prompt_id.clone(),
        };

        self.tool_use
            .request_tool_use(tool_message_id, tool_use, tool_use_metadata, cx);

        self.tool_use.insert_tool_output(
            tool_use_id,
            tool_name,
            tool_output,
            self.configured_model.as_ref(),
            self.completion_mode,
        )
    }

    pub fn stream_completion(
        &mut self,
        request: LanguageModelRequest,
        model: Arc<dyn LanguageModel>,
        intent: CompletionIntent,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        self.tool_use_limit_reached = false;

        let pending_completion_id = post_inc(&mut self.completion_count);
        let mut request_callback_parameters = if self.request_callback.is_some() {
            Some((request.clone(), Vec::new()))
        } else {
            None
        };
        let prompt_id = self.last_prompt_id.clone();
        let tool_use_metadata = ToolUseMetadata {
            model: model.clone(),
            thread_id: self.id.clone(),
            prompt_id: prompt_id.clone(),
        };

        let completion_mode = request
            .mode
            .unwrap_or(cloud_llm_client::CompletionMode::Normal);

        self.last_received_chunk_at = Some(Instant::now());

        let task = cx.spawn(async move |thread, cx| {
            let stream_completion_future = model.stream_completion(request, cx);
            let initial_token_usage =
                thread.read_with(cx, |thread, _cx| thread.cumulative_token_usage);
            let stream_completion = async {
                let mut events = stream_completion_future.await?;

                let mut stop_reason = StopReason::EndTurn;
                let mut current_token_usage = TokenUsage::default();

                thread
                    .update(cx, |_thread, cx| {
                        cx.emit(ThreadEvent::NewRequest);
                    })
                    .ok();

                let mut request_assistant_message_id = None;

                while let Some(event) = events.next().await {
                    if let Some((_, response_events)) = request_callback_parameters.as_mut() {
                        response_events
                            .push(event.as_ref().map_err(|error| error.to_string()).cloned());
                    }

                    thread.update(cx, |thread, cx| {
                        match event? {
                            LanguageModelCompletionEvent::StartMessage { .. } => {
                                request_assistant_message_id =
                                    Some(thread.insert_assistant_message(
                                        vec![MessageSegment::Text(String::new())],
                                        cx,
                                    ));
                            }
                            LanguageModelCompletionEvent::Stop(reason) => {
                                stop_reason = reason;
                            }
                            LanguageModelCompletionEvent::UsageUpdate(token_usage) => {
                                thread.update_token_usage_at_last_message(token_usage);
                                thread.cumulative_token_usage = thread.cumulative_token_usage
                                    + token_usage
                                    - current_token_usage;
                                current_token_usage = token_usage;
                            }
                            LanguageModelCompletionEvent::Text(chunk) => {
                                thread.received_chunk();

                                cx.emit(ThreadEvent::ReceivedTextChunk);
                                if let Some(last_message) = thread.messages.last_mut() {
                                    if last_message.role == Role::Assistant
                                        && !thread.tool_use.has_tool_results(last_message.id)
                                    {
                                        last_message.push_text(&chunk);
                                        cx.emit(ThreadEvent::StreamedAssistantText(
                                            last_message.id,
                                            chunk,
                                        ));
                                    } else {
                                        // If we won't have an Assistant message yet, assume this chunk marks the beginning
                                        // of a new Assistant response.
                                        //
                                        // Importantly: We do *not* want to emit a `StreamedAssistantText` event here, as it
                                        // will result in duplicating the text of the chunk in the rendered Markdown.
                                        request_assistant_message_id =
                                            Some(thread.insert_assistant_message(
                                                vec![MessageSegment::Text(chunk.to_string())],
                                                cx,
                                            ));
                                    };
                                }
                            }
                            LanguageModelCompletionEvent::Thinking {
                                text: chunk,
                                signature,
                            } => {
                                thread.received_chunk();

                                if let Some(last_message) = thread.messages.last_mut() {
                                    if last_message.role == Role::Assistant
                                        && !thread.tool_use.has_tool_results(last_message.id)
                                    {
                                        last_message.push_thinking(&chunk, signature);
                                        cx.emit(ThreadEvent::StreamedAssistantThinking(
                                            last_message.id,
                                            chunk,
                                        ));
                                    } else {
                                        // If we won't have an Assistant message yet, assume this chunk marks the beginning
                                        // of a new Assistant response.
                                        //
                                        // Importantly: We do *not* want to emit a `StreamedAssistantText` event here, as it
                                        // will result in duplicating the text of the chunk in the rendered Markdown.
                                        request_assistant_message_id =
                                            Some(thread.insert_assistant_message(
                                                vec![MessageSegment::Thinking {
                                                    text: chunk.to_string(),
                                                    signature,
                                                }],
                                                cx,
                                            ));
                                    };
                                }
                            }
                            LanguageModelCompletionEvent::RedactedThinking { data } => {
                                thread.received_chunk();

                                if let Some(last_message) = thread.messages.last_mut() {
                                    if last_message.role == Role::Assistant
                                        && !thread.tool_use.has_tool_results(last_message.id)
                                    {
                                        last_message.push_redacted_thinking(data);
                                    } else {
                                        request_assistant_message_id =
                                            Some(thread.insert_assistant_message(
                                                vec![MessageSegment::RedactedThinking(data)],
                                                cx,
                                            ));
                                    };
                                }
                            }
                            LanguageModelCompletionEvent::ToolUse(tool_use) => {
                                let last_assistant_message_id = request_assistant_message_id
                                    .unwrap_or_else(|| {
                                        let new_assistant_message_id =
                                            thread.insert_assistant_message(vec![], cx);
                                        request_assistant_message_id =
                                            Some(new_assistant_message_id);
                                        new_assistant_message_id
                                    });

                                let tool_use_id = tool_use.id.clone();
                                let streamed_input = if tool_use.is_input_complete {
                                    None
                                } else {
                                    Some(tool_use.input.clone())
                                };

                                let ui_text = thread.tool_use.request_tool_use(
                                    last_assistant_message_id,
                                    tool_use,
                                    tool_use_metadata.clone(),
                                    cx,
                                );

                                if let Some(input) = streamed_input {
                                    cx.emit(ThreadEvent::StreamedToolUse {
                                        tool_use_id,
                                        ui_text,
                                        input,
                                    });
                                }
                            }
                            LanguageModelCompletionEvent::ToolUseJsonParseError {
                                id,
                                tool_name,
                                raw_input: invalid_input_json,
                                json_parse_error,
                            } => {
                                thread.receive_invalid_tool_json(
                                    id,
                                    tool_name,
                                    invalid_input_json,
                                    json_parse_error,
                                    window,
                                    cx,
                                );
                            }
                            LanguageModelCompletionEvent::StatusUpdate(status_update) => {
                                if let Some(completion) = thread
                                    .pending_completions
                                    .iter_mut()
                                    .find(|completion| completion.id == pending_completion_id)
                                {
                                    match status_update {
                                        CompletionRequestStatus::Queued { position } => {
                                            completion.queue_state =
                                                QueueState::Queued { position };
                                        }
                                        CompletionRequestStatus::Started => {
                                            completion.queue_state = QueueState::Started;
                                        }
                                        CompletionRequestStatus::Failed {
                                            code,
                                            message,
                                            request_id: _,
                                            retry_after,
                                        } => {
                                            return Err(
                                                LanguageModelCompletionError::from_cloud_failure(
                                                    model.upstream_provider_name(),
                                                    code,
                                                    message,
                                                    retry_after.map(Duration::from_secs_f64),
                                                ),
                                            );
                                        }
                                        CompletionRequestStatus::UsageUpdated { amount, limit } => {
                                            thread.update_model_request_usage(
                                                amount as u32,
                                                limit,
                                                cx,
                                            );
                                        }
                                        CompletionRequestStatus::ToolUseLimitReached => {
                                            thread.tool_use_limit_reached = true;
                                            cx.emit(ThreadEvent::ToolUseLimitReached);
                                        }
                                    }
                                }
                            }
                        }

                        thread.touch_updated_at();
                        cx.emit(ThreadEvent::StreamedCompletion);
                        cx.notify();

                        Ok(())
                    })??;

                    smol::future::yield_now().await;
                }

                thread.update(cx, |thread, cx| {
                    thread.last_received_chunk_at = None;
                    thread
                        .pending_completions
                        .retain(|completion| completion.id != pending_completion_id);

                    // If there is a response without tool use, summarize the message. Otherwise,
                    // allow two tool uses before summarizing.
                    if matches!(thread.summary, ThreadSummary::Pending)
                        && thread.messages.len() >= 2
                        && (!thread.has_pending_tool_uses() || thread.messages.len() >= 6)
                    {
                        thread.summarize(cx);
                    }
                })?;

                anyhow::Ok(stop_reason)
            };

            let result = stream_completion.await;
            let mut retry_scheduled = false;

            thread
                .update(cx, |thread, cx| {
                    thread.finalize_pending_checkpoint(cx);
                    match result.as_ref() {
                        Ok(stop_reason) => {
                            match stop_reason {
                                StopReason::ToolUse => {
                                    let tool_uses =
                                        thread.use_pending_tools(window, model.clone(), cx);
                                    cx.emit(ThreadEvent::UsePendingTools { tool_uses });
                                }
                                StopReason::EndTurn | StopReason::MaxTokens => {
                                    thread.project.update(cx, |project, cx| {
                                        project.set_agent_location(None, cx);
                                    });
                                }
                                StopReason::Refusal => {
                                    thread.project.update(cx, |project, cx| {
                                        project.set_agent_location(None, cx);
                                    });

                                    // Remove the turn that was refused.
                                    //
                                    // https://docs.anthropic.com/en/docs/test-and-evaluate/strengthen-guardrails/handle-streaming-refusals#reset-context-after-refusal
                                    {
                                        let mut messages_to_remove = Vec::new();

                                        for (ix, message) in
                                            thread.messages.iter().enumerate().rev()
                                        {
                                            messages_to_remove.push(message.id);

                                            if message.role == Role::User {
                                                if ix == 0 {
                                                    break;
                                                }

                                                if let Some(prev_message) =
                                                    thread.messages.get(ix - 1)
                                                    && prev_message.role == Role::Assistant {
                                                        break;
                                                    }
                                            }
                                        }

                                        for message_id in messages_to_remove {
                                            thread.delete_message(message_id, cx);
                                        }
                                    }

                                    cx.emit(ThreadEvent::ShowError(ThreadError::Message {
                                        header: "Language model refusal".into(),
                                        message:
                                            "Model refused to generate content for safety reasons."
                                                .into(),
                                    }));
                                }
                            }

                            // We successfully completed, so cancel any remaining retries.
                            thread.retry_state = None;
                        }
                        Err(error) => {
                            thread.project.update(cx, |project, cx| {
                                project.set_agent_location(None, cx);
                            });

                            if error.is::<PaymentRequiredError>() {
                                cx.emit(ThreadEvent::ShowError(ThreadError::PaymentRequired));
                            } else if let Some(error) =
                                error.downcast_ref::<ModelRequestLimitReachedError>()
                            {
                                cx.emit(ThreadEvent::ShowError(
                                    ThreadError::ModelRequestLimitReached { plan: error.plan },
                                ));
                            } else if let Some(completion_error) =
                                error.downcast_ref::<LanguageModelCompletionError>()
                            {
                                match &completion_error {
                                    LanguageModelCompletionError::PromptTooLarge {
                                        tokens, ..
                                    } => {
                                        let tokens = tokens.unwrap_or_else(|| {
                                            // We didn't get an exact token count from the API, so fall back on our estimate.
                                            thread
                                                .total_token_usage()
                                                .map(|usage| usage.total)
                                                .unwrap_or(0)
                                                // We know the context window was exceeded in practice, so if our estimate was
                                                // lower than max tokens, the estimate was wrong; return that we exceeded by 1.
                                                .max(
                                                    model
                                                        .max_token_count_for_mode(completion_mode)
                                                        .saturating_add(1),
                                                )
                                        });
                                        thread.exceeded_window_error = Some(ExceededWindowError {
                                            model_id: model.id(),
                                            token_count: tokens,
                                        });
                                        cx.notify();
                                    }
                                    _ => {
                                        if let Some(retry_strategy) =
                                            Thread::get_retry_strategy(completion_error)
                                        {
                                            log::info!(
                                                "Retrying with {:?} for language model completion error {:?}",
                                                retry_strategy,
                                                completion_error
                                            );

                                            retry_scheduled = thread
                                                .handle_retryable_error_with_delay(
                                                    completion_error,
                                                    Some(retry_strategy),
                                                    model.clone(),
                                                    intent,
                                                    window,
                                                    cx,
                                                );
                                        }
                                    }
                                }
                            }

                            if !retry_scheduled {
                                thread.cancel_last_completion(window, cx);
                            }
                        }
                    }

                    if !retry_scheduled {
                        cx.emit(ThreadEvent::Stopped(result.map_err(Arc::new)));
                    }

                    if let Some((request_callback, (request, response_events))) = thread
                        .request_callback
                        .as_mut()
                        .zip(request_callback_parameters.as_ref())
                    {
                        request_callback(request, response_events);
                    }

                    if let Ok(initial_usage) = initial_token_usage {
                        let usage = thread.cumulative_token_usage - initial_usage;

                        telemetry::event!(
                            "Assistant Thread Completion",
                            thread_id = thread.id().to_string(),
                            prompt_id = prompt_id,
                            model = model.telemetry_id(),
                            model_provider = model.provider_id().to_string(),
                            input_tokens = usage.input_tokens,
                            output_tokens = usage.output_tokens,
                            cache_creation_input_tokens = usage.cache_creation_input_tokens,
                            cache_read_input_tokens = usage.cache_read_input_tokens,
                        );
                    }
                })
                .ok();
        });

        self.pending_completions.push(PendingCompletion {
            id: pending_completion_id,
            queue_state: QueueState::Sending,
            _task: task,
        });
    }

    pub fn summarize(&mut self, cx: &mut Context<Self>) {
        let Some(model) = LanguageModelRegistry::read_global(cx).thread_summary_model() else {
            println!("No thread summary model");
            return;
        };

        if !model.provider.is_authenticated(cx) {
            return;
        }

        let request = self.to_summarize_request(
            &model.model,
            CompletionIntent::ThreadSummarization,
            SUMMARIZE_THREAD_PROMPT.into(),
            cx,
        );

        self.summary = ThreadSummary::Generating;

        self.pending_summary = cx.spawn(async move |this, cx| {
            let result = async {
                let mut messages = model.model.stream_completion(request, cx).await?;

                let mut new_summary = String::new();
                while let Some(event) = messages.next().await {
                    let Ok(event) = event else {
                        continue;
                    };
                    let text = match event {
                        LanguageModelCompletionEvent::Text(text) => text,
                        LanguageModelCompletionEvent::StatusUpdate(
                            CompletionRequestStatus::UsageUpdated { amount, limit },
                        ) => {
                            this.update(cx, |thread, cx| {
                                thread.update_model_request_usage(amount as u32, limit, cx);
                            })?;
                            continue;
                        }
                        _ => continue,
                    };

                    let mut lines = text.lines();
                    new_summary.extend(lines.next());

                    // Stop if the LLM generated multiple lines.
                    if lines.next().is_some() {
                        break;
                    }
                }

                anyhow::Ok(new_summary)
            }
            .await;

            this.update(cx, |this, cx| {
                match result {
                    Ok(new_summary) => {
                        if new_summary.is_empty() {
                            this.summary = ThreadSummary::Error;
                        } else {
                            this.summary = ThreadSummary::Ready(new_summary.into());
                        }
                    }
                    Err(err) => {
                        this.summary = ThreadSummary::Error;
                        log::error!("Failed to generate thread summary: {}", err);
                    }
                }
                cx.emit(ThreadEvent::SummaryGenerated);
            })
            .log_err()?;

            Some(())
        });
    }

    fn get_retry_strategy(error: &LanguageModelCompletionError) -> Option<RetryStrategy> {
        use LanguageModelCompletionError::*;

        // General strategy here:
        // - If retrying won't help (e.g. invalid API key or payload too large), return None so we don't retry at all.
        // - If it's a time-based issue (e.g. server overloaded, rate limit exceeded), retry up to 4 times with exponential backoff.
        // - If it's an issue that *might* be fixed by retrying (e.g. internal server error), retry up to 3 times.
        match error {
            HttpResponseError {
                status_code: StatusCode::TOO_MANY_REQUESTS,
                ..
            } => Some(RetryStrategy::ExponentialBackoff {
                initial_delay: BASE_RETRY_DELAY,
                max_attempts: MAX_RETRY_ATTEMPTS,
            }),
            ServerOverloaded { retry_after, .. } | RateLimitExceeded { retry_after, .. } => {
                Some(RetryStrategy::Fixed {
                    delay: retry_after.unwrap_or(BASE_RETRY_DELAY),
                    max_attempts: MAX_RETRY_ATTEMPTS,
                })
            }
            UpstreamProviderError {
                status,
                retry_after,
                ..
            } => match *status {
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                    Some(RetryStrategy::Fixed {
                        delay: retry_after.unwrap_or(BASE_RETRY_DELAY),
                        max_attempts: MAX_RETRY_ATTEMPTS,
                    })
                }
                StatusCode::INTERNAL_SERVER_ERROR => Some(RetryStrategy::Fixed {
                    delay: retry_after.unwrap_or(BASE_RETRY_DELAY),
                    // Internal Server Error could be anything, retry up to 3 times.
                    max_attempts: 3,
                }),
                status => {
                    // There is no StatusCode variant for the unofficial HTTP 529 ("The service is overloaded"),
                    // but we frequently get them in practice. See https://http.dev/529
                    if status.as_u16() == 529 {
                        Some(RetryStrategy::Fixed {
                            delay: retry_after.unwrap_or(BASE_RETRY_DELAY),
                            max_attempts: MAX_RETRY_ATTEMPTS,
                        })
                    } else {
                        Some(RetryStrategy::Fixed {
                            delay: retry_after.unwrap_or(BASE_RETRY_DELAY),
                            max_attempts: 2,
                        })
                    }
                }
            },
            ApiInternalServerError { .. } => Some(RetryStrategy::Fixed {
                delay: BASE_RETRY_DELAY,
                max_attempts: 3,
            }),
            ApiReadResponseError { .. }
            | HttpSend { .. }
            | DeserializeResponse { .. }
            | BadRequestFormat { .. } => Some(RetryStrategy::Fixed {
                delay: BASE_RETRY_DELAY,
                max_attempts: 3,
            }),
            // Retrying these errors definitely shouldn't help.
            HttpResponseError {
                status_code:
                    StatusCode::PAYLOAD_TOO_LARGE | StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED,
                ..
            }
            | AuthenticationError { .. }
            | PermissionError { .. }
            | NoApiKey { .. }
            | ApiEndpointNotFound { .. }
            | PromptTooLarge { .. } => None,
            // These errors might be transient, so retry them
            SerializeRequest { .. } | BuildRequestBody { .. } => Some(RetryStrategy::Fixed {
                delay: BASE_RETRY_DELAY,
                max_attempts: 1,
            }),
            // Retry all other 4xx and 5xx errors once.
            HttpResponseError { status_code, .. }
                if status_code.is_client_error() || status_code.is_server_error() =>
            {
                Some(RetryStrategy::Fixed {
                    delay: BASE_RETRY_DELAY,
                    max_attempts: 3,
                })
            }
            Other(err)
                if err.is::<PaymentRequiredError>()
                    || err.is::<ModelRequestLimitReachedError>() =>
            {
                // Retrying won't help for Payment Required or Model Request Limit errors (where
                // the user must upgrade to usage-based billing to get more requests, or else wait
                // for a significant amount of time for the request limit to reset).
                None
            }
            // Conservatively assume that any other errors are non-retryable
            HttpResponseError { .. } | Other(..) => Some(RetryStrategy::Fixed {
                delay: BASE_RETRY_DELAY,
                max_attempts: 2,
            }),
        }
    }

    fn handle_retryable_error_with_delay(
        &mut self,
        error: &LanguageModelCompletionError,
        strategy: Option<RetryStrategy>,
        model: Arc<dyn LanguageModel>,
        intent: CompletionIntent,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) -> bool {
        // Store context for the Retry button
        self.last_error_context = Some((model.clone(), intent));

        // Only auto-retry if Burn Mode is enabled
        if self.completion_mode != CompletionMode::Burn {
            // Show error with retry options
            cx.emit(ThreadEvent::ShowError(ThreadError::RetryableError {
                message: format!(
                    "{}\n\nTo automatically retry when similar errors happen, enable Burn Mode.",
                    error
                )
                .into(),
                can_enable_burn_mode: true,
            }));
            return false;
        }

        let Some(strategy) = strategy.or_else(|| Self::get_retry_strategy(error)) else {
            return false;
        };

        let max_attempts = match &strategy {
            RetryStrategy::ExponentialBackoff { max_attempts, .. } => *max_attempts,
            RetryStrategy::Fixed { max_attempts, .. } => *max_attempts,
        };

        let retry_state = self.retry_state.get_or_insert(RetryState {
            attempt: 0,
            max_attempts,
            intent,
        });

        retry_state.attempt += 1;
        let attempt = retry_state.attempt;
        let max_attempts = retry_state.max_attempts;
        let intent = retry_state.intent;

        if attempt <= max_attempts {
            let delay = match &strategy {
                RetryStrategy::ExponentialBackoff { initial_delay, .. } => {
                    let delay_secs = initial_delay.as_secs() * 2u64.pow((attempt - 1) as u32);
                    Duration::from_secs(delay_secs)
                }
                RetryStrategy::Fixed { delay, .. } => *delay,
            };

            // Add a transient message to inform the user
            let delay_secs = delay.as_secs();
            let retry_message = if max_attempts == 1 {
                format!("{error}. Retrying in {delay_secs} seconds...")
            } else {
                format!(
                    "{error}. Retrying (attempt {attempt} of {max_attempts}) \
                    in {delay_secs} seconds..."
                )
            };
            log::warn!(
                "Retrying completion request (attempt {attempt} of {max_attempts}) \
                in {delay_secs} seconds: {error:?}",
            );

            // Add a UI-only message instead of a regular message
            let id = self.next_message_id.post_inc();
            self.messages.push(Message {
                id,
                role: Role::System,
                segments: vec![MessageSegment::Text(retry_message)],
                loaded_context: LoadedContext::default(),
                creases: Vec::new(),
                is_hidden: false,
                ui_only: true,
            });
            cx.emit(ThreadEvent::MessageAdded(id));

            // Schedule the retry
            let thread_handle = cx.entity().downgrade();

            cx.spawn(async move |_thread, cx| {
                cx.background_executor().timer(delay).await;

                thread_handle
                    .update(cx, |thread, cx| {
                        // Retry the completion
                        thread.send_to_model(model, intent, window, cx);
                    })
                    .log_err();
            })
            .detach();

            true
        } else {
            // Max retries exceeded
            self.retry_state = None;

            // Stop generating since we're giving up on retrying.
            self.pending_completions.clear();

            // Show error alongside a Retry button, but no
            // Enable Burn Mode button (since it's already enabled)
            cx.emit(ThreadEvent::ShowError(ThreadError::RetryableError {
                message: format!("Failed after retrying: {}", error).into(),
                can_enable_burn_mode: false,
            }));

            false
        }
    }

    pub fn start_generating_detailed_summary_if_needed(
        &mut self,
        thread_store: WeakEntity<ThreadStore>,
        cx: &mut Context<Self>,
    ) {
        let Some(last_message_id) = self.messages.last().map(|message| message.id) else {
            return;
        };

        match &*self.detailed_summary_rx.borrow() {
            DetailedSummaryState::Generating { message_id, .. }
            | DetailedSummaryState::Generated { message_id, .. }
                if *message_id == last_message_id =>
            {
                // Already up-to-date
                return;
            }
            _ => {}
        }

        let Some(ConfiguredModel { model, provider }) =
            LanguageModelRegistry::read_global(cx).thread_summary_model()
        else {
            return;
        };

        if !provider.is_authenticated(cx) {
            return;
        }

        let request = self.to_summarize_request(
            &model,
            CompletionIntent::ThreadContextSummarization,
            SUMMARIZE_THREAD_DETAILED_PROMPT.into(),
            cx,
        );

        *self.detailed_summary_tx.borrow_mut() = DetailedSummaryState::Generating {
            message_id: last_message_id,
        };

        // Replace the detailed summarization task if there is one, cancelling it. It would probably
        // be better to allow the old task to complete, but this would require logic for choosing
        // which result to prefer (the old task could complete after the new one, resulting in a
        // stale summary).
        self.detailed_summary_task = cx.spawn(async move |thread, cx| {
            let stream = model.stream_completion_text(request, cx);
            let Some(mut messages) = stream.await.log_err() else {
                thread
                    .update(cx, |thread, _cx| {
                        *thread.detailed_summary_tx.borrow_mut() =
                            DetailedSummaryState::NotGenerated;
                    })
                    .ok()?;
                return None;
            };

            let mut new_detailed_summary = String::new();

            while let Some(chunk) = messages.stream.next().await {
                if let Some(chunk) = chunk.log_err() {
                    new_detailed_summary.push_str(&chunk);
                }
            }

            thread
                .update(cx, |thread, _cx| {
                    *thread.detailed_summary_tx.borrow_mut() = DetailedSummaryState::Generated {
                        text: new_detailed_summary.into(),
                        message_id: last_message_id,
                    };
                })
                .ok()?;

            // Save thread so its summary can be reused later
            if let Some(thread) = thread.upgrade()
                && let Ok(Ok(save_task)) = cx.update(|cx| {
                    thread_store
                        .update(cx, |thread_store, cx| thread_store.save_thread(&thread, cx))
                })
            {
                save_task.await.log_err();
            }

            Some(())
        });
    }

    pub async fn wait_for_detailed_summary_or_text(
        this: &Entity<Self>,
        cx: &mut AsyncApp,
    ) -> Option<SharedString> {
        let mut detailed_summary_rx = this
            .read_with(cx, |this, _cx| this.detailed_summary_rx.clone())
            .ok()?;
        loop {
            match detailed_summary_rx.recv().await? {
                DetailedSummaryState::Generating { .. } => {}
                DetailedSummaryState::NotGenerated => {
                    return this.read_with(cx, |this, _cx| this.text().into()).ok();
                }
                DetailedSummaryState::Generated { text, .. } => return Some(text),
            }
        }
    }

    pub fn latest_detailed_summary_or_text(&self) -> SharedString {
        self.detailed_summary_rx
            .borrow()
            .text()
            .unwrap_or_else(|| self.text().into())
    }

    pub fn is_generating_detailed_summary(&self) -> bool {
        matches!(
            &*self.detailed_summary_rx.borrow(),
            DetailedSummaryState::Generating { .. }
        )
    }

    pub fn use_pending_tools(
        &mut self,
        window: Option<AnyWindowHandle>,
        model: Arc<dyn LanguageModel>,
        cx: &mut Context<Self>,
    ) -> Vec<PendingToolUse> {
        let request =
            Arc::new(self.to_completion_request(model.clone(), CompletionIntent::ToolResults, cx));
        let pending_tool_uses = self
            .tool_use
            .pending_tool_uses()
            .into_iter()
            .filter(|tool_use| tool_use.status.is_idle())
            .cloned()
            .collect::<Vec<_>>();

        for tool_use in pending_tool_uses.iter() {
            self.use_pending_tool(tool_use.clone(), request.clone(), model.clone(), window, cx);
        }

        pending_tool_uses
    }

    fn use_pending_tool(
        &mut self,
        tool_use: PendingToolUse,
        request: Arc<LanguageModelRequest>,
        model: Arc<dyn LanguageModel>,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let Some(tool) = self.tools.read(cx).tool(&tool_use.name, cx) else {
            return self.handle_hallucinated_tool_use(tool_use.id, tool_use.name, window, cx);
        };

        if !self.profile.is_tool_enabled(tool.source(), tool.name(), cx) {
            return self.handle_hallucinated_tool_use(tool_use.id, tool_use.name, window, cx);
        }

        if tool.needs_confirmation(&tool_use.input, &self.project, cx)
            && !AgentSettings::get_global(cx).always_allow_tool_actions
        {
            self.tool_use.confirm_tool_use(
                tool_use.id,
                tool_use.ui_text,
                tool_use.input,
                request,
                tool,
            );
            cx.emit(ThreadEvent::ToolConfirmationNeeded);
        } else {
            self.run_tool(
                tool_use.id,
                tool_use.ui_text,
                tool_use.input,
                request,
                tool,
                model,
                window,
                cx,
            );
        }
    }

    pub fn handle_hallucinated_tool_use(
        &mut self,
        tool_use_id: LanguageModelToolUseId,
        hallucinated_tool_name: Arc<str>,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Thread>,
    ) {
        let available_tools = self.profile.enabled_tools(cx);

        let tool_list = available_tools
            .iter()
            .map(|(name, tool)| format!("- {}: {}", name, tool.description()))
            .collect::<Vec<_>>()
            .join("\n");

        let error_message = format!(
            "The tool '{}' doesn't exist or is not enabled. Available tools:\n{}",
            hallucinated_tool_name, tool_list
        );

        let pending_tool_use = self.tool_use.insert_tool_output(
            tool_use_id.clone(),
            hallucinated_tool_name,
            Err(anyhow!("Missing tool call: {error_message}")),
            self.configured_model.as_ref(),
            self.completion_mode,
        );

        cx.emit(ThreadEvent::MissingToolUse {
            tool_use_id: tool_use_id.clone(),
            ui_text: error_message.into(),
        });

        self.tool_finished(tool_use_id, pending_tool_use, false, window, cx);
    }

    pub fn receive_invalid_tool_json(
        &mut self,
        tool_use_id: LanguageModelToolUseId,
        tool_name: Arc<str>,
        invalid_json: Arc<str>,
        error: String,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Thread>,
    ) {
        log::error!("The model returned invalid input JSON: {invalid_json}");

        let pending_tool_use = self.tool_use.insert_tool_output(
            tool_use_id.clone(),
            tool_name,
            Err(anyhow!("Error parsing input JSON: {error}")),
            self.configured_model.as_ref(),
            self.completion_mode,
        );
        let ui_text = if let Some(pending_tool_use) = &pending_tool_use {
            pending_tool_use.ui_text.clone()
        } else {
            log::error!(
                "There was no pending tool use for tool use {tool_use_id}, even though it finished (with invalid input JSON)."
            );
            format!("Unknown tool {}", tool_use_id).into()
        };

        cx.emit(ThreadEvent::InvalidToolInput {
            tool_use_id: tool_use_id.clone(),
            ui_text,
            invalid_input_json: invalid_json,
        });

        self.tool_finished(tool_use_id, pending_tool_use, false, window, cx);
    }

    pub fn run_tool(
        &mut self,
        tool_use_id: LanguageModelToolUseId,
        ui_text: impl Into<SharedString>,
        input: serde_json::Value,
        request: Arc<LanguageModelRequest>,
        tool: Arc<dyn Tool>,
        model: Arc<dyn LanguageModel>,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Thread>,
    ) {
        let task =
            self.spawn_tool_use(tool_use_id.clone(), request, input, tool, model, window, cx);
        self.tool_use
            .run_pending_tool(tool_use_id, ui_text.into(), task);
    }

    fn spawn_tool_use(
        &mut self,
        tool_use_id: LanguageModelToolUseId,
        request: Arc<LanguageModelRequest>,
        input: serde_json::Value,
        tool: Arc<dyn Tool>,
        model: Arc<dyn LanguageModel>,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Thread>,
    ) -> Task<()> {
        let tool_name: Arc<str> = tool.name().into();

        let tool_result = tool.run(
            input,
            request,
            self.project.clone(),
            self.action_log.clone(),
            model,
            window,
            cx,
        );

        // Store the card separately if it exists
        if let Some(card) = tool_result.card.clone() {
            self.tool_use
                .insert_tool_result_card(tool_use_id.clone(), card);
        }

        cx.spawn({
            async move |thread: WeakEntity<Thread>, cx| {
                let output = tool_result.output.await;

                thread
                    .update(cx, |thread, cx| {
                        let pending_tool_use = thread.tool_use.insert_tool_output(
                            tool_use_id.clone(),
                            tool_name,
                            output,
                            thread.configured_model.as_ref(),
                            thread.completion_mode,
                        );
                        thread.tool_finished(tool_use_id, pending_tool_use, false, window, cx);
                    })
                    .ok();
            }
        })
    }

    fn tool_finished(
        &mut self,
        tool_use_id: LanguageModelToolUseId,
        pending_tool_use: Option<PendingToolUse>,
        canceled: bool,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        if self.all_tools_finished()
            && let Some(ConfiguredModel { model, .. }) = self.configured_model.as_ref()
            && !canceled
        {
            self.send_to_model(model.clone(), CompletionIntent::ToolResults, window, cx);
        }

        cx.emit(ThreadEvent::ToolFinished {
            tool_use_id,
            pending_tool_use,
        });
    }

    /// Cancels the last pending completion, if there are any pending.
    ///
    /// Returns whether a completion was canceled.
    pub fn cancel_last_completion(
        &mut self,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut canceled = self.pending_completions.pop().is_some() || self.retry_state.is_some();

        self.retry_state = None;

        for pending_tool_use in self.tool_use.cancel_pending() {
            canceled = true;
            self.tool_finished(
                pending_tool_use.id.clone(),
                Some(pending_tool_use),
                true,
                window,
                cx,
            );
        }

        if canceled {
            cx.emit(ThreadEvent::CompletionCanceled);

            // When canceled, we always want to insert the checkpoint.
            // (We skip over finalize_pending_checkpoint, because it
            // would conclude we didn't have anything to insert here.)
            if let Some(checkpoint) = self.pending_checkpoint.take() {
                self.insert_checkpoint(checkpoint, cx);
            }
        } else {
            self.finalize_pending_checkpoint(cx);
        }

        canceled
    }

    /// Signals that any in-progress editing should be canceled.
    ///
    /// This method is used to notify listeners (like ActiveThread) that
    /// they should cancel any editing operations.
    pub fn cancel_editing(&mut self, cx: &mut Context<Self>) {
        cx.emit(ThreadEvent::CancelEditing);
    }

    pub fn message_feedback(&self, message_id: MessageId) -> Option<ThreadFeedback> {
        self.message_feedback.get(&message_id).copied()
    }

    pub fn report_message_feedback(
        &mut self,
        message_id: MessageId,
        feedback: ThreadFeedback,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        if self.message_feedback.get(&message_id) == Some(&feedback) {
            return Task::ready(Ok(()));
        }

        let final_project_snapshot = Self::project_snapshot(self.project.clone(), cx);
        let serialized_thread = self.serialize(cx);
        let thread_id = self.id().clone();
        let client = self.project.read(cx).client();

        let enabled_tool_names: Vec<String> = self
            .profile
            .enabled_tools(cx)
            .iter()
            .map(|(name, _)| name.clone().into())
            .collect();

        self.message_feedback.insert(message_id, feedback);

        cx.notify();

        let message_content = self
            .message(message_id)
            .map(|msg| msg.to_message_content())
            .unwrap_or_default();

        cx.background_spawn(async move {
            let final_project_snapshot = final_project_snapshot.await;
            let serialized_thread = serialized_thread.await?;
            let thread_data =
                serde_json::to_value(serialized_thread).unwrap_or_else(|_| serde_json::Value::Null);

            let rating = match feedback {
                ThreadFeedback::Positive => "positive",
                ThreadFeedback::Negative => "negative",
            };
            telemetry::event!(
                "Assistant Thread Rated",
                rating,
                thread_id,
                enabled_tool_names,
                message_id = message_id.0,
                message_content,
                thread_data,
                final_project_snapshot
            );
            client.telemetry().flush_events().await;

            Ok(())
        })
    }

    /// Create a snapshot of the current project state including git information and unsaved buffers.
    fn project_snapshot(
        project: Entity<Project>,
        cx: &mut Context<Self>,
    ) -> Task<Arc<ProjectSnapshot>> {
        let git_store = project.read(cx).git_store().clone();
        let worktree_snapshots: Vec<_> = project
            .read(cx)
            .visible_worktrees(cx)
            .map(|worktree| Self::worktree_snapshot(worktree, git_store.clone(), cx))
            .collect();

        cx.spawn(async move |_, cx| {
            let worktree_snapshots = futures::future::join_all(worktree_snapshots).await;

            let mut unsaved_buffers = Vec::new();
            cx.update(|app_cx| {
                let buffer_store = project.read(app_cx).buffer_store();
                for buffer_handle in buffer_store.read(app_cx).buffers() {
                    let buffer = buffer_handle.read(app_cx);
                    if buffer.is_dirty()
                        && let Some(file) = buffer.file()
                    {
                        let path = file.path().to_string_lossy().to_string();
                        unsaved_buffers.push(path);
                    }
                }
            })
            .ok();

            Arc::new(ProjectSnapshot {
                worktree_snapshots,
                unsaved_buffer_paths: unsaved_buffers,
                timestamp: Utc::now(),
            })
        })
    }

    fn worktree_snapshot(
        worktree: Entity<project::Worktree>,
        git_store: Entity<GitStore>,
        cx: &App,
    ) -> Task<WorktreeSnapshot> {
        cx.spawn(async move |cx| {
            // Get worktree path and snapshot
            let worktree_info = cx.update(|app_cx| {
                let worktree = worktree.read(app_cx);
                let path = worktree.abs_path().to_string_lossy().to_string();
                let snapshot = worktree.snapshot();
                (path, snapshot)
            });

            let Ok((worktree_path, _snapshot)) = worktree_info else {
                return WorktreeSnapshot {
                    worktree_path: String::new(),
                    git_state: None,
                };
            };

            let git_state = git_store
                .update(cx, |git_store, cx| {
                    git_store
                        .repositories()
                        .values()
                        .find(|repo| {
                            repo.read(cx)
                                .abs_path_to_repo_path(&worktree.read(cx).abs_path())
                                .is_some()
                        })
                        .cloned()
                })
                .ok()
                .flatten()
                .map(|repo| {
                    repo.update(cx, |repo, _| {
                        let current_branch =
                            repo.branch.as_ref().map(|branch| branch.name().to_owned());
                        repo.send_job(None, |state, _| async move {
                            let RepositoryState::Local { backend, .. } = state else {
                                return GitState {
                                    remote_url: None,
                                    head_sha: None,
                                    current_branch,
                                    diff: None,
                                };
                            };

                            let remote_url = backend.remote_url("origin");
                            let head_sha = backend.head_sha().await;
                            let diff = backend.diff(DiffType::HeadToWorktree).await.ok();

                            GitState {
                                remote_url,
                                head_sha,
                                current_branch,
                                diff,
                            }
                        })
                    })
                });

            let git_state = match git_state {
                Some(git_state) => match git_state.ok() {
                    Some(git_state) => git_state.await.ok(),
                    None => None,
                },
                None => None,
            };

            WorktreeSnapshot {
                worktree_path,
                git_state,
            }
        })
    }

    pub fn to_markdown(&self, cx: &App) -> Result<String> {
        let mut markdown = Vec::new();

        let summary = self.summary().or_default();
        writeln!(markdown, "# {summary}\n")?;

        for message in self.messages() {
            writeln!(
                markdown,
                "## {role}\n",
                role = match message.role {
                    Role::User => "User",
                    Role::Assistant => "Agent",
                    Role::System => "System",
                }
            )?;

            if !message.loaded_context.text.is_empty() {
                writeln!(markdown, "{}", message.loaded_context.text)?;
            }

            if !message.loaded_context.images.is_empty() {
                writeln!(
                    markdown,
                    "\n{} images attached as context.\n",
                    message.loaded_context.images.len()
                )?;
            }

            for segment in &message.segments {
                match segment {
                    MessageSegment::Text(text) => writeln!(markdown, "{}\n", text)?,
                    MessageSegment::Thinking { text, .. } => {
                        writeln!(markdown, "<think>\n{}\n</think>\n", text)?
                    }
                    MessageSegment::RedactedThinking(_) => {}
                }
            }

            for tool_use in self.tool_uses_for_message(message.id, cx) {
                writeln!(
                    markdown,
                    "**Use Tool: {} ({})**",
                    tool_use.name, tool_use.id
                )?;
                writeln!(markdown, "```json")?;
                writeln!(
                    markdown,
                    "{}",
                    serde_json::to_string_pretty(&tool_use.input)?
                )?;
                writeln!(markdown, "```")?;
            }

            for tool_result in self.tool_results_for_message(message.id) {
                write!(markdown, "\n**Tool Results: {}", tool_result.tool_use_id)?;
                if tool_result.is_error {
                    write!(markdown, " (Error)")?;
                }

                writeln!(markdown, "**\n")?;
                match &tool_result.content {
                    LanguageModelToolResultContent::Text(text) => {
                        writeln!(markdown, "{text}")?;
                    }
                    LanguageModelToolResultContent::Image(image) => {
                        writeln!(markdown, "![Image](data:base64,{})", image.source)?;
                    }
                }

                if let Some(output) = tool_result.output.as_ref() {
                    writeln!(
                        markdown,
                        "\n\nDebug Output:\n\n```json\n{}\n```\n",
                        serde_json::to_string_pretty(output)?
                    )?;
                }
            }
        }

        Ok(String::from_utf8_lossy(&markdown).to_string())
    }

    pub fn keep_edits_in_range(
        &mut self,
        buffer: Entity<language::Buffer>,
        buffer_range: Range<language::Anchor>,
        cx: &mut Context<Self>,
    ) {
        self.action_log.update(cx, |action_log, cx| {
            action_log.keep_edits_in_range(buffer, buffer_range, cx)
        });
    }

    pub fn keep_all_edits(&mut self, cx: &mut Context<Self>) {
        self.action_log
            .update(cx, |action_log, cx| action_log.keep_all_edits(cx));
    }

    pub fn reject_edits_in_ranges(
        &mut self,
        buffer: Entity<language::Buffer>,
        buffer_ranges: Vec<Range<language::Anchor>>,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.action_log.update(cx, |action_log, cx| {
            action_log.reject_edits_in_ranges(buffer, buffer_ranges, cx)
        })
    }

    pub fn action_log(&self) -> &Entity<ActionLog> {
        &self.action_log
    }

    pub fn project(&self) -> &Entity<Project> {
        &self.project
    }

    pub fn cumulative_token_usage(&self) -> TokenUsage {
        self.cumulative_token_usage
    }

    pub fn token_usage_up_to_message(&self, message_id: MessageId) -> TotalTokenUsage {
        let Some(model) = self.configured_model.as_ref() else {
            return TotalTokenUsage::default();
        };

        let max = model
            .model
            .max_token_count_for_mode(self.completion_mode().into());

        let index = self
            .messages
            .iter()
            .position(|msg| msg.id == message_id)
            .unwrap_or(0);

        if index == 0 {
            return TotalTokenUsage { total: 0, max };
        }

        let token_usage = &self
            .request_token_usage
            .get(index - 1)
            .cloned()
            .unwrap_or_default();

        TotalTokenUsage {
            total: token_usage.total_tokens(),
            max,
        }
    }

    pub fn total_token_usage(&self) -> Option<TotalTokenUsage> {
        let model = self.configured_model.as_ref()?;

        let max = model
            .model
            .max_token_count_for_mode(self.completion_mode().into());

        if let Some(exceeded_error) = &self.exceeded_window_error
            && model.model.id() == exceeded_error.model_id
        {
            return Some(TotalTokenUsage {
                total: exceeded_error.token_count,
                max,
            });
        }

        let total = self
            .token_usage_at_last_message()
            .unwrap_or_default()
            .total_tokens();

        Some(TotalTokenUsage { total, max })
    }

    fn token_usage_at_last_message(&self) -> Option<TokenUsage> {
        self.request_token_usage
            .get(self.messages.len().saturating_sub(1))
            .or_else(|| self.request_token_usage.last())
            .cloned()
    }

    fn update_token_usage_at_last_message(&mut self, token_usage: TokenUsage) {
        let placeholder = self.token_usage_at_last_message().unwrap_or_default();
        self.request_token_usage
            .resize(self.messages.len(), placeholder);

        if let Some(last) = self.request_token_usage.last_mut() {
            *last = token_usage;
        }
    }

    fn update_model_request_usage(&self, amount: u32, limit: UsageLimit, cx: &mut Context<Self>) {
        self.project
            .read(cx)
            .user_store()
            .update(cx, |user_store, cx| {
                user_store.update_model_request_usage(
                    ModelRequestUsage(RequestUsage {
                        amount: amount as i32,
                        limit,
                    }),
                    cx,
                )
            });
    }

    pub fn deny_tool_use(
        &mut self,
        tool_use_id: LanguageModelToolUseId,
        tool_name: Arc<str>,
        window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let err = Err(anyhow::anyhow!(
            "Permission to run tool action denied by user"
        ));

        self.tool_use.insert_tool_output(
            tool_use_id.clone(),
            tool_name,
            err,
            self.configured_model.as_ref(),
            self.completion_mode,
        );
        self.tool_finished(tool_use_id, None, true, window, cx);
    }
}

#[derive(Debug, Clone, Error)]
pub enum ThreadError {
    #[error("Payment required")]
    PaymentRequired,
    #[error("Model request limit reached")]
    ModelRequestLimitReached { plan: Plan },
    #[error("Message {header}: {message}")]
    Message {
        header: SharedString,
        message: SharedString,
    },
    #[error("Retryable error: {message}")]
    RetryableError {
        message: SharedString,
        can_enable_burn_mode: bool,
    },
}

#[derive(Debug, Clone)]
pub enum ThreadEvent {
    ShowError(ThreadError),
    StreamedCompletion,
    ReceivedTextChunk,
    NewRequest,
    StreamedAssistantText(MessageId, String),
    StreamedAssistantThinking(MessageId, String),
    StreamedToolUse {
        tool_use_id: LanguageModelToolUseId,
        ui_text: Arc<str>,
        input: serde_json::Value,
    },
    MissingToolUse {
        tool_use_id: LanguageModelToolUseId,
        ui_text: Arc<str>,
    },
    InvalidToolInput {
        tool_use_id: LanguageModelToolUseId,
        ui_text: Arc<str>,
        invalid_input_json: Arc<str>,
    },
    Stopped(Result<StopReason, Arc<anyhow::Error>>),
    MessageAdded(MessageId),
    MessageEdited(MessageId),
    MessageDeleted(MessageId),
    SummaryGenerated,
    SummaryChanged,
    UsePendingTools {
        tool_uses: Vec<PendingToolUse>,
    },
    ToolFinished {
        #[allow(unused)]
        tool_use_id: LanguageModelToolUseId,
        /// The pending tool use that corresponds to this tool.
        pending_tool_use: Option<PendingToolUse>,
    },
    CheckpointChanged,
    ToolConfirmationNeeded,
    ToolUseLimitReached,
    CancelEditing,
    CompletionCanceled,
    ProfileChanged,
}

impl EventEmitter<ThreadEvent> for Thread {}

struct PendingCompletion {
    id: usize,
    queue_state: QueueState,
    _task: Task<()>,
}
