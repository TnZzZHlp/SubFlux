use serde::Serialize;

use crate::{
    error::{AppError, Result},
    translator::TranslationItem,
};

use super::TranslationRequest;

pub fn system_prompt(request: &TranslationRequest) -> String {
    instruction(request, "")
}

pub fn correction_prompt(request: &TranslationRequest) -> String {
    instruction(request, "A prior response was invalid. ")
}

fn instruction(request: &TranslationRequest, prefix: &str) -> String {
    let ids = request
        .segments
        .iter()
        .map(|segment| segment.id.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{prefix}Translate subtitle text from {} to {}. The user message is a JSON object with \
         previous_context, segments, and next_context. previous_context and next_context are \
         read-only context to help comprehension: never translate or return either context list. \
         Translate only segments. Return JSON only, with exactly this object shape: \
         {{\"translations\":[{{\"id\":123,\"text\":\"translated text\"}}]}}. Return exactly {} translations \
         in this order with IDs [{ids}]. Preserve every segments id exactly once; do not add, \
         remove, modify, merge, or split entries. Do not return explanations or Markdown. \
         Translate only text; formatting tags are intentionally absent from the input and must not \
         be invented.",
        request.source_language,
        request.target_language,
        request.segments.len(),
    )
}

pub fn user_payload(request: &TranslationRequest) -> Result<String> {
    #[derive(Serialize)]
    struct Payload<'a> {
        source_language: &'a str,
        target_language: &'a str,
        previous_context: &'a [TranslationItem],
        segments: &'a [TranslationItem],
        next_context: &'a [TranslationItem],
    }

    serde_json::to_string(&Payload {
        source_language: request.source_language.as_str(),
        target_language: request.target_language.as_str(),
        previous_context: &request.previous_context,
        segments: &request.segments,
        next_context: &request.next_context,
    })
    .map_err(|error| {
        AppError::TranslationError(format!("could not encode translation request: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use crate::{config::LanguageCode, translator::TranslationItem};

    use super::*;

    fn request() -> TranslationRequest {
        TranslationRequest {
            source_language: LanguageCode::parse("ja").unwrap(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            previous_context: vec![TranslationItem {
                id: 100,
                text: "before".into(),
            }],
            segments: vec![
                TranslationItem {
                    id: 101,
                    text: "translate".into(),
                },
                TranslationItem {
                    id: 102,
                    text: "also translate".into(),
                },
            ],
            next_context: vec![TranslationItem {
                id: 103,
                text: "after".into(),
            }],
        }
    }

    #[test]
    fn payload_contains_languages_context_and_segments_without_timeline_data() {
        let payload: Value = serde_json::from_str(&user_payload(&request()).unwrap()).unwrap();
        assert_eq!(payload["source_language"], "ja");
        assert_eq!(payload["target_language"], "zh-CN");
        assert_eq!(payload["previous_context"][0]["id"], 100);
        assert_eq!(payload["segments"][0]["id"], 101);
        assert_eq!(payload["next_context"][0]["id"], 103);
        assert!(payload["segments"][0].get("start_ms").is_none());
        assert!(payload["segments"][0].get("end_ms").is_none());
    }

    #[test]
    fn system_prompt_limits_translation_to_segments() {
        let prompt = system_prompt(&request());
        assert!(prompt.contains("Translate only segments"));
        assert!(prompt.contains("exactly 2 translations in this order with IDs [101, 102]"));
        assert!(prompt.contains("never translate or return either context list"));
        assert!(prompt.contains(r#"{"translations":[{"id":123,"text":"translated text"}]}"#));
    }

    #[test]
    fn correction_prompt_repeats_expected_ids_without_response_content() {
        let prompt = correction_prompt(&request());
        assert!(prompt.contains("A prior response was invalid"));
        assert!(prompt.contains("exactly 2 translations in this order with IDs [101, 102]"));
        assert!(!prompt.contains("before"));
        assert!(!prompt.contains("translate\""));
    }
}
