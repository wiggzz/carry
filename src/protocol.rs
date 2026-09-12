use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Step {
    pub action: Action,
    pub context: ContextManagement,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Action {
    pub kind: ActionKind,
    pub command: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
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
    #[serde(rename = "protected")]
    pub keep: Vec<u64>,
    #[serde(rename = "removable")]
    pub drop: Vec<u64>,
    pub remember: Vec<String>,
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
                if self.answer.as_deref().is_none_or(str::is_empty)
                    || self.command.is_some()
                    || self.message.is_some()
                {
                    bail!("finish action requires answer and no command or message");
                }
            }
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
struct ShellArguments {
    command: String,
    #[serde(default)]
    message: Option<String>,
    context: ContextManagement,
}

#[derive(Deserialize, Serialize)]
struct FinishArguments {
    answer: String,
    context: ContextManagement,
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
                        message: args.message.filter(|message| !message.trim().is_empty()),
                        answer: None,
                    },
                    context: args.context,
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
                        message: None,
                        answer: Some(args.answer),
                    },
                    context: args.context,
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
                    message: self.action.message.clone(),
                    context: self.context.clone(),
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
                    context: self.context.clone(),
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

fn context_schema() -> Value {
    json!({
        "type": "object",
        "description": "After choosing the next action, manage recent context. User messages start protected; mark one removable only when deliberately safe. Stable context stays by default; volatile context may compact under budget pressure.",
        "properties": {
            "protected": {
                "type": "array",
                "description": "IDs that must remain. Use for volatile context whose information is not preserved elsewhere. Reverses removable.",
                "items": { "type": "integer", "minimum": 1 },
                "maxItems": 4
            },
            "removable": {
                "type": "array",
                "description": "IDs safe to remove because they add no needed information or remember preserves it. Reverses protected.",
                "items": { "type": "integer", "minimum": 1 },
                "maxItems": 4
            },
            "remember": {
                "type": "array",
                "description": "One concise fact to preserve before removing a bulky source. Do not include chain-of-thought.",
                "items": { "type": "string" },
                "maxItems": 1
            }
        },
        "required": ["protected", "removable", "remember"],
        "additionalProperties": false
    })
}

pub fn tool_definitions() -> Value {
    let context = context_schema();
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
                    "message": {
                        "type": ["string", "null"],
                        "description": "Optional concise commentary shown before the command, explaining what is being done and why."
                    },
                    "context": context.clone()
                },
                "required": ["command", "message", "context"],
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
                    "context": context
                },
                "required": ["answer", "context"],
                "additionalProperties": false
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_schema_uses_learning_framed_protected_and_removable_names() {
        let schema = tool_definitions();
        let context = &schema[0]["parameters"]["properties"]["context"];
        let description = context["description"].as_str().unwrap();
        let protected = context["properties"]["protected"]["description"]
            .as_str()
            .unwrap();
        let removable = context["properties"]["removable"]["description"]
            .as_str()
            .unwrap();
        let remember = context["properties"]["remember"]["description"]
            .as_str()
            .unwrap();

        assert!(
            !context["properties"]
                .as_object()
                .unwrap()
                .contains_key("keep")
        );
        assert!(
            !context["properties"]
                .as_object()
                .unwrap()
                .contains_key("drop")
        );
        assert_eq!(
            context["required"],
            json!(["protected", "removable", "remember"])
        );
        assert_eq!(
            description,
            "After choosing the next action, manage recent context. User messages start protected; mark one removable only when deliberately safe. Stable context stays by default; volatile context may compact under budget pressure."
        );
        assert_eq!(
            protected,
            "IDs that must remain. Use for volatile context whose information is not preserved elsewhere. Reverses removable."
        );
        assert_eq!(
            removable,
            "IDs safe to remove because they add no needed information or remember preserves it. Reverses protected."
        );
        assert_eq!(
            remember,
            "One concise fact to preserve before removing a bulky source. Do not include chain-of-thought."
        );
    }

    #[test]
    fn parses_shell_and_finish_function_calls() {
        let shell = json!({
            "type": "function_call",
            "name": "shell",
            "call_id": "call_1",
            "arguments": r#"{"command":"cargo test","message":"Checking the focused tests first.","context":{"protected":[1],"removable":[],"remember":[]}}"#
        });
        let parsed = Step::from_function_call(&shell).unwrap();
        assert_eq!(parsed.action.kind, ActionKind::Shell);
        assert_eq!(parsed.action.command.as_deref(), Some("cargo test"));
        assert_eq!(
            parsed.action.message.as_deref(),
            Some("Checking the focused tests first.")
        );
        assert_eq!(parsed.context.keep, vec![1]);
        let schema = tool_definitions();
        assert_eq!(
            schema[0]["parameters"]["properties"]["context"]["properties"]["protected"]["items"]["type"],
            "integer"
        );
        assert!(
            schema[0]["parameters"]["properties"]
                .get("message")
                .is_some()
        );

        let finish = json!({
            "type": "function_call",
            "name": "finish",
            "call_id": "call_2",
            "arguments": r#"{"answer":"done","context":{"protected":[],"removable":[],"remember":[]}}"#
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
