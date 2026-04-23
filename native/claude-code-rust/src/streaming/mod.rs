use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::api::Usage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamToolCall {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamUpdate {
    AnswerDelta {
        delta: String,
        full_text: String,
    },
    ReasoningDelta {
        delta: String,
        full_text: String,
    },
    ToolCallDelta {
        tool_call: StreamToolCall,
        arguments_delta: String,
    },
    Finished {
        finish_reason: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamSnapshot {
    pub answer_text: String,
    pub reasoning_text: String,
    pub tool_calls: Vec<StreamToolCall>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
    pub completed: bool,
}

#[derive(Debug, Default)]
pub struct StreamingAssembler {
    pending_sse: String,
    answer_text: String,
    reasoning_text: String,
    tool_calls: BTreeMap<usize, StreamToolCall>,
    usage: Option<Usage>,
    finish_reason: Option<String>,
    completed: bool,
}

impl StreamingAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Result<Vec<StreamUpdate>> {
        self.pending_sse.push_str(&String::from_utf8_lossy(bytes));
        self.pending_sse = self.pending_sse.replace("\r\n", "\n");

        let mut updates = Vec::new();
        while let Some(boundary) = self.pending_sse.find("\n\n") {
            let frame = self.pending_sse[..boundary].to_string();
            self.pending_sse = self.pending_sse[boundary + 2..].to_string();

            if frame.trim().is_empty() {
                continue;
            }

            updates.extend(self.process_sse_frame(&frame)?);
        }

        Ok(updates)
    }

    pub fn snapshot(&self) -> StreamSnapshot {
        StreamSnapshot {
            answer_text: self.answer_text.clone(),
            reasoning_text: self.reasoning_text.clone(),
            tool_calls: self.tool_calls.values().cloned().collect(),
            usage: self.usage.clone(),
            finish_reason: self.finish_reason.clone(),
            completed: self.completed,
        }
    }

    fn process_sse_frame(&mut self, frame: &str) -> Result<Vec<StreamUpdate>> {
        let mut event_name = None;
        let mut data_lines = Vec::new();

        for raw_line in frame.lines() {
            let line = raw_line.trim_end();
            if let Some(rest) = line.strip_prefix("event:") {
                event_name = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.trim_start().to_string());
            }
        }

        let payload = data_lines.join("\n");
        if payload.is_empty() {
            return Ok(Vec::new());
        }

        if payload == "[DONE]" {
            return Ok(self.finish_stream(self.finish_reason.clone()));
        }

        let value: Value = serde_json::from_str(&payload)
            .map_err(|error| anyhow!("Invalid streaming payload JSON: {}", error))?;

        if value.get("choices").is_some() {
            self.process_openai_chunk(&value)
        } else {
            self.process_anthropic_event(event_name.as_deref(), &value)
        }
    }

    fn process_openai_chunk(&mut self, value: &Value) -> Result<Vec<StreamUpdate>> {
        let mut updates = Vec::new();
        if let Some(usage) = parse_openai_usage(value) {
            self.usage = Some(usage);
        }
        let choices = value
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("OpenAI chunk missing choices array"))?;

        for choice in choices {
            let delta = choice.get("delta").unwrap_or(choice);

            if let Some(reasoning_delta) = first_non_blank(
                delta,
                &["reasoning_content", "reasoningContent", "reasoning"],
            ) {
                updates.push(self.append_reasoning(reasoning_delta));
            }

            if let Some(content_delta) = delta.get("content").and_then(Value::as_str) {
                if !content_delta.is_empty() {
                    updates.push(self.append_answer(content_delta));
                }
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for (fallback_index, tool_delta) in tool_calls.iter().enumerate() {
                    let index = tool_delta
                        .get("index")
                        .and_then(Value::as_u64)
                        .map(|value| value as usize)
                        .unwrap_or(fallback_index);
                    let tool_call = self.upsert_openai_tool_call(index, tool_delta);
                    let arguments_delta = tool_delta
                        .get("function")
                        .and_then(|value| value.get("arguments"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();

                    updates.push(StreamUpdate::ToolCallDelta {
                        tool_call,
                        arguments_delta,
                    });
                }
            }

            if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
                if !finish_reason.is_empty() {
                    updates.extend(self.finish_stream(Some(finish_reason.to_string())));
                }
            }
        }

        Ok(updates)
    }

    fn process_anthropic_event(
        &mut self,
        event_name: Option<&str>,
        value: &Value,
    ) -> Result<Vec<StreamUpdate>> {
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .or(event_name)
            .unwrap_or_default();

        let mut updates = Vec::new();
        match event_type {
            "ping" => {}
            "error" => {
                let message = value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .or_else(|| value.get("message").and_then(Value::as_str))
                    .unwrap_or("Unknown streaming error");
                return Err(anyhow!("Streaming provider error: {}", message));
            }
            "content_block_start" => {
                let index = value
                    .get("index")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize)
                    .unwrap_or_default();
                let content_block = value.get("content_block").unwrap_or(value);
                match content_block
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "text" => {
                        if let Some(text) = content_block.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                updates.push(self.append_answer(text));
                            }
                        }
                    }
                    "thinking" => {
                        if let Some(thinking) =
                            content_block.get("thinking").and_then(Value::as_str)
                        {
                            if !thinking.is_empty() {
                                updates.push(self.append_reasoning(thinking));
                            }
                        }
                    }
                    "tool_use" => {
                        let tool_call = self.upsert_anthropic_tool_call(index, content_block);
                        updates.push(StreamUpdate::ToolCallDelta {
                            tool_call,
                            arguments_delta: String::new(),
                        });
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let index = value
                    .get("index")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize)
                    .unwrap_or_default();
                let delta = value.get("delta").unwrap_or(value);
                match delta
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                updates.push(self.append_answer(text));
                            }
                        }
                    }
                    "thinking_delta" => {
                        if let Some(thinking) = delta.get("thinking").and_then(Value::as_str) {
                            if !thinking.is_empty() {
                                updates.push(self.append_reasoning(thinking));
                            }
                        }
                    }
                    "input_json_delta" => {
                        let arguments_delta = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let tool_call =
                            self.append_tool_arguments(index, arguments_delta.to_string());
                        updates.push(StreamUpdate::ToolCallDelta {
                            tool_call,
                            arguments_delta: arguments_delta.to_string(),
                        });
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(usage) = parse_anthropic_usage(value) {
                    self.usage = Some(usage);
                }
                if let Some(stop_reason) = value
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    if !stop_reason.is_empty() {
                        self.finish_reason = Some(stop_reason.to_string());
                    }
                }
            }
            "message_stop" => {
                updates.extend(self.finish_stream(self.finish_reason.clone()));
            }
            "content_block_stop" | "message_start" => {}
            _ => {}
        }

        Ok(updates)
    }

    fn append_answer(&mut self, delta: &str) -> StreamUpdate {
        self.answer_text.push_str(delta);
        StreamUpdate::AnswerDelta {
            delta: delta.to_string(),
            full_text: self.answer_text.clone(),
        }
    }

    fn append_reasoning(&mut self, delta: &str) -> StreamUpdate {
        self.reasoning_text.push_str(delta);
        StreamUpdate::ReasoningDelta {
            delta: delta.to_string(),
            full_text: self.reasoning_text.clone(),
        }
    }

    fn upsert_openai_tool_call(&mut self, index: usize, tool_delta: &Value) -> StreamToolCall {
        let tool_call = self.ensure_tool_call(index);
        if let Some(id) = tool_delta.get("id").and_then(Value::as_str) {
            tool_call.id = Some(id.to_string());
        }
        if let Some(name) = tool_delta
            .get("function")
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
        {
            tool_call.name = Some(name.to_string());
        }
        if let Some(arguments_delta) = tool_delta
            .get("function")
            .and_then(|value| value.get("arguments"))
            .and_then(Value::as_str)
        {
            tool_call.arguments.push_str(arguments_delta);
        }
        tool_call.clone()
    }

    fn upsert_anthropic_tool_call(
        &mut self,
        index: usize,
        content_block: &Value,
    ) -> StreamToolCall {
        let tool_call = self.ensure_tool_call(index);
        if let Some(id) = content_block.get("id").and_then(Value::as_str) {
            tool_call.id = Some(id.to_string());
        }
        if let Some(name) = content_block.get("name").and_then(Value::as_str) {
            tool_call.name = Some(name.to_string());
        }
        if let Some(input) = content_block
            .get("input")
            .filter(|value| has_meaningful_json(value))
        {
            tool_call.arguments = serde_json::to_string(input).unwrap_or_default();
        }
        tool_call.clone()
    }

    fn append_tool_arguments(&mut self, index: usize, arguments_delta: String) -> StreamToolCall {
        let tool_call = self.ensure_tool_call(index);
        tool_call.arguments.push_str(&arguments_delta);
        tool_call.clone()
    }

    fn ensure_tool_call(&mut self, index: usize) -> &mut StreamToolCall {
        self.tool_calls
            .entry(index)
            .or_insert_with(|| StreamToolCall {
                index,
                id: None,
                name: None,
                arguments: String::new(),
            })
    }

    fn finish_stream(&mut self, finish_reason: Option<String>) -> Vec<StreamUpdate> {
        if let Some(reason) = finish_reason {
            self.finish_reason = Some(reason);
        }

        if self.completed {
            return Vec::new();
        }

        self.completed = true;
        vec![StreamUpdate::Finished {
            finish_reason: self.finish_reason.clone(),
        }]
    }
}

fn first_non_blank<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|entry| !entry.is_empty())
    })
}

fn has_meaningful_json(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
        _ => true,
    }
}

fn parse_openai_usage(value: &Value) -> Option<Usage> {
    value.get("usage").and_then(parse_usage_from_value)
}

fn parse_anthropic_usage(value: &Value) -> Option<Usage> {
    value.get("usage")
        .or_else(|| value.get("delta").and_then(|delta| delta.get("usage")))
        .and_then(parse_usage_from_value)
}

fn parse_usage_from_value(value: &Value) -> Option<Usage> {
    let prompt_tokens = value
        .get("prompt_tokens")
        .or_else(|| value.get("input_tokens"))
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    let completion_tokens = value
        .get("completion_tokens")
        .or_else(|| value.get("output_tokens"))
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    let total_tokens = value
        .get("total_tokens")
        .and_then(Value::as_u64)
        .map(|value| value as usize);

    if prompt_tokens.is_none() && completion_tokens.is_none() && total_tokens.is_none() {
        return None;
    }

    let prompt_tokens = prompt_tokens.unwrap_or_default();
    let completion_tokens = completion_tokens.unwrap_or_default();
    let total_tokens = total_tokens.unwrap_or(prompt_tokens + completion_tokens);

    Some(Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_text_reasoning_and_tool_call_deltas() {
        let mut assembler = StreamingAssembler::new();

        let updates = assembler
            .push_bytes(
                br#"data: {"choices":[{"delta":{"reasoning_content":"Thinking "},"finish_reason":null}]}

data: {"choices":[{"delta":{"content":"Hello "},"finish_reason":null}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"search","arguments":"{\"path\":\"src"}}]},"finish_reason":null}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\",\"pattern\":\"chat\"}"}}],"content":"world"},"finish_reason":"tool_calls"}]}

data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":120,"completion_tokens":24,"total_tokens":144}}

data: [DONE]

"#,
            )
            .expect("stream parse");

        assert!(updates.iter().any(|update| matches!(
            update,
            StreamUpdate::ReasoningDelta { full_text, .. } if full_text == "Thinking "
        )));
        assert!(updates.iter().any(|update| matches!(
            update,
            StreamUpdate::AnswerDelta { full_text, .. } if full_text == "Hello "
        )));
        assert!(updates.iter().any(|update| matches!(
            update,
            StreamUpdate::AnswerDelta { full_text, .. } if full_text == "Hello world"
        )));
        assert!(updates.iter().any(|update| matches!(
            update,
            StreamUpdate::ToolCallDelta { tool_call, .. }
                if tool_call.id.as_deref() == Some("call_1")
                && tool_call.name.as_deref() == Some("search")
                && tool_call.arguments == "{\"path\":\"src\",\"pattern\":\"chat\"}"
        )));
        assert!(updates.iter().any(|update| matches!(
            update,
            StreamUpdate::Finished { finish_reason } if finish_reason.as_deref() == Some("tool_calls")
        )));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.reasoning_text, "Thinking ");
        assert_eq!(snapshot.answer_text, "Hello world");
        assert_eq!(snapshot.tool_calls.len(), 1);
        assert_eq!(
            snapshot.tool_calls[0].arguments,
            "{\"path\":\"src\",\"pattern\":\"chat\"}"
        );
        assert_eq!(
            snapshot.usage,
            Some(Usage {
                prompt_tokens: 120,
                completion_tokens: 24,
                total_tokens: 144,
            })
        );
        assert!(snapshot.completed);
    }

    #[test]
    fn parses_anthropic_content_blocks() {
        let mut assembler = StreamingAssembler::new();

        let updates = assembler
            .push_bytes(
                br#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","role":"assistant","content":[]}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"Need to inspect files. "}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Final "}}

event: content_block_start
data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"search","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"pattern\":\"stream\"}"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"answer"}}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"input_tokens":220,"output_tokens":44}}

event: message_stop
data: {"type":"message_stop"}

"#,
            )
            .expect("stream parse");

        assert!(updates.iter().any(|update| matches!(
            update,
            StreamUpdate::ReasoningDelta { full_text, .. } if full_text == "Need to inspect files. "
        )));
        assert!(updates.iter().any(|update| matches!(
            update,
            StreamUpdate::AnswerDelta { full_text, .. } if full_text == "Final answer"
        )));
        assert!(updates.iter().any(|update| matches!(
            update,
            StreamUpdate::ToolCallDelta { tool_call, .. }
                if tool_call.id.as_deref() == Some("toolu_1")
                && tool_call.name.as_deref() == Some("search")
                && tool_call.arguments == "{\"pattern\":\"stream\"}"
        )));

        let snapshot = assembler.snapshot();
        assert_eq!(snapshot.reasoning_text, "Need to inspect files. ");
        assert_eq!(snapshot.answer_text, "Final answer");
        assert_eq!(snapshot.finish_reason.as_deref(), Some("tool_use"));
        assert_eq!(
            snapshot.usage,
            Some(Usage {
                prompt_tokens: 220,
                completion_tokens: 44,
                total_tokens: 264,
            })
        );
        assert!(snapshot.completed);
    }

    #[test]
    fn buffers_partial_sse_frames_until_separator_arrives() {
        let mut assembler = StreamingAssembler::new();

        let first_updates = assembler
            .push_bytes(br#"data: {"choices":[{"delta":{"content":"hel"#)
            .expect("partial parse");
        assert!(first_updates.is_empty());

        let second_updates = assembler
            .push_bytes(
                br#"lo"}}]}

data: [DONE]

"#,
            )
            .expect("completion parse");

        assert!(second_updates.iter().any(|update| matches!(
            update,
            StreamUpdate::AnswerDelta { full_text, .. } if full_text == "hello"
        )));
        assert!(assembler.snapshot().completed);
    }
}
