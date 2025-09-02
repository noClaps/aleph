use anyhow::Result;
use serde_json::Value;

use crate::LanguageModelToolSchemaFormat;

/// Tries to adapt a JSON schema representation to be compatible with the specified format.
///
/// If the json cannot be made compatible with the specified format, an error is returned.
pub fn adapt_schema_to_format(
    json: &mut Value,
    format: LanguageModelToolSchemaFormat,
) -> Result<()> {
    if let Value::Object(obj) = json {
        obj.remove("$schema");
        obj.remove("title");
    }

    match format {
        LanguageModelToolSchemaFormat::JsonSchema => preprocess_json_schema(json),
        LanguageModelToolSchemaFormat::JsonSchemaSubset => adapt_to_json_schema_subset(json),
    }
}

fn preprocess_json_schema(json: &mut Value) -> Result<()> {
    // `additionalProperties` defaults to `false` unless explicitly specified.
    // This prevents models from hallucinating tool parameters.
    if let Value::Object(obj) = json
        && matches!(obj.get("type"), Some(Value::String(s)) if s == "object")
    {
        if !obj.contains_key("additionalProperties") {
            obj.insert("additionalProperties".to_string(), Value::Bool(false));
        }

        // OpenAI API requires non-missing `properties`
        if !obj.contains_key("properties") {
            obj.insert("properties".to_string(), Value::Object(Default::default()));
        }
    }
    Ok(())
}

/// Tries to adapt the json schema so that it is compatible with https://ai.google.dev/api/caching#Schema
fn adapt_to_json_schema_subset(json: &mut Value) -> Result<()> {
    if let Value::Object(obj) = json {
        const UNSUPPORTED_KEYS: [&str; 4] = ["if", "then", "else", "$ref"];

        for key in UNSUPPORTED_KEYS {
            anyhow::ensure!(
                !obj.contains_key(key),
                "Schema cannot be made compatible because it contains \"{key}\""
            );
        }

        const KEYS_TO_REMOVE: [(&str, fn(&Value) -> bool); 5] = [
            ("format", |value| value.is_string()),
            ("additionalProperties", |value| value.is_boolean()),
            ("exclusiveMinimum", |value| value.is_number()),
            ("exclusiveMaximum", |value| value.is_number()),
            ("optional", |value| value.is_boolean()),
        ];
        for (key, predicate) in KEYS_TO_REMOVE {
            if let Some(value) = obj.get(key)
                && predicate(value)
            {
                obj.remove(key);
            }
        }

        // If a type is not specified for an input parameter, add a default type
        if matches!(obj.get("description"), Some(Value::String(_)))
            && !obj.contains_key("type")
            && !(obj.contains_key("anyOf")
                || obj.contains_key("oneOf")
                || obj.contains_key("allOf"))
        {
            obj.insert("type".to_string(), Value::String("string".to_string()));
        }

        // Handle oneOf -> anyOf conversion
        if let Some(subschemas) = obj.get_mut("oneOf")
            && subschemas.is_array()
        {
            let subschemas_clone = subschemas.clone();
            obj.remove("oneOf");
            obj.insert("anyOf".to_string(), subschemas_clone);
        }

        // Recursively process all nested objects and arrays
        for (_, value) in obj.iter_mut() {
            if let Value::Object(_) | Value::Array(_) = value {
                adapt_to_json_schema_subset(value)?;
            }
        }
    } else if let Value::Array(arr) = json {
        for item in arr.iter_mut() {
            adapt_to_json_schema_subset(item)?;
        }
    }
    Ok(())
}
