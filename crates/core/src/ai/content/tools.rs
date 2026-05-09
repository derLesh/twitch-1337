use llm::ToolDefinition;

const WEB_TOOL_NAMES: &[&str] = &["web_search", "read_url"];

pub fn is_web_tool(name: &str) -> bool {
    WEB_TOOL_NAMES.contains(&name)
}

pub fn ai_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "web_search".into(),
            description:
                "Search the web for current information and return concise results with URLs."
                    .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": 10}
                },
                "required": ["query"]
            }),
        },
        ToolDefinition::derived::<super::executor::ReadUrlArgs>(
            "read_url",
            "Fetch a URL and return a textual answer. Pass an optional `instruction` to focus the answer; without one a full description is returned. Handles HTML, plain text, JSON, images, PDFs, audio, and video.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_names() -> Vec<String> {
        ai_tools().into_iter().map(|t| t.name).collect()
    }

    #[test]
    fn ai_tools_surface_contains_search_and_read() {
        let names = tool_names();
        assert_eq!(names, vec!["web_search", "read_url"]);
    }

    #[test]
    fn read_url_does_not_appear_under_old_name() {
        let names = tool_names();
        assert!(!names.iter().any(|n| n == "fetch_url"));
    }
}
