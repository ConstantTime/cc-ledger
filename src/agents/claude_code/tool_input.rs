//! Pure: extract the file path that a Claude Code file-editing tool touches.
//!
//! Returns `None` for non-file tools (Bash, Read, Glob, …) so the file-edit
//! handlers can soft-skip them.

use serde_json::Value;

/// Resolve the file path a tool call writes to, by tool name.
pub fn affected_file(tool_name: &str, tool_input: &Value) -> Option<String> {
    let key = match tool_name {
        "Edit" | "Write" | "MultiEdit" => "file_path",
        "NotebookEdit" => "notebook_path",
        _ => return None,
    };
    tool_input
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn edit_extracts_file_path() {
        let v = json!({"file_path": "/a.rs", "old_string": "x", "new_string": "y"});
        assert_eq!(affected_file("Edit", &v), Some("/a.rs".into()));
    }

    #[test]
    fn write_extracts_file_path() {
        let v = json!({"file_path": "/a.rs", "content": "..."});
        assert_eq!(affected_file("Write", &v), Some("/a.rs".into()));
    }

    #[test]
    fn multi_edit_extracts_file_path() {
        let v = json!({"file_path": "/a.rs", "edits": []});
        assert_eq!(affected_file("MultiEdit", &v), Some("/a.rs".into()));
    }

    #[test]
    fn notebook_edit_extracts_notebook_path() {
        let v = json!({"notebook_path": "/n.ipynb"});
        assert_eq!(affected_file("NotebookEdit", &v), Some("/n.ipynb".into()));
    }

    #[test]
    fn bash_returns_none() {
        assert!(affected_file("Bash", &json!({"command": "ls"})).is_none());
    }

    #[test]
    fn read_returns_none_even_with_file_path() {
        assert!(affected_file("Read", &json!({"file_path": "/a.rs"})).is_none());
    }

    #[test]
    fn missing_field_returns_none() {
        assert!(affected_file("Edit", &json!({})).is_none());
    }
}
