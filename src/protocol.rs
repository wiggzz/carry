use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Step {
    pub action: Action,
    pub context_management: ContextManagement,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Action {
    pub kind: ActionKind,
    pub command: Option<String>,
    pub answer: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Shell,
    Finish,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ContextManagement {
    pub retain_ids: Vec<String>,
    pub add_memories: Vec<String>,
}

impl Action {
    pub fn validate(&self) -> Result<()> {
        match self.kind {
            ActionKind::Shell => {
                if self.command.as_deref().is_none_or(str::is_empty) {
                    bail!("shell action requires a non-empty command");
                }
                if self.answer.is_some() {
                    bail!("shell action must set answer to null");
                }
            }
            ActionKind::Finish => {
                if self.answer.as_deref().is_none_or(str::is_empty) {
                    bail!("finish action requires a non-empty answer");
                }
                if self.command.is_some() {
                    bail!("finish action must set command to null");
                }
            }
        }
        Ok(())
    }
}

pub fn step_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["shell", "finish"] },
                    "command": { "type": ["string", "null"] },
                    "answer": { "type": ["string", "null"] }
                },
                "required": ["kind", "command", "answer"],
                "additionalProperties": false
            },
            "context_management": {
                "type": "object",
                "description": "Explicitly control which context items remain available after this response.",
                "properties": {
                    "retain_ids": {
                        "type": "array",
                        "description": "Complete set of existing retained-item and latest-tool-result IDs to include in the next step. Omitted existing IDs expire.",
                        "items": { "type": "string" }
                    },
                    "add_memories": {
                        "type": "array",
                        "description": "New concise durable conclusions to add as memory items. Do not copy tool output or text already retained by ID.",
                        "items": { "type": "string" }
                    }
                },
                "required": ["retain_ids", "add_memories"],
                "additionalProperties": false
            }
        },
        "required": ["action", "context_management"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_action_semantics() {
        let shell = Action {
            kind: ActionKind::Shell,
            command: Some("cargo test".into()),
            answer: None,
        };
        assert!(shell.validate().is_ok());

        let invalid = Action {
            kind: ActionKind::Finish,
            command: Some("true".into()),
            answer: Some("done".into()),
        };
        assert!(invalid.validate().is_err());
    }
}
