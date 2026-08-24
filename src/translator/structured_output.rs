use serde_json::{Map, Value, json};

use super::TranslationRequest;

pub fn openai_translation_schema(request: &TranslationRequest) -> Value {
    let mut schema = translation_schema(request);
    let count = request.segments.len();
    schema["properties"]["translations"]["minItems"] = Value::from(count);
    schema["properties"]["translations"]["maxItems"] = Value::from(count);
    schema
}

pub fn translation_schema(request: &TranslationRequest) -> Value {
    let ids = request
        .segments
        .iter()
        .map(|segment| Value::from(segment.id))
        .collect::<Vec<_>>();

    json!({
        "type": "object",
        "properties": {
            "translations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "integer", "enum": ids},
                        "text": {"type": "string"},
                    },
                    "required": ["id", "text"],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["translations"],
        "additionalProperties": false,
    })
}

pub fn rejects_structured_output_field(status: u16, body: &[u8], field: &str) -> bool {
    matches!(status, 400 | 422)
        && serde_json::from_slice::<Value>(body)
            .is_ok_and(|value| value_rejects_field(&value, field))
}

fn value_rejects_field(value: &Value, field: &str) -> bool {
    match value {
        Value::Array(values) => values.iter().any(|value| value_rejects_field(value, field)),
        Value::Object(object) => {
            object_rejects_field(object, field)
                || object
                    .values()
                    .any(|value| value_rejects_field(value, field))
        }
        _ => false,
    }
}

fn object_rejects_field(object: &Map<String, Value>, field: &str) -> bool {
    object_mentions_field(object, field)
        && !object_mentions_schema(object)
        && (["code", "type"]
            .iter()
            .filter_map(|key| object.get(*key).and_then(Value::as_str))
            .any(contains_rejection_marker)
            || ["message", "msg", "detail"]
                .iter()
                .filter_map(|key| object.get(*key).and_then(Value::as_str))
                .any(|text| text_rejects_field(text, field)))
}

fn object_mentions_field(object: &Map<String, Value>, field: &str) -> bool {
    object
        .get("param")
        .is_some_and(|value| value_mentions_field(value, field))
        || object
            .get("loc")
            .is_some_and(|value| value_mentions_field(value, field))
}

fn value_mentions_field(value: &Value, field: &str) -> bool {
    match value {
        Value::String(value) => field_path_matches(value, field),
        Value::Array(values) => values
            .iter()
            .any(|value| value_mentions_field(value, field)),
        _ => false,
    }
}

fn field_path_matches(value: &str, field: &str) -> bool {
    value == field
        || value
            .strip_prefix(field)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('['))
        || value
            .strip_prefix('/')
            .is_some_and(|value| field_path_matches(value, field))
}

fn object_mentions_schema(object: &Map<String, Value>) -> bool {
    object.values().any(value_mentions_schema)
}

fn value_mentions_schema(value: &Value) -> bool {
    match value {
        Value::String(value) => value.to_ascii_lowercase().contains("schema"),
        Value::Array(values) => values.iter().any(value_mentions_schema),
        Value::Object(object) => object.values().any(value_mentions_schema),
        _ => false,
    }
}

fn text_rejects_field(text: &str, field: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains(&field.to_ascii_lowercase())
        && !text.contains("schema")
        && contains_rejection_marker(&text)
}

fn contains_rejection_marker(value: &str) -> bool {
    [
        "unsupported parameter",
        "unsupported request parameter",
        "unknown parameter",
        "unknown field",
        "unknown argument",
        "unrecognized parameter",
        "unrecognized request argument",
        "unrecognized argument",
        "unexpected parameter",
        "unexpected field",
        "extra field",
        "extra fields",
        "extra input",
        "extra inputs",
        "extra_forbidden",
        "additional properties are not allowed",
    ]
    .iter()
    .any(|marker| value.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::LanguageCode, translator::TranslationItem};

    fn request() -> TranslationRequest {
        TranslationRequest {
            source_language: LanguageCode::parse("ja").unwrap(),
            target_language: LanguageCode::parse("zh-CN").unwrap(),
            previous_context: vec![TranslationItem {
                id: 99,
                text: "before".into(),
            }],
            segments: vec![
                TranslationItem {
                    id: 101,
                    text: "one".into(),
                },
                TranslationItem {
                    id: 102,
                    text: "two".into(),
                },
            ],
            next_context: vec![TranslationItem {
                id: 103,
                text: "after".into(),
            }],
        }
    }

    #[test]
    fn openai_schema_limits_output_to_segment_entries() {
        let schema = openai_translation_schema(&request());
        let translations = &schema["properties"]["translations"];
        let item = &translations["items"];

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], serde_json::json!(["translations"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(translations["minItems"], 2);
        assert_eq!(translations["maxItems"], 2);
        assert_eq!(item["required"], serde_json::json!(["id", "text"]));
        assert_eq!(item["additionalProperties"], false);
        assert_eq!(
            item["properties"]["id"]["enum"],
            serde_json::json!([101, 102])
        );
    }

    #[test]
    fn anthropic_schema_omits_unsupported_array_limits() {
        let translations = &translation_schema(&request())["properties"]["translations"];

        assert!(translations.get("minItems").is_none());
        assert!(translations.get("maxItems").is_none());
    }

    #[test]
    fn detects_explicit_structured_output_field_rejections() {
        assert!(rejects_structured_output_field(
            400,
            br#"{"error":{"message":"Unsupported parameter: response_format","param":"response_format","code":"unsupported_parameter"}}"#,
            "response_format",
        ));
        assert!(rejects_structured_output_field(
            422,
            br#"{"detail":[{"type":"extra_forbidden","loc":["body","output_config"],"msg":"Extra inputs are not permitted"}]}"#,
            "output_config",
        ));
    }

    #[test]
    fn ignores_non_field_and_schema_errors() {
        assert!(!rejects_structured_output_field(
            401,
            br#"{"error":{"message":"Unsupported parameter: response_format"}}"#,
            "response_format",
        ));
        assert!(!rejects_structured_output_field(
            400,
            br#"{"error":{"message":"Unsupported parameter: model","param":"model"}}"#,
            "response_format",
        ));
        assert!(!rejects_structured_output_field(
            400,
            br#"{"error":{"message":"Invalid schema for response_format: minItems is not supported","param":"response_format","code":"invalid_json_schema"}}"#,
            "response_format",
        ));
        assert!(!rejects_structured_output_field(
            400,
            br#"{"error":{"message":"response_format does not support minItems","param":"response_format"}}"#,
            "response_format",
        ));
        assert!(!rejects_structured_output_field(
            422,
            br#"{"detail":[{"type":"extra_forbidden","loc":["body","output_config","format","schema","properties","translations","maxItems"],"msg":"Extra inputs are not permitted"}]}"#,
            "output_config",
        ));
        assert!(!rejects_structured_output_field(
            400,
            b"not JSON",
            "response_format",
        ));
    }
}
