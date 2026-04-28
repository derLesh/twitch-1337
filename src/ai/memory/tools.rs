//! Tool-surface definitions for the memory subsystem.
//!
//! The extractor agent (per-turn) and the consolidator agent (daily) see
//! disjoint tool sets. The extractor can only save or inspect; destructive
//! operations (merge, drop, edit) are reserved for the consolidator so a
//! hijacked per-turn extraction cannot nuke user data.

use crate::ai::llm::ToolDefinition;

/// Tool set exposed to the per-turn extractor. Read + additive write only.
pub fn extractor_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "save_memory".into(),
            description: "Save or update a long-term fact. Use a short descriptive slug. \
                          For user/pref scopes, subject_id must equal the current speaker's user_id \
                          (regular users) or any user (moderator/broadcaster, except Pref stays self-only). \
                          Lore is moderator/broadcaster-only. Overwrites if a memory with the same \
                          (scope, subject_id, slug) already exists."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "enum": ["user", "lore", "pref"]},
                    "subject_id": {"type": "string", "description": "Twitch numeric user-id of the subject. Required for user/pref. Omit for lore."},
                    "slug": {"type": "string", "description": "Short slug identifier for this memory (lowercase, dashes)."},
                    "fact": {"type": "string", "description": "The fact to remember."}
                },
                "required": ["scope", "slug", "fact"]
            }),
        },
        ToolDefinition {
            name: "get_memories".into(),
            description:
                "Read-only listing of memories in a given scope, optionally filtered by subject_id."
                    .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "enum": ["user", "lore", "pref"]},
                    "subject_id": {"type": "string"}
                },
                "required": ["scope"]
            }),
        },
    ]
}

/// Tool set exposed to the daily consolidator. Destructive/editorial only.
pub fn consolidator_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "merge_memories".into(),
            description: "Combine 2+ memories of the same scope and subject into a single canonical entry with a new slug and fact. Metadata (sources, confidence, timestamps, access_count) is merged deterministically by the store.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "keys": {"type": "array", "items": {"type": "string"}, "minItems": 2},
                    "new_slug": {"type": "string"},
                    "new_fact": {"type": "string"}
                },
                "required": ["keys", "new_slug", "new_fact"]
            }),
        },
        ToolDefinition {
            name: "drop_memory".into(),
            description:
                "Remove a memory outright (contradicted, hallucinated, or stale beyond recovery)."
                    .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "key": {"type": "string"} },
                "required": ["key"]
            }),
        },
        ToolDefinition {
            name: "edit_memory".into(),
            description: "Refine a memory without merging. Apply signed confidence_delta in [-50, +30] (clamped to [0,100]), remove one source from provenance, or replace the fact wording. Do not use to change the subject or the core claim (that is merge or drop).".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "fact": {"type": "string"},
                    "confidence_delta": {"type": "integer", "minimum": -50, "maximum": 30},
                    "drop_source": {"type": "string", "description": "Exact username to remove from sources."}
                },
                "required": ["key"]
            }),
        },
        ToolDefinition {
            name: "get_memory".into(),
            description: "Read a single memory by key.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "key": {"type": "string"} },
                "required": ["key"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_names(tools: Vec<ToolDefinition>) -> Vec<String> {
        tools.into_iter().map(|t| t.name).collect()
    }

    #[test]
    fn extractor_tools_surface() {
        let names = tool_names(extractor_tools());
        assert_eq!(names, vec!["save_memory", "get_memories"]);
        assert!(!names.iter().any(|n| n == "delete_memory"));
        assert!(!names.iter().any(|n| n == "merge_memories"));
        assert!(!names.iter().any(|n| n == "edit_memory"));
        assert!(!names.iter().any(|n| n == "drop_memory"));
        assert!(!names.iter().any(|n| n == "web_search"));
        assert!(!names.iter().any(|n| n == "fetch_url"));
    }

    #[test]
    fn consolidator_tools_surface() {
        let names = tool_names(consolidator_tools());
        assert_eq!(
            names,
            vec!["merge_memories", "drop_memory", "edit_memory", "get_memory"]
        );
        assert!(!names.iter().any(|n| n == "save_memory"));
        assert!(!names.iter().any(|n| n == "web_search"));
        assert!(!names.iter().any(|n| n == "fetch_url"));
    }

    #[test]
    fn extractor_tool_parameters_require_scope_slug_fact() {
        // Regression guard: the permission-gated dispatcher parses these fields
        // by name, so their presence in the schema is load-bearing.
        let save = extractor_tools()
            .into_iter()
            .find(|t| t.name == "save_memory")
            .unwrap();
        let required = save.parameters["required"].as_array().unwrap();
        let required: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required.contains(&"scope"));
        assert!(required.contains(&"slug"));
        assert!(required.contains(&"fact"));
    }

    #[test]
    fn consolidator_tool_parameters_merge_required_fields() {
        let merge = consolidator_tools()
            .into_iter()
            .find(|t| t.name == "merge_memories")
            .expect("merge_memories must be present");
        let required: Vec<&str> = merge.parameters["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"keys"));
        assert!(required.contains(&"new_slug"));
        assert!(required.contains(&"new_fact"));
    }

    #[test]
    fn consolidator_tool_parameters_edit_required_key() {
        let edit = consolidator_tools()
            .into_iter()
            .find(|t| t.name == "edit_memory")
            .expect("edit_memory must be present");
        let required: Vec<&str> = edit.parameters["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"key"));
    }
}
