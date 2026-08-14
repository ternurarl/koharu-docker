use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use koharu_scene::{Authored, LanguageTag, Origin, SourceText, Translation};
use koharu_translator::{TranslationRequest, Translator};

use crate::TranslationConfig;

use super::{StageInput, StageProcessor, finish, generation};

const PRODUCER: &str = "dev.koharu.pipeline.translation";

/// Maximum concurrent per-bubble translation requests per page.
const TRANSLATION_CONCURRENCY: usize = 4;

pub(super) struct Processor {
    config: TranslationConfig,
    translator: Translator,
}

impl Processor {
    pub(super) fn new(config: TranslationConfig, translator: Translator) -> Self {
        Self { config, translator }
    }
}

#[async_trait]
impl StageProcessor for Processor {
    fn model(&self) -> &'static str {
        Translator::model(&self.config.model)
    }

    fn unload(&self) -> bool {
        self.translator.unload()
    }

    async fn load(&self) -> Result<()> {
        self.translator.load_model(&self.config.model).await
    }

    async fn process(&self, input: StageInput) -> Result<koharu_scene::Patch> {
        let mut targets = Vec::new();
        if let Some(group) = input.scene.page(input.page)?.text_group()? {
            for layer in group.text_layers()? {
                if !input.contains_entity(layer.id())? {
                    continue;
                }
                let content = layer.content()?;
                let Some(source) = content.source()? else {
                    continue;
                };
                if !source.text.value.trim().is_empty() {
                    targets.push((content.id(), source.text.value));
                }
            }
        }
        // Translate each bubble through its own request so that a text-heavy
        // page is not gated by one long generation. Requests run with bounded
        // concurrency; a single failing bubble fails the stage, matching the
        // previous whole-page behavior. Vision pages attach the page image to
        // every bubble request (costly but correct).
        let vision = Translator::supports_vision(&self.config.model);
        let image = if vision {
            input.images.get(&input.scene, input.page, "source").await?
        } else {
            None
        };
        let instructions = self.config.instructions.as_deref();
        let mut requests = Vec::new();
        for (_, source) in &targets {
            let mut request =
                TranslationRequest::new([source.clone()], self.config.target_language);
            if let Some(instructions) = instructions {
                request = request.with_instructions(instructions);
            }
            if let Some(image) = &image {
                request = request.with_image(Arc::clone(image));
            }
            requests.push(
                self.translator
                    .translate(&self.config.model, self.config.generation, request),
            );
        }
        let results: Vec<_> = futures::stream::iter(requests)
            .buffer_unordered(TRANSLATION_CONCURRENCY)
            .collect()
            .await;
        let provider = results
            .iter()
            .find_map(|result| result.as_ref().ok().map(|(provider, _)| *provider))
            .unwrap_or_else(|| Translator::model(&self.config.model));
        let language = LanguageTag::new(self.config.target_language.tag())?;
        let generated = generation(PRODUCER, provider)?;
        let mut edit = input.scene.edit_as(generated.clone());
        for (entity, _) in &targets {
            edit.observe::<SourceText>(*entity)?;
            edit.observe::<Translation>(*entity)?;
        }
        for ((entity, source), result) in targets.into_iter().zip(results) {
            if input
                .scene
                .component::<Translation>(entity)?
                .is_some_and(|value| matches!(value.text.origin, Origin::User))
            {
                continue;
            }
            let (_, translated) = result?;
            let text = if source.trim() == "\u{2026}" {
                "\u{2026}".to_owned()
            } else {
                translated.into_iter().next().unwrap_or_default()
            };
            edit.set(
                entity,
                &Translation {
                    text: Authored::generated(text, generated.clone()),
                    language: Some(language.clone()),
                },
            )?;
        }
        finish(edit)
    }
}
