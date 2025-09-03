use serde::{Deserialize, Serialize};
use strum::EnumIter;

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum BedrockModelMode {
    #[default]
    Default,
    Thinking {
        budget_tokens: Option<u64>,
    },
}

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct BedrockModelCacheConfiguration {
    pub max_cache_anchors: usize,
    pub min_total_token: u64,
}

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, EnumIter)]
pub enum Model {
    // Amazon Nova Models
    #[default]
    AmazonNovaLite,
    AmazonNovaMicro,
    AmazonNovaPro,
    AmazonNovaPremier,
    // AI21 models
    AI21J2GrandeInstruct,
    AI21J2JumboInstruct,
    AI21J2Mid,
    AI21J2MidV1,
    AI21J2Ultra,
    AI21J2UltraV1_8k,
    AI21J2UltraV1,
    AI21JambaInstructV1,
    AI21Jamba15LargeV1,
    AI21Jamba15MiniV1,
    // Cohere models
    CohereCommandTextV14_4k,
    CohereCommandRV1,
    CohereCommandRPlusV1,
    CohereCommandLightTextV14_4k,
    // DeepSeek
    DeepSeekR1,
    // Meta models
    MetaLlama38BInstructV1,
    MetaLlama370BInstructV1,
    MetaLlama318BInstructV1_128k,
    MetaLlama318BInstructV1,
    MetaLlama3170BInstructV1_128k,
    MetaLlama3170BInstructV1,
    MetaLlama31405BInstructV1,
    MetaLlama321BInstructV1,
    MetaLlama323BInstructV1,
    MetaLlama3211BInstructV1,
    MetaLlama3290BInstructV1,
    MetaLlama3370BInstructV1,
    #[allow(non_camel_case_types)]
    MetaLlama4Scout17BInstructV1,
    #[allow(non_camel_case_types)]
    MetaLlama4Maverick17BInstructV1,
    // Mistral models
    MistralMistral7BInstructV0,
    MistralMixtral8x7BInstructV0,
    MistralMistralLarge2402V1,
    MistralMistralSmall2402V1,
    MistralPixtralLarge2502V1,
    // Writer models
    PalmyraWriterX5,
    PalmyraWriterX4,
    #[serde(rename = "custom")]
    Custom {
        name: String,
        max_tokens: u64,
        /// The name displayed in the UI, such as in the assistant panel model dropdown menu.
        display_name: Option<String>,
        max_output_tokens: Option<u64>,
        default_temperature: Option<f32>,
        cache_configuration: Option<BedrockModelCacheConfiguration>,
    },
}

impl Model {
    pub fn default_fast() -> Self {
        Self::default()
    }

    pub fn from_id(id: &str) -> anyhow::Result<Self> {
        anyhow::bail!("invalid model id {id}");
    }

    pub fn id(&self) -> &str {
        match self {
            Model::AmazonNovaLite => "amazon-nova-lite",
            Model::AmazonNovaMicro => "amazon-nova-micro",
            Model::AmazonNovaPro => "amazon-nova-pro",
            Model::AmazonNovaPremier => "amazon-nova-premier",
            Model::DeepSeekR1 => "deepseek-r1",
            Model::AI21J2GrandeInstruct => "ai21-j2-grande-instruct",
            Model::AI21J2JumboInstruct => "ai21-j2-jumbo-instruct",
            Model::AI21J2Mid => "ai21-j2-mid",
            Model::AI21J2MidV1 => "ai21-j2-mid-v1",
            Model::AI21J2Ultra => "ai21-j2-ultra",
            Model::AI21J2UltraV1_8k => "ai21-j2-ultra-v1-8k",
            Model::AI21J2UltraV1 => "ai21-j2-ultra-v1",
            Model::AI21JambaInstructV1 => "ai21-jamba-instruct-v1",
            Model::AI21Jamba15LargeV1 => "ai21-jamba-1-5-large-v1",
            Model::AI21Jamba15MiniV1 => "ai21-jamba-1-5-mini-v1",
            Model::CohereCommandTextV14_4k => "cohere-command-text-v14-4k",
            Model::CohereCommandRV1 => "cohere-command-r-v1",
            Model::CohereCommandRPlusV1 => "cohere-command-r-plus-v1",
            Model::CohereCommandLightTextV14_4k => "cohere-command-light-text-v14-4k",
            Model::MetaLlama38BInstructV1 => "meta-llama3-8b-instruct-v1",
            Model::MetaLlama370BInstructV1 => "meta-llama3-70b-instruct-v1",
            Model::MetaLlama318BInstructV1_128k => "meta-llama3-1-8b-instruct-v1-128k",
            Model::MetaLlama318BInstructV1 => "meta-llama3-1-8b-instruct-v1",
            Model::MetaLlama3170BInstructV1_128k => "meta-llama3-1-70b-instruct-v1-128k",
            Model::MetaLlama3170BInstructV1 => "meta-llama3-1-70b-instruct-v1",
            Model::MetaLlama31405BInstructV1 => "meta-llama3-1-405b-instruct-v1",
            Model::MetaLlama321BInstructV1 => "meta-llama3-2-1b-instruct-v1",
            Model::MetaLlama323BInstructV1 => "meta-llama3-2-3b-instruct-v1",
            Model::MetaLlama3211BInstructV1 => "meta-llama3-2-11b-instruct-v1",
            Model::MetaLlama3290BInstructV1 => "meta-llama3-2-90b-instruct-v1",
            Model::MetaLlama3370BInstructV1 => "meta-llama3-3-70b-instruct-v1",
            Model::MetaLlama4Scout17BInstructV1 => "meta-llama4-scout-17b-instruct-v1",
            Model::MetaLlama4Maverick17BInstructV1 => "meta-llama4-maverick-17b-instruct-v1",
            Model::MistralMistral7BInstructV0 => "mistral-7b-instruct-v0",
            Model::MistralMixtral8x7BInstructV0 => "mistral-mixtral-8x7b-instruct-v0",
            Model::MistralMistralLarge2402V1 => "mistral-large-2402-v1",
            Model::MistralMistralSmall2402V1 => "mistral-small-2402-v1",
            Model::MistralPixtralLarge2502V1 => "mistral-pixtral-large-2502-v1",
            Model::PalmyraWriterX4 => "palmyra-writer-x4",
            Model::PalmyraWriterX5 => "palmyra-writer-x5",
            Self::Custom { name, .. } => name,
        }
    }

    pub fn request_id(&self) -> &str {
        match self {
            Model::AmazonNovaLite => "amazon.nova-lite-v1:0",
            Model::AmazonNovaMicro => "amazon.nova-micro-v1:0",
            Model::AmazonNovaPro => "amazon.nova-pro-v1:0",
            Model::AmazonNovaPremier => "amazon.nova-premier-v1:0",
            Model::DeepSeekR1 => "deepseek.r1-v1:0",
            Model::AI21J2GrandeInstruct => "ai21.j2-grande-instruct",
            Model::AI21J2JumboInstruct => "ai21.j2-jumbo-instruct",
            Model::AI21J2Mid => "ai21.j2-mid",
            Model::AI21J2MidV1 => "ai21.j2-mid-v1",
            Model::AI21J2Ultra => "ai21.j2-ultra",
            Model::AI21J2UltraV1_8k => "ai21.j2-ultra-v1:0:8k",
            Model::AI21J2UltraV1 => "ai21.j2-ultra-v1",
            Model::AI21JambaInstructV1 => "ai21.jamba-instruct-v1:0",
            Model::AI21Jamba15LargeV1 => "ai21.jamba-1-5-large-v1:0",
            Model::AI21Jamba15MiniV1 => "ai21.jamba-1-5-mini-v1:0",
            Model::CohereCommandTextV14_4k => "cohere.command-text-v14:7:4k",
            Model::CohereCommandRV1 => "cohere.command-r-v1:0",
            Model::CohereCommandRPlusV1 => "cohere.command-r-plus-v1:0",
            Model::CohereCommandLightTextV14_4k => "cohere.command-light-text-v14:7:4k",
            Model::MetaLlama38BInstructV1 => "meta.llama3-8b-instruct-v1:0",
            Model::MetaLlama370BInstructV1 => "meta.llama3-70b-instruct-v1:0",
            Model::MetaLlama318BInstructV1_128k => "meta.llama3-1-8b-instruct-v1:0",
            Model::MetaLlama318BInstructV1 => "meta.llama3-1-8b-instruct-v1:0",
            Model::MetaLlama3170BInstructV1_128k => "meta.llama3-1-70b-instruct-v1:0",
            Model::MetaLlama3170BInstructV1 => "meta.llama3-1-70b-instruct-v1:0",
            Model::MetaLlama31405BInstructV1 => "meta.llama3-1-405b-instruct-v1:0",
            Model::MetaLlama3211BInstructV1 => "meta.llama3-2-11b-instruct-v1:0",
            Model::MetaLlama3290BInstructV1 => "meta.llama3-2-90b-instruct-v1:0",
            Model::MetaLlama321BInstructV1 => "meta.llama3-2-1b-instruct-v1:0",
            Model::MetaLlama323BInstructV1 => "meta.llama3-2-3b-instruct-v1:0",
            Model::MetaLlama3370BInstructV1 => "meta.llama3-3-70b-instruct-v1:0",
            Model::MetaLlama4Scout17BInstructV1 => "meta.llama4-scout-17b-instruct-v1:0",
            Model::MetaLlama4Maverick17BInstructV1 => "meta.llama4-maverick-17b-instruct-v1:0",
            Model::MistralMistral7BInstructV0 => "mistral.mistral-7b-instruct-v0:2",
            Model::MistralMixtral8x7BInstructV0 => "mistral.mixtral-8x7b-instruct-v0:1",
            Model::MistralMistralLarge2402V1 => "mistral.mistral-large-2402-v1:0",
            Model::MistralMistralSmall2402V1 => "mistral.mistral-small-2402-v1:0",
            Model::MistralPixtralLarge2502V1 => "mistral.pixtral-large-2502-v1:0",
            Model::PalmyraWriterX4 => "writer.palmyra-x4-v1:0",
            Model::PalmyraWriterX5 => "writer.palmyra-x5-v1:0",
            Self::Custom { name, .. } => name,
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::AmazonNovaLite => "Amazon Nova Lite",
            Self::AmazonNovaMicro => "Amazon Nova Micro",
            Self::AmazonNovaPro => "Amazon Nova Pro",
            Self::AmazonNovaPremier => "Amazon Nova Premier",
            Self::DeepSeekR1 => "DeepSeek R1",
            Self::AI21J2GrandeInstruct => "AI21 Jurassic2 Grande Instruct",
            Self::AI21J2JumboInstruct => "AI21 Jurassic2 Jumbo Instruct",
            Self::AI21J2Mid => "AI21 Jurassic2 Mid",
            Self::AI21J2MidV1 => "AI21 Jurassic2 Mid V1",
            Self::AI21J2Ultra => "AI21 Jurassic2 Ultra",
            Self::AI21J2UltraV1_8k => "AI21 Jurassic2 Ultra V1 8K",
            Self::AI21J2UltraV1 => "AI21 Jurassic2 Ultra V1",
            Self::AI21JambaInstructV1 => "AI21 Jamba Instruct",
            Self::AI21Jamba15LargeV1 => "AI21 Jamba 1.5 Large",
            Self::AI21Jamba15MiniV1 => "AI21 Jamba 1.5 Mini",
            Self::CohereCommandTextV14_4k => "Cohere Command Text V14 4K",
            Self::CohereCommandRV1 => "Cohere Command R V1",
            Self::CohereCommandRPlusV1 => "Cohere Command R Plus V1",
            Self::CohereCommandLightTextV14_4k => "Cohere Command Light Text V14 4K",
            Self::MetaLlama38BInstructV1 => "Meta Llama 3 8B Instruct",
            Self::MetaLlama370BInstructV1 => "Meta Llama 3 70B Instruct",
            Self::MetaLlama318BInstructV1_128k => "Meta Llama 3.1 8B Instruct 128K",
            Self::MetaLlama318BInstructV1 => "Meta Llama 3.1 8B Instruct",
            Self::MetaLlama3170BInstructV1_128k => "Meta Llama 3.1 70B Instruct 128K",
            Self::MetaLlama3170BInstructV1 => "Meta Llama 3.1 70B Instruct",
            Self::MetaLlama31405BInstructV1 => "Meta Llama 3.1 405B Instruct",
            Self::MetaLlama3211BInstructV1 => "Meta Llama 3.2 11B Instruct",
            Self::MetaLlama3290BInstructV1 => "Meta Llama 3.2 90B Instruct",
            Self::MetaLlama321BInstructV1 => "Meta Llama 3.2 1B Instruct",
            Self::MetaLlama323BInstructV1 => "Meta Llama 3.2 3B Instruct",
            Self::MetaLlama3370BInstructV1 => "Meta Llama 3.3 70B Instruct",
            Self::MetaLlama4Scout17BInstructV1 => "Meta Llama 4 Scout 17B Instruct",
            Self::MetaLlama4Maverick17BInstructV1 => "Meta Llama 4 Maverick 17B Instruct",
            Self::MistralMistral7BInstructV0 => "Mistral 7B Instruct V0",
            Self::MistralMixtral8x7BInstructV0 => "Mistral Mixtral 8x7B Instruct V0",
            Self::MistralMistralLarge2402V1 => "Mistral Large 2402 V1",
            Self::MistralMistralSmall2402V1 => "Mistral Small 2402 V1",
            Self::MistralPixtralLarge2502V1 => "Pixtral Large 25.02 V1",
            Self::PalmyraWriterX5 => "Writer Palmyra X5",
            Self::PalmyraWriterX4 => "Writer Palmyra X4",
            Self::Custom {
                display_name, name, ..
            } => display_name.as_deref().unwrap_or(name),
        }
    }

    pub fn max_token_count(&self) -> u64 {
        match self {
            Self::AmazonNovaPremier => 1_000_000,
            Self::PalmyraWriterX5 => 1_000_000,
            Self::PalmyraWriterX4 => 128_000,
            Self::Custom { max_tokens, .. } => *max_tokens,
            _ => 128_000,
        }
    }

    pub fn max_output_tokens(&self) -> u64 {
        match self {
            Self::PalmyraWriterX4 | Self::PalmyraWriterX5 => 8_192,
            Self::Custom {
                max_output_tokens, ..
            } => max_output_tokens.unwrap_or(4_096),
            _ => 4_096,
        }
    }

    pub fn default_temperature(&self) -> f32 {
        match self {
            Self::Custom {
                default_temperature,
                ..
            } => default_temperature.unwrap_or(1.0),
            _ => 1.0,
        }
    }

    pub fn supports_tool_use(&self) -> bool {
        match self {
            // Amazon Nova models (all support tool use)
            Self::AmazonNovaPremier
            | Self::AmazonNovaPro
            | Self::AmazonNovaLite
            | Self::AmazonNovaMicro => true,

            // AI21 Jamba 1.5 models support tool use
            Self::AI21Jamba15LargeV1 | Self::AI21Jamba15MiniV1 => true,

            // Cohere Command R models support tool use
            Self::CohereCommandRV1 | Self::CohereCommandRPlusV1 => true,

            // All other models don't support tool use
            // Including Meta Llama 3.2, AI21 Jurassic, and others
            _ => false,
        }
    }

    pub fn supports_caching(&self) -> bool {
        match self {
            // Nova models support only text caching
            // https://docs.aws.amazon.com/bedrock/latest/userguide/prompt-caching.html#prompt-caching-models

            // Custom models - check if they have cache configuration
            Self::Custom {
                cache_configuration,
                ..
            } => cache_configuration.is_some(),

            // All other models don't support caching
            _ => false,
        }
    }

    pub fn cache_configuration(&self) -> Option<BedrockModelCacheConfiguration> {
        match self {
            Self::Custom {
                cache_configuration,
                ..
            } => cache_configuration.clone(),

            _ => None,
        }
    }

    pub fn mode(&self) -> BedrockModelMode {
        BedrockModelMode::Default
    }

    pub fn cross_region_inference_id(&self, region: &str) -> anyhow::Result<String> {
        let region_group = if region.starts_with("us-gov-") {
            "us-gov"
        } else if region.starts_with("us-") {
            "us"
        } else if region.starts_with("eu-") {
            "eu"
        } else if region.starts_with("ap-") || region == "me-central-1" || region == "me-south-1" {
            "apac"
        } else if region.starts_with("ca-") || region.starts_with("sa-") {
            // Canada and South America regions - default to US profiles
            "us"
        } else {
            anyhow::bail!("Unsupported Region {region}");
        };

        let model_id = self.request_id();

        match (self, region_group) {
            // Custom models can't have CRI IDs
            (Model::Custom { .. }, _) => Ok(self.request_id().into()),

            // Available everywhere
            (Model::AmazonNovaLite | Model::AmazonNovaMicro | Model::AmazonNovaPro, _) => {
                Ok(format!("{}.{}", region_group, model_id))
            }

            // Models in US
            (
                Model::AmazonNovaPremier
                | Model::DeepSeekR1
                | Model::MetaLlama31405BInstructV1
                | Model::MetaLlama3170BInstructV1_128k
                | Model::MetaLlama3170BInstructV1
                | Model::MetaLlama318BInstructV1_128k
                | Model::MetaLlama318BInstructV1
                | Model::MetaLlama3211BInstructV1
                | Model::MetaLlama321BInstructV1
                | Model::MetaLlama323BInstructV1
                | Model::MetaLlama3290BInstructV1
                | Model::MetaLlama3370BInstructV1
                | Model::MetaLlama4Maverick17BInstructV1
                | Model::MetaLlama4Scout17BInstructV1
                | Model::MistralPixtralLarge2502V1
                | Model::PalmyraWriterX4
                | Model::PalmyraWriterX5,
                "us",
            ) => Ok(format!("{}.{}", region_group, model_id)),

            // Models available in EU
            (
                Model::MetaLlama321BInstructV1
                | Model::MetaLlama323BInstructV1
                | Model::MistralPixtralLarge2502V1,
                "eu",
            ) => Ok(format!("{}.{}", region_group, model_id)),

            // Any other combination is not supported
            _ => Ok(self.request_id().into()),
        }
    }
}
