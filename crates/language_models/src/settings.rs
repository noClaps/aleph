use std::sync::Arc;

use anyhow::Result;
use collections::HashMap;
use gpui::App;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::{Settings, SettingsKey, SettingsSources, SettingsUi};

use crate::provider::{
    self, google::GoogleSettings, lmstudio::LmStudioSettings, mistral::MistralSettings,
    ollama::OllamaSettings, open_ai::OpenAiSettings, open_ai_compatible::OpenAiCompatibleSettings,
    open_router::OpenRouterSettings, vercel::VercelSettings, x_ai::XAiSettings,
};

/// Initializes the language model settings.
pub fn init_settings(cx: &mut App) {
    AllLanguageModelSettings::register(cx);
}

#[derive(Default)]
pub struct AllLanguageModelSettings {
    pub google: GoogleSettings,
    pub lmstudio: LmStudioSettings,
    pub mistral: MistralSettings,
    pub ollama: OllamaSettings,
    pub open_router: OpenRouterSettings,
    pub openai: OpenAiSettings,
    pub openai_compatible: HashMap<Arc<str>, OpenAiCompatibleSettings>,
    pub vercel: VercelSettings,
    pub x_ai: XAiSettings,
}

#[derive(
    Default, Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema, SettingsUi, SettingsKey,
)]
#[settings_key(key = "language_models")]
pub struct AllLanguageModelSettingsContent {
    pub google: Option<GoogleSettingsContent>,
    pub lmstudio: Option<LmStudioSettingsContent>,
    pub mistral: Option<MistralSettingsContent>,
    pub ollama: Option<OllamaSettingsContent>,
    pub open_router: Option<OpenRouterSettingsContent>,
    pub openai: Option<OpenAiSettingsContent>,
    pub openai_compatible: Option<HashMap<Arc<str>, OpenAiCompatibleSettingsContent>>,
    pub vercel: Option<VercelSettingsContent>,
    pub x_ai: Option<XAiSettingsContent>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct OllamaSettingsContent {
    pub api_url: Option<String>,
    pub available_models: Option<Vec<provider::ollama::AvailableModel>>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct LmStudioSettingsContent {
    pub api_url: Option<String>,
    pub available_models: Option<Vec<provider::lmstudio::AvailableModel>>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct MistralSettingsContent {
    pub api_url: Option<String>,
    pub available_models: Option<Vec<provider::mistral::AvailableModel>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct OpenAiSettingsContent {
    pub api_url: Option<String>,
    pub available_models: Option<Vec<provider::open_ai::AvailableModel>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct OpenAiCompatibleSettingsContent {
    pub api_url: String,
    pub available_models: Vec<provider::open_ai_compatible::AvailableModel>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct VercelSettingsContent {
    pub api_url: Option<String>,
    pub available_models: Option<Vec<provider::vercel::AvailableModel>>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct GoogleSettingsContent {
    pub api_url: Option<String>,
    pub available_models: Option<Vec<provider::google::AvailableModel>>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct XAiSettingsContent {
    pub api_url: Option<String>,
    pub available_models: Option<Vec<provider::x_ai::AvailableModel>>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct OpenRouterSettingsContent {
    pub api_url: Option<String>,
    pub available_models: Option<Vec<provider::open_router::AvailableModel>>,
}

impl settings::Settings for AllLanguageModelSettings {
    const PRESERVED_KEYS: Option<&'static [&'static str]> = Some(&["version"]);

    type FileContent = AllLanguageModelSettingsContent;

    fn load(sources: SettingsSources<Self::FileContent>, _: &mut App) -> Result<Self> {
        fn merge<T>(target: &mut T, value: Option<T>) {
            if let Some(value) = value {
                *target = value;
            }
        }

        let mut settings = AllLanguageModelSettings::default();

        for value in sources.defaults_and_customizations() {
            // Ollama
            let ollama = value.ollama.clone();

            merge(
                &mut settings.ollama.api_url,
                value.ollama.as_ref().and_then(|s| s.api_url.clone()),
            );
            merge(
                &mut settings.ollama.available_models,
                ollama.as_ref().and_then(|s| s.available_models.clone()),
            );

            // LM Studio
            let lmstudio = value.lmstudio.clone();

            merge(
                &mut settings.lmstudio.api_url,
                value.lmstudio.as_ref().and_then(|s| s.api_url.clone()),
            );
            merge(
                &mut settings.lmstudio.available_models,
                lmstudio.as_ref().and_then(|s| s.available_models.clone()),
            );

            // OpenAI
            let openai = value.openai.clone();
            merge(
                &mut settings.openai.api_url,
                openai.as_ref().and_then(|s| s.api_url.clone()),
            );
            merge(
                &mut settings.openai.available_models,
                openai.as_ref().and_then(|s| s.available_models.clone()),
            );

            // OpenAI Compatible
            if let Some(openai_compatible) = value.openai_compatible.clone() {
                for (id, openai_compatible_settings) in openai_compatible {
                    settings.openai_compatible.insert(
                        id,
                        OpenAiCompatibleSettings {
                            api_url: openai_compatible_settings.api_url,
                            available_models: openai_compatible_settings.available_models,
                        },
                    );
                }
            }

            // Vercel
            let vercel = value.vercel.clone();
            merge(
                &mut settings.vercel.api_url,
                vercel.as_ref().and_then(|s| s.api_url.clone()),
            );
            merge(
                &mut settings.vercel.available_models,
                vercel.as_ref().and_then(|s| s.available_models.clone()),
            );

            // XAI
            let x_ai = value.x_ai.clone();
            merge(
                &mut settings.x_ai.api_url,
                x_ai.as_ref().and_then(|s| s.api_url.clone()),
            );
            merge(
                &mut settings.x_ai.available_models,
                x_ai.as_ref().and_then(|s| s.available_models.clone()),
            );

            merge(
                &mut settings.google.api_url,
                value.google.as_ref().and_then(|s| s.api_url.clone()),
            );
            merge(
                &mut settings.google.available_models,
                value
                    .google
                    .as_ref()
                    .and_then(|s| s.available_models.clone()),
            );

            // Mistral
            let mistral = value.mistral.clone();
            merge(
                &mut settings.mistral.api_url,
                mistral.as_ref().and_then(|s| s.api_url.clone()),
            );
            merge(
                &mut settings.mistral.available_models,
                mistral.as_ref().and_then(|s| s.available_models.clone()),
            );

            // OpenRouter
            let open_router = value.open_router.clone();
            merge(
                &mut settings.open_router.api_url,
                open_router.as_ref().and_then(|s| s.api_url.clone()),
            );
            merge(
                &mut settings.open_router.available_models,
                open_router
                    .as_ref()
                    .and_then(|s| s.available_models.clone()),
            );
        }

        Ok(settings)
    }

    fn import_from_vscode(_vscode: &settings::VsCodeSettings, _current: &mut Self::FileContent) {}
}
