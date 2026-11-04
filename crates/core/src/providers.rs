//! The real provider source: local Ollama on loopback (models from the active registry
//! profile) and, only when the turn engine asks for it, Anthropic. Everything about WHEN a
//! provider may be used (`local_only`, model id, key, preview consent) is decided by the turn
//! engine and `Buddy::prepare_and_consent`; this file only builds providers.

use std::sync::Arc;

use llm::{
    AnthropicConfig, AnthropicProvider, CredentialStore, LlmError, LlmProvider, OllamaConfig,
    OllamaProvider, DEFAULT_OLLAMA_BASE_URL,
};

use crate::settings::builtin_registry;
use crate::turn::ProviderSource;

pub struct DefaultProviders {
    credentials: Arc<dyn CredentialStore>,
}

impl DefaultProviders {
    pub fn new(credentials: Arc<dyn CredentialStore>) -> Self {
        Self { credentials }
    }
}

/// The Ollama tags of the profile's text model and optional vision model.
pub fn ollama_models(profile: &str) -> Result<(String, Option<String>), LlmError> {
    let reg = builtin_registry();
    let p = reg
        .profile(profile)
        .ok_or(LlmError::Config("unknown_profile"))?;
    let tag = |model: &str| -> Result<String, LlmError> {
        reg.get(model)
            .map_err(|_| LlmError::Config("unknown_model"))?
            .ollama_tag
            .clone()
            .ok_or(LlmError::Config("ollama_tag_missing"))
    };
    let text = tag(&p.llm.model)?;
    let vision = p.vlm.as_ref().map(|v| tag(&v.model)).transpose()?;
    Ok((text, vision))
}

impl ProviderSource for DefaultProviders {
    fn local(&self, profile: &str) -> Result<Arc<dyn LlmProvider>, LlmError> {
        let (text_model, vision_model) = ollama_models(profile)?;
        let p = OllamaProvider::new(OllamaConfig {
            base_url: DEFAULT_OLLAMA_BASE_URL.into(),
            text_model,
            vision_model,
        })?;
        Ok(Arc::new(p))
    }

    fn cloud(&self, model: &str) -> Result<Arc<dyn LlmProvider>, LlmError> {
        let p = AnthropicProvider::new(AnthropicConfig::new(model), self.credentials.clone())?;
        Ok(Arc::new(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_map_to_ollama_tags() {
        assert_eq!(
            ollama_models("default").unwrap(),
            ("qwen3:4b".into(), Some("qwen3-vl:2b".into()))
        );
        assert_eq!(
            ollama_models("low-memory").unwrap(),
            ("qwen3:1.7b".into(), None)
        );
        assert_eq!(
            ollama_models("nope").unwrap_err(),
            LlmError::Config("unknown_profile")
        );
    }
}
