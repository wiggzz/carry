use anyhow::{Context, Result, bail};
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
                if self.command.as_deref().is_none_or(str::is_empty) || self.answer.is_some() {
                    bail!("shell action requires command and no answer");
                }
            }
            ActionKind::Finish => {
                if self.answer.as_deref().is_none_or(str::is_empty) || self.command.is_some() {
                    bail!("finish action requires answer and no command");
                }
            }
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
struct ShellArguments {
    command: String,
    context_management: ContextManagement,
}

#[derive(Deserialize, Serialize)]
struct FinishArguments {
    answer: String,
    context_management: ContextManagement,
}

impl Step {
    pub fn from_function_call(item: &Value) -> Result<Self> {
        let name = item["name"].as_str().context("function call has no name")?;
        let arguments = item["arguments"]
            .as_str()
            .context("function call has no string arguments")?;
        match name {
            "shell" => {
                let args: ShellArguments = serde_json::from_str(arguments)
                    .context("shell function arguments do not match the schema")?;
                if args.command.is_empty() {
                    bail!("shell command must not be empty");
                }
                Ok(Self {
                    action: Action {
                        kind: ActionKind::Shell,
                        command: Some(args.command),
                        answer: None,
                    },
                    context_management: args.context_management,
                })
            }
            "finish" => {
                let args: FinishArguments = serde_json::from_str(arguments)
                    .context("finish function arguments do not match the schema")?;
                if args.answer.is_empty() {
                    bail!("finish answer must not be empty");
                }
                Ok(Self {
                    action: Action {
                        kind: ActionKind::Finish,
                        command: None,
                        answer: Some(args.answer),
                    },
                    context_management: args.context_management,
                })
            }
            other => bail!("unknown function call {other}"),
        }
    }

    pub fn synthetic_function_call(&self, call_id: &str) -> Result<Value> {
        let (name, arguments) = match self.action.kind {
            ActionKind::Shell => (
                "shell",
                serde_json::to_string(&ShellArguments {
                    command: self
                        .action
                        .command
                        .clone()
                        .context("shell command missing")?,
                    context_management: self.context_management.clone(),
                })?,
            ),
            ActionKind::Finish => (
                "finish",
                serde_json::to_string(&FinishArguments {
                    answer: self
                        .action
                        .answer
                        .clone()
                        .context("finish answer missing")?,
                    context_management: self.context_management.clone(),
                })?,
            ),
        };
        Ok(json!({
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": arguments
        }))
    }
}

fn context_management_schema() -> Value {
    json!({
        "type": "object",
        "description": "Controls the complete persistent context for the next decision. Context not explicitly retained is lost and unavailable to future work.",
        "properties": {
            "retain_ids": {
                "type": "array",
                "description": "The complete set of existing context IDs to preserve verbatim. Include a t-prefixed tool-interaction ID to replay both its original assistant function call and its function result exactly. Include an m-prefixed memory ID to keep that memory. Omitted IDs are permanently dropped. The latest tool interaction is visible for the current decision without being listed, but must be listed here to remain visible afterward.",
                "items": { "type": "string" }
            },
            "add_memories": {
                "type": "array",
                "description": "Concise new conclusions worth preserving when verbatim evidence is unnecessary. The harness assigns each one an m-prefixed ID in the next request. Do not copy content already preserved through retain_ids.",
                "items": { "type": "string" }
            }
        },
        "required": ["retain_ids", "add_memories"],
        "additionalProperties": false
    })
}

pub fn tool_definitions() -> Value {
    let context = context_management_schema();
    json!([
        {
            "type": "function",
            "name": "shell",
            "description": "Run one noninteractive shell command in the assigned repository. Use it to inspect files, edit files, and run tests. The command runs through /bin/sh -lc with no stdin; stdout and stderr are returned in one function result. Commands must terminate on their own.",
            "strict": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The complete shell command to execute. It may contain pipes, conditionals, or a heredoc when useful."
                    },
                    "context_management": context.clone()
                },
                "required": ["command", "context_management"],
                "additionalProperties": false
            }
        },
        {
            "type": "function",
            "name": "finish",
            "description": "End the run only when the coding task is complete and relevant verification has passed, or when no further useful work is possible. No shell command is executed.",
            "strict": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "answer": {
                        "type": "string",
                        "description": "A concise final report of the completed work and verification, or the reason work cannot continue."
                    },
                    "context_management": context
                },
                "required": ["answer", "context_management"],
                "additionalProperties": false
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shell_and_finish_function_calls() {
        let shell = json!({
            "type": "function_call",
            "name": "shell",
            "call_id": "call_1",
            "arguments": r#"{"command":"cargo test","context_management":{"retain_ids":["t0001"],"add_memories":[]}}"#
        });
        let parsed = Step::from_function_call(&shell).unwrap();
        assert_eq!(parsed.action.kind, ActionKind::Shell);
        assert_eq!(parsed.action.command.as_deref(), Some("cargo test"));

        let finish = json!({
            "type": "function_call",
            "name": "finish",
            "call_id": "call_2",
            "arguments": r#"{"answer":"done","context_management":{"retain_ids":[],"add_memories":[]}}"#
        });
        assert_eq!(
            Step::from_function_call(&finish)
                .unwrap()
                .action
                .answer
                .as_deref(),
            Some("done")
        );
    }
}
