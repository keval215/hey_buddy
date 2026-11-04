//! The closed `plan/v1` JSON schema sent as the model's output format (Ollama `format`; in the
//! prompt for Anthropic). It mirrors `policy::plan` (ADR 0003 section 4) but is a flat, simple
//! shape small local models handle reliably: one step object whose `op` is a closed enum and whose
//! other fields are optional. Per-op requirements are NOT expressed here: the engine's light
//! pre-check and, above all, core's `policy::validate` enforce them. `additionalProperties: false`
//! everywhere, so there is no tier, approval, url, path, argument or command field.

use std::sync::OnceLock;

use serde_json::Value;

/// The schema text (compact JSON). This is the constant; [`plan_v1_schema`] parses it once.
pub const PLAN_V1_SCHEMA_JSON: &str = r#"{"type":"object","additionalProperties":false,"required":["steps"],"properties":{"steps":{"type":"array","maxItems":12,"items":{"type":"object","additionalProperties":false,"required":["op"],"properties":{"op":{"type":"string","enum":["observe","find_text","answer_from_screen","scroll","focus_window","open_app","click","type","submit","key_combo","copy_selection"]},"query":{"type":"string","minLength":1,"maxLength":200},"direction":{"type":"string","enum":["up","down"]},"amount":{"type":"integer","minimum":1,"maximum":10},"app":{"type":"string","minLength":1,"maxLength":64},"target":{"type":"object","additionalProperties":false,"required":["label"],"properties":{"role":{"type":"string","enum":["window","button","link","menu_item","tab","checkbox","radio_button","combo_box","text_field","static_text","heading","image","list","list_item","table","toolbar","group","web_area","opaque","other"]},"label":{"type":"string","minLength":1,"maxLength":120},"nth":{"type":"integer","minimum":1,"maximum":5}}},"text":{"type":"string","minLength":1,"maxLength":1000},"keys":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"string","minLength":1,"maxLength":16}}}}},"cannot":{"type":["string","null"],"maxLength":32}}}"#;

static SCHEMA: OnceLock<Value> = OnceLock::new();

/// The parsed schema (built once from [`PLAN_V1_SCHEMA_JSON`]).
pub fn plan_v1_schema() -> &'static Value {
    SCHEMA.get_or_init(|| {
        // The constant is valid JSON (unit-tested); an empty object is the fail-safe.
        serde_json::from_str(PLAN_V1_SCHEMA_JSON).unwrap_or(Value::Null)
    })
}
