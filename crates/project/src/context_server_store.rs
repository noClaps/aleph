pub mod extension;
pub mod registry;

use std::{path::Path, sync::Arc};

use anyhow::{Context as _, Result};
use collections::{HashMap, HashSet};
use context_server::{ContextServer, ContextServerCommand, ContextServerId};
use futures::{FutureExt as _, future::join_all};
use gpui::{App, AsyncApp, Context, Entity, EventEmitter, Subscription, Task, WeakEntity, actions};
use registry::ContextServerDescriptorRegistry;
use settings::{Settings as _, SettingsStore};
use util::ResultExt as _;

use crate::{
    Project,
    project_settings::{ContextServerSettings, ProjectSettings},
    worktree_store::WorktreeStore,
};

pub fn init(cx: &mut App) {
    extension::init(cx);
}

actions!(
    context_server,
    [
        /// Restarts the context server.
        Restart
    ]
);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContextServerStatus {
    Starting,
    Running,
    Stopped,
    Error(Arc<str>),
}

impl ContextServerStatus {
    fn from_state(state: &ContextServerState) -> Self {
        match state {
            ContextServerState::Starting { .. } => ContextServerStatus::Starting,
            ContextServerState::Running { .. } => ContextServerStatus::Running,
            ContextServerState::Stopped { .. } => ContextServerStatus::Stopped,
            ContextServerState::Error { error, .. } => ContextServerStatus::Error(error.clone()),
        }
    }
}

enum ContextServerState {
    Starting {
        server: Arc<ContextServer>,
        configuration: Arc<ContextServerConfiguration>,
        _task: Task<()>,
    },
    Running {
        server: Arc<ContextServer>,
        configuration: Arc<ContextServerConfiguration>,
    },
    Stopped {
        server: Arc<ContextServer>,
        configuration: Arc<ContextServerConfiguration>,
    },
    Error {
        server: Arc<ContextServer>,
        configuration: Arc<ContextServerConfiguration>,
        error: Arc<str>,
    },
}

impl ContextServerState {
    pub fn server(&self) -> Arc<ContextServer> {
        match self {
            ContextServerState::Starting { server, .. } => server.clone(),
            ContextServerState::Running { server, .. } => server.clone(),
            ContextServerState::Stopped { server, .. } => server.clone(),
            ContextServerState::Error { server, .. } => server.clone(),
        }
    }

    pub fn configuration(&self) -> Arc<ContextServerConfiguration> {
        match self {
            ContextServerState::Starting { configuration, .. } => configuration.clone(),
            ContextServerState::Running { configuration, .. } => configuration.clone(),
            ContextServerState::Stopped { configuration, .. } => configuration.clone(),
            ContextServerState::Error { configuration, .. } => configuration.clone(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContextServerConfiguration {
    Custom {
        command: ContextServerCommand,
    },
    Extension {
        command: ContextServerCommand,
        settings: serde_json::Value,
    },
}

impl ContextServerConfiguration {
    pub fn command(&self) -> &ContextServerCommand {
        match self {
            ContextServerConfiguration::Custom { command } => command,
            ContextServerConfiguration::Extension { command, .. } => command,
        }
    }

    pub async fn from_settings(
        settings: ContextServerSettings,
        id: ContextServerId,
        registry: Entity<ContextServerDescriptorRegistry>,
        worktree_store: Entity<WorktreeStore>,
        cx: &AsyncApp,
    ) -> Option<Self> {
        match settings {
            ContextServerSettings::Custom {
                enabled: _,
                command,
            } => Some(ContextServerConfiguration::Custom { command }),
            ContextServerSettings::Extension {
                enabled: _,
                settings,
            } => {
                let descriptor = cx
                    .update(|cx| registry.read(cx).context_server_descriptor(&id.0))
                    .ok()
                    .flatten()?;

                let command = descriptor.command(worktree_store, cx).await.log_err()?;

                Some(ContextServerConfiguration::Extension { command, settings })
            }
        }
    }
}

pub type ContextServerFactory =
    Box<dyn Fn(ContextServerId, Arc<ContextServerConfiguration>) -> Arc<ContextServer>>;

pub struct ContextServerStore {
    context_server_settings: HashMap<Arc<str>, ContextServerSettings>,
    servers: HashMap<ContextServerId, ContextServerState>,
    worktree_store: Entity<WorktreeStore>,
    project: WeakEntity<Project>,
    registry: Entity<ContextServerDescriptorRegistry>,
    update_servers_task: Option<Task<Result<()>>>,
    context_server_factory: Option<ContextServerFactory>,
    needs_server_update: bool,
    _subscriptions: Vec<Subscription>,
}

pub enum Event {
    ServerStatusChanged {
        server_id: ContextServerId,
        status: ContextServerStatus,
    },
}

impl EventEmitter<Event> for ContextServerStore {}

impl ContextServerStore {
    pub fn new(
        worktree_store: Entity<WorktreeStore>,
        weak_project: WeakEntity<Project>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_internal(
            true,
            None,
            ContextServerDescriptorRegistry::default_global(cx),
            worktree_store,
            weak_project,
            cx,
        )
    }

    /// Returns all configured context server ids, regardless of enabled state.
    pub fn configured_server_ids(&self) -> Vec<ContextServerId> {
        self.context_server_settings
            .keys()
            .cloned()
            .map(ContextServerId)
            .collect()
    }

    fn new_internal(
        maintain_server_loop: bool,
        context_server_factory: Option<ContextServerFactory>,
        registry: Entity<ContextServerDescriptorRegistry>,
        worktree_store: Entity<WorktreeStore>,
        weak_project: WeakEntity<Project>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = if maintain_server_loop {
            vec![
                cx.observe(&registry, |this, _registry, cx| {
                    this.available_context_servers_changed(cx);
                }),
                cx.observe_global::<SettingsStore>(|this, cx| {
                    let settings = Self::resolve_context_server_settings(&this.worktree_store, cx);
                    if &this.context_server_settings == settings {
                        return;
                    }
                    this.context_server_settings = settings.clone();
                    this.available_context_servers_changed(cx);
                }),
            ]
        } else {
            Vec::new()
        };

        let mut this = Self {
            _subscriptions: subscriptions,
            context_server_settings: Self::resolve_context_server_settings(&worktree_store, cx)
                .clone(),
            worktree_store,
            project: weak_project,
            registry,
            needs_server_update: false,
            servers: HashMap::default(),
            update_servers_task: None,
            context_server_factory,
        };
        if maintain_server_loop {
            this.available_context_servers_changed(cx);
        }
        this
    }

    pub fn get_server(&self, id: &ContextServerId) -> Option<Arc<ContextServer>> {
        self.servers.get(id).map(|state| state.server())
    }

    pub fn get_running_server(&self, id: &ContextServerId) -> Option<Arc<ContextServer>> {
        if let Some(ContextServerState::Running { server, .. }) = self.servers.get(id) {
            Some(server.clone())
        } else {
            None
        }
    }

    pub fn status_for_server(&self, id: &ContextServerId) -> Option<ContextServerStatus> {
        self.servers.get(id).map(ContextServerStatus::from_state)
    }

    pub fn configuration_for_server(
        &self,
        id: &ContextServerId,
    ) -> Option<Arc<ContextServerConfiguration>> {
        self.servers.get(id).map(|state| state.configuration())
    }

    pub fn all_server_ids(&self) -> Vec<ContextServerId> {
        self.servers.keys().cloned().collect()
    }

    pub fn running_servers(&self) -> Vec<Arc<ContextServer>> {
        self.servers
            .values()
            .filter_map(|state| {
                if let ContextServerState::Running { server, .. } = state {
                    Some(server.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn start_server(&mut self, server: Arc<ContextServer>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let this = this.upgrade().context("Context server store dropped")?;
            let settings = this
                .update(cx, |this, _| {
                    this.context_server_settings.get(&server.id().0).cloned()
                })
                .ok()
                .flatten()
                .context("Failed to get context server settings")?;

            if !settings.enabled() {
                return Ok(());
            }

            let (registry, worktree_store) = this.update(cx, |this, _| {
                (this.registry.clone(), this.worktree_store.clone())
            })?;
            let configuration = ContextServerConfiguration::from_settings(
                settings,
                server.id(),
                registry,
                worktree_store,
                cx,
            )
            .await
            .context("Failed to create context server configuration")?;

            this.update(cx, |this, cx| {
                this.run_server(server, Arc::new(configuration), cx)
            })
        })
        .detach_and_log_err(cx);
    }

    pub fn stop_server(&mut self, id: &ContextServerId, cx: &mut Context<Self>) -> Result<()> {
        if matches!(
            self.servers.get(id),
            Some(ContextServerState::Stopped { .. })
        ) {
            return Ok(());
        }

        let state = self
            .servers
            .remove(id)
            .context("Context server not found")?;

        let server = state.server();
        let configuration = state.configuration();
        let mut result = Ok(());
        if let ContextServerState::Running { server, .. } = &state {
            result = server.stop();
        }
        drop(state);

        self.update_server_state(
            id.clone(),
            ContextServerState::Stopped {
                configuration,
                server,
            },
            cx,
        );

        result
    }

    pub fn restart_server(&mut self, id: &ContextServerId, cx: &mut Context<Self>) -> Result<()> {
        if let Some(state) = self.servers.get(id) {
            let configuration = state.configuration();

            self.stop_server(&state.server().id(), cx)?;
            let new_server = self.create_context_server(id.clone(), configuration.clone(), cx);
            self.run_server(new_server, configuration, cx);
        }
        Ok(())
    }

    fn run_server(
        &mut self,
        server: Arc<ContextServer>,
        configuration: Arc<ContextServerConfiguration>,
        cx: &mut Context<Self>,
    ) {
        let id = server.id();
        if matches!(
            self.servers.get(&id),
            Some(ContextServerState::Starting { .. } | ContextServerState::Running { .. })
        ) {
            self.stop_server(&id, cx).log_err();
        }

        let task = cx.spawn({
            let id = server.id();
            let server = server.clone();
            let configuration = configuration.clone();
            async move |this, cx| {
                match server.clone().start(cx).await {
                    Ok(_) => {
                        debug_assert!(server.client().is_some());

                        this.update(cx, |this, cx| {
                            this.update_server_state(
                                id.clone(),
                                ContextServerState::Running {
                                    server,
                                    configuration,
                                },
                                cx,
                            )
                        })
                        .log_err()
                    }
                    Err(err) => {
                        log::error!("{} context server failed to start: {}", id, err);
                        this.update(cx, |this, cx| {
                            this.update_server_state(
                                id.clone(),
                                ContextServerState::Error {
                                    configuration,
                                    server,
                                    error: err.to_string().into(),
                                },
                                cx,
                            )
                        })
                        .log_err()
                    }
                };
            }
        });

        self.update_server_state(
            id.clone(),
            ContextServerState::Starting {
                configuration,
                _task: task,
                server,
            },
            cx,
        );
    }

    fn remove_server(&mut self, id: &ContextServerId, cx: &mut Context<Self>) -> Result<()> {
        let state = self
            .servers
            .remove(id)
            .context("Context server not found")?;
        drop(state);
        cx.emit(Event::ServerStatusChanged {
            server_id: id.clone(),
            status: ContextServerStatus::Stopped,
        });
        Ok(())
    }

    fn create_context_server(
        &self,
        id: ContextServerId,
        configuration: Arc<ContextServerConfiguration>,
        cx: &mut Context<Self>,
    ) -> Arc<ContextServer> {
        let root_path = self
            .project
            .read_with(cx, |project, cx| project.active_project_directory(cx))
            .ok()
            .flatten()
            .or_else(|| {
                self.worktree_store.read_with(cx, |store, cx| {
                    store.visible_worktrees(cx).fold(None, |acc, item| {
                        if acc.is_none() {
                            item.read(cx).root_dir()
                        } else {
                            acc
                        }
                    })
                })
            });

        if let Some(factory) = self.context_server_factory.as_ref() {
            factory(id, configuration)
        } else {
            Arc::new(ContextServer::stdio(
                id,
                configuration.command().clone(),
                root_path,
            ))
        }
    }

    fn resolve_context_server_settings<'a>(
        worktree_store: &'a Entity<WorktreeStore>,
        cx: &'a App,
    ) -> &'a HashMap<Arc<str>, ContextServerSettings> {
        let location = worktree_store
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .map(|worktree| settings::SettingsLocation {
                worktree_id: worktree.read(cx).id(),
                path: Path::new(""),
            });
        &ProjectSettings::get(location, cx).context_servers
    }

    fn update_server_state(
        &mut self,
        id: ContextServerId,
        state: ContextServerState,
        cx: &mut Context<Self>,
    ) {
        let status = ContextServerStatus::from_state(&state);
        self.servers.insert(id.clone(), state);
        cx.emit(Event::ServerStatusChanged {
            server_id: id,
            status,
        });
    }

    fn available_context_servers_changed(&mut self, cx: &mut Context<Self>) {
        if self.update_servers_task.is_some() {
            self.needs_server_update = true;
        } else {
            self.needs_server_update = false;
            self.update_servers_task = Some(cx.spawn(async move |this, cx| {
                if let Err(err) = Self::maintain_servers(this.clone(), cx).await {
                    log::error!("Error maintaining context servers: {}", err);
                }

                this.update(cx, |this, cx| {
                    this.update_servers_task.take();
                    if this.needs_server_update {
                        this.available_context_servers_changed(cx);
                    }
                })?;

                Ok(())
            }));
        }
    }

    async fn maintain_servers(this: WeakEntity<Self>, cx: &mut AsyncApp) -> Result<()> {
        let (mut configured_servers, registry, worktree_store) = this.update(cx, |this, _| {
            (
                this.context_server_settings.clone(),
                this.registry.clone(),
                this.worktree_store.clone(),
            )
        })?;

        for (id, _) in
            registry.read_with(cx, |registry, _| registry.context_server_descriptors())?
        {
            configured_servers
                .entry(id)
                .or_insert(ContextServerSettings::default_extension());
        }

        let (enabled_servers, disabled_servers): (HashMap<_, _>, HashMap<_, _>) =
            configured_servers
                .into_iter()
                .partition(|(_, settings)| settings.enabled());

        let configured_servers = join_all(enabled_servers.into_iter().map(|(id, settings)| {
            let id = ContextServerId(id);
            ContextServerConfiguration::from_settings(
                settings,
                id.clone(),
                registry.clone(),
                worktree_store.clone(),
                cx,
            )
            .map(|config| (id, config))
        }))
        .await
        .into_iter()
        .filter_map(|(id, config)| config.map(|config| (id, config)))
        .collect::<HashMap<_, _>>();

        let mut servers_to_start = Vec::new();
        let mut servers_to_remove = HashSet::default();
        let mut servers_to_stop = HashSet::default();

        this.update(cx, |this, cx| {
            for server_id in this.servers.keys() {
                // All servers that are not in desired_servers should be removed from the store.
                // This can happen if the user removed a server from the context server settings.
                if !configured_servers.contains_key(server_id) {
                    if disabled_servers.contains_key(&server_id.0) {
                        servers_to_stop.insert(server_id.clone());
                    } else {
                        servers_to_remove.insert(server_id.clone());
                    }
                }
            }

            for (id, config) in configured_servers {
                let state = this.servers.get(&id);
                let is_stopped = matches!(state, Some(ContextServerState::Stopped { .. }));
                let existing_config = state.as_ref().map(|state| state.configuration());
                if existing_config.as_deref() != Some(&config) || is_stopped {
                    let config = Arc::new(config);
                    let server = this.create_context_server(id.clone(), config.clone(), cx);
                    servers_to_start.push((server, config));
                    if this.servers.contains_key(&id) {
                        servers_to_stop.insert(id);
                    }
                }
            }
        })?;

        this.update(cx, |this, cx| {
            for id in servers_to_stop {
                this.stop_server(&id, cx)?;
            }
            for id in servers_to_remove {
                this.remove_server(&id, cx)?;
            }
            for (server, config) in servers_to_start {
                this.run_server(server, config, cx);
            }
            anyhow::Ok(())
        })?
    }
}
