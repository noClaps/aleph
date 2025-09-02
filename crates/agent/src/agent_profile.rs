use std::sync::Arc;

use agent_settings::{AgentProfileId, AgentProfileSettings, AgentSettings};
use assistant_tool::{Tool, ToolSource, ToolWorkingSet, UniqueToolName};
use collections::IndexMap;
use convert_case::{Case, Casing};
use fs::Fs;
use gpui::{App, Entity, SharedString};
use settings::{Settings, update_settings_file};
use util::ResultExt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentProfile {
    id: AgentProfileId,
    tool_set: Entity<ToolWorkingSet>,
}

pub type AvailableProfiles = IndexMap<AgentProfileId, SharedString>;

impl AgentProfile {
    pub fn new(id: AgentProfileId, tool_set: Entity<ToolWorkingSet>) -> Self {
        Self { id, tool_set }
    }

    /// Saves a new profile to the settings.
    pub fn create(
        name: String,
        base_profile_id: Option<AgentProfileId>,
        fs: Arc<dyn Fs>,
        cx: &App,
    ) -> AgentProfileId {
        let id = AgentProfileId(name.to_case(Case::Kebab).into());

        let base_profile =
            base_profile_id.and_then(|id| AgentSettings::get_global(cx).profiles.get(&id).cloned());

        let profile_settings = AgentProfileSettings {
            name: name.into(),
            tools: base_profile
                .as_ref()
                .map(|profile| profile.tools.clone())
                .unwrap_or_default(),
            enable_all_context_servers: base_profile
                .as_ref()
                .map(|profile| profile.enable_all_context_servers)
                .unwrap_or_default(),
            context_servers: base_profile
                .map(|profile| profile.context_servers)
                .unwrap_or_default(),
        };

        update_settings_file::<AgentSettings>(fs, cx, {
            let id = id.clone();
            move |settings, _cx| {
                settings.create_profile(id, profile_settings).log_err();
            }
        });

        id
    }

    /// Returns a map of AgentProfileIds to their names
    pub fn available_profiles(cx: &App) -> AvailableProfiles {
        let mut profiles = AvailableProfiles::default();
        for (id, profile) in AgentSettings::get_global(cx).profiles.iter() {
            profiles.insert(id.clone(), profile.name.clone());
        }
        profiles
    }

    pub fn id(&self) -> &AgentProfileId {
        &self.id
    }

    pub fn enabled_tools(&self, cx: &App) -> Vec<(UniqueToolName, Arc<dyn Tool>)> {
        let Some(settings) = AgentSettings::get_global(cx).profiles.get(&self.id) else {
            return Vec::new();
        };

        self.tool_set
            .read(cx)
            .tools(cx)
            .into_iter()
            .filter(|(_, tool)| Self::is_enabled(settings, tool.source(), tool.name()))
            .collect()
    }

    pub fn is_tool_enabled(&self, source: ToolSource, tool_name: String, cx: &App) -> bool {
        let Some(settings) = AgentSettings::get_global(cx).profiles.get(&self.id) else {
            return false;
        };

        Self::is_enabled(settings, source, tool_name)
    }

    fn is_enabled(settings: &AgentProfileSettings, source: ToolSource, name: String) -> bool {
        match source {
            ToolSource::Native => *settings.tools.get(name.as_str()).unwrap_or(&false),
            ToolSource::ContextServer { id } => settings
                .context_servers
                .get(id.as_ref())
                .and_then(|preset| preset.tools.get(name.as_str()).copied())
                .unwrap_or(settings.enable_all_context_servers),
        }
    }
}
