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
    #[serde(default)]
    pub timeout_secs: Option<u64>,
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
                if self
                    .timeout_secs
                    .is_some_and(|secs| !(1..=300).contains(&secs))
                {
                    bail!("shell timeout_secs must be between 1 and 300 seconds");
                }
            }
            ActionKind::Finish => {
                if self.answer.as_deref().is_none_or(str::is_empty)
                    || self.command.is_some()
                    || self.message.is_some()
                    || self.timeout_secs.is_some()
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
    #[serde(default)]
    timeout_secs: Option<u64>,
    context: ContextManagement,
}

#[derive(Deserialize, Serialize)]
struct FinishArguments {
    answer: String,
    context: ContextManagement,
}

impl Step {
    pub fn from_function_call_for_mode(item: &Value, enabled: bool) -> Result<Self> {
        if enabled {
            return Self::from_function_call(item);
        }
        let mut call = item.clone();
        let mut args: Value = serde_json::from_str(
            item["arguments"]
                .as_str()
                .context("function call has no string arguments")?,
        )?;
        let object = args
            .as_object_mut()
            .context("function arguments must be an object")?;
        if object.contains_key("context") {
            bail!("context field is not available with model context management disabled");
        }
        object.insert(
            "context".into(),
            serde_json::to_value(ContextManagement::default())?,
        );
        call["arguments"] = json!(args.to_string());
        Self::from_function_call(&call)
    }

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
                let action = Action {
                    kind: ActionKind::Shell,
                    command: Some(args.command),
                    message: args.message.filter(|message| !message.trim().is_empty()),
                    timeout_secs: args.timeout_secs,
                    answer: None,
                };
                action.validate()?;
                Ok(Self {
                    action,
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
                        timeout_secs: None,
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
                    timeout_secs: self.action.timeout_secs,
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

    pub fn synthetic_function_call_for_mode(&self, call_id: &str, enabled: bool) -> Result<Value> {
        let mut call = self.synthetic_function_call(call_id)?;
        if !enabled {
            let mut args: Value = serde_json::from_str(call["arguments"].as_str().unwrap())?;
            args.as_object_mut().unwrap().remove("context");
            call["arguments"] = json!(args.to_string());
        }
        Ok(call)
    }
}

fn context_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "protected": {
                "type": "array",
                "description": "Protect up to four context item IDs that contained useful learnings or exact facts which need to be preserved. Items which are unprotected may be removed immediately by compaction, so ensure that you protect items if you need them. Human-authored content and memories will be retained by default, so there is no need to protect them.",
                "items": { "type": "integer", "minimum": 1 },
                "maxItems": 4
            },
            "removable": {
                "type": "array",
                "description": "Mark up to four context item IDs as removable when their information is no longer needed or is kept elsewhere. Marking an ID removable reverses protection. Consider removing redundant items when the learnings from the associated text, function call or function output is preserved elsewhere.",
                "items": { "type": "integer", "minimum": 1 },
                "maxItems": 4
            },
            "remember": {
                "type": "array",
                "description": "Add a concise additional fact to be preserved. Do this if you see a large item that is not useful verbatim, and any learnings from it are expected to remain useful for a long time and would be better preserved by a concise memory rather than the original assistant response, function call and function output. In that case, leave the item unprotected (or, mark it removable if it was previously protected) and capture its learnings as a memory.",
                "items": { "type": "string" },
                "maxItems": 1
            }
        },
        "required": ["protected", "removable", "remember"],
        "additionalProperties": false
    })
}

pub fn tool_definitions(default_shell_timeout_secs: u64) -> Value {
    let context = context_schema();
    json!([
        {
            "type": "function",
            "name": "shell",
            "description": format!("Run one noninteractive shell command in the working directory. The command runs through /bin/sh -lc with no stdin; stdout and stderr are returned in one function result. Set timeout_secs for commands that need a shorter or longer deadline; null uses this session's default of {default_shell_timeout_secs} seconds. Commands must terminate on their own."),
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
                    "timeout_secs": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "maximum": 300,
                        "description": format!("Wall-clock timeout for this command in seconds (1-300). Use null for this session's default of {default_shell_timeout_secs} seconds; request a longer timeout when a build or test needs it.")
                    },
                    "context": context.clone()
                },
                "required": ["command", "message", "timeout_secs", "context"],
                "additionalProperties": false
            }
        },
        {
            "type": "function",
            "name": "finish",
            "description": "End the run only when the requested task is complete and relevant verification has passed, or when no further useful work is possible. No shell command is executed.",
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

pub fn tool_definitions_for_mode(default_shell_timeout_secs: u64, enabled: bool) -> Value {
    let mut tools = tool_definitions(default_shell_timeout_secs);
    if !enabled {
        for tool in tools.as_array_mut().unwrap() {
            let params = &mut tool["parameters"];
            params["properties"]
                .as_object_mut()
                .unwrap()
                .remove("context");
            params["required"]
                .as_array_mut()
                .unwrap()
                .retain(|field| field != "context");
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_free_wire_contract_has_only_action_fields() {
        let tools = tool_definitions_for_mode(60, false);
        for tool in tools.as_array().unwrap() {
            let params = &tool["parameters"];
            assert!(params["properties"].get("context").is_none());
            assert!(
                !params["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("context"))
            );
            assert_eq!(params["additionalProperties"], false);
        }
        assert!(
            tool_definitions_for_mode(60, true)[0]["parameters"]["properties"]
                .get("context")
                .is_some()
        );
        for (name, arguments) in [
            (
                "shell",
                json!({"command":"true", "message":null, "timeout_secs":null}),
            ),
            ("finish", json!({"answer":"done"})),
        ] {
            let call = json!({"name":name, "arguments":arguments.to_string()});
            let step = Step::from_function_call_for_mode(&call, false).unwrap();
            assert_eq!(step.context, ContextManagement::default());
            let echo = step
                .synthetic_function_call_for_mode("call-2", false)
                .unwrap();
            let echoed: Value = serde_json::from_str(echo["arguments"].as_str().unwrap()).unwrap();
            assert!(echoed.get("context").is_none());
            assert_eq!(echoed, arguments);
            let mut invalid = arguments;
            invalid["context"] = json!({"protected":[],"removable":[],"remember":[]});
            let invalid_call = json!({"name":name, "arguments":invalid.to_string()});
            assert!(Step::from_function_call_for_mode(&invalid_call, false).is_err());
            assert!(Step::from_function_call_for_mode(&call, true).is_err());
        }
    }

    #[test]
    fn context_schema_keeps_human_content_by_default() {
        let schema = tool_definitions(60);
        let context = &schema[0]["parameters"]["properties"]["context"];
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
    }

    #[test]
    fn shell_timeout_is_model_visible_and_survives_function_call_roundtrip() {
        let schema = tool_definitions(60);
        let shell = &schema[0]["parameters"];
        assert_eq!(
            shell["properties"]["timeout_secs"]["type"],
            json!(["integer", "null"])
        );
        assert_eq!(shell["properties"]["timeout_secs"]["minimum"], 1);
        assert_eq!(shell["properties"]["timeout_secs"]["maximum"], 300);
        assert!(
            shell["required"]
                .as_array()
                .unwrap()
                .contains(&json!("timeout_secs"))
        );

        let call = json!({
            "type": "function_call", "name": "shell", "call_id": "call_1",
            "arguments": r#"{"command":"sleep 3","timeout_secs":1,"message":null,"context":{"protected":[],"removable":[],"remember":[]}}"#
        });
        let step = Step::from_function_call(&call).unwrap();
        assert_eq!(
            serde_json::to_value(&step).unwrap()["action"]["timeout_secs"],
            1
        );
        let echoed = step.synthetic_function_call("call_2").unwrap();
        let echoed_args: Value =
            serde_json::from_str(echoed["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(echoed_args["timeout_secs"], 1);
    }

    #[test]
    fn shell_timeout_rejects_out_of_range_model_values_and_accepts_legacy_calls() {
        for seconds in [0, 301] {
            let call = json!({
                "name": "shell", "arguments": json!({
                    "command": "true", "timeout_secs": seconds,
                    "context": {"protected": [], "removable": [], "remember": []}
                }).to_string()
            });
            assert!(Step::from_function_call(&call).is_err());
        }
        let legacy = json!({
            "name": "shell", "arguments": r#"{"command":"true","context":{"protected":[],"removable":[],"remember":[]}}"#
        });
        let step = Step::from_function_call(&legacy).unwrap();
        assert!(serde_json::to_value(step).unwrap()["action"]["timeout_secs"].is_null());
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
        let schema = tool_definitions(60);
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
