//! Bounded, fail-closed collection of decoded streaming model content.
//!
//! The transport must call `record_wire` before accepting each raw response chunk, and pass only
//! decoded choice-content bytes to `feed_content`. This type performs no I/O, repair, retry, or
//! partial-output delivery. A valid prefix never authorizes a result: `finish` validates the entire
//! object with the trusted extraction parser after the transport has observed a clean stop.
use nova_core::intake::{ExtractionOutput, MAX_MODEL_BYTES};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const MAX_GENERATION_WIRE_BYTES: usize = 1024 * 1024;
pub const MAX_GENERATION_DECODED_BYTES: usize = MAX_MODEL_BYTES;
pub const MAX_GENERATION_UNITS: usize = 16;
pub const REPEATED_UNIT_THRESHOLD: usize = 3;
const MAX_JSON_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationGuardError {
    WireLimit,
    DecodedLimit,
    JsonDepthLimit,
    InvalidJson,
    DuplicateJsonKey,
    TopLevelObjectRequired,
    InvalidExtraction,
    UnitsLimit,
    RepeatedUnits,
    ProviderError,
    ToolOutput,
    MissingFinishReason,
    OutputLength,
    NonStopFinish,
    MissingDone,
    InvalidUsage,
}
impl GenerationGuardError {
    /// Stable allowlisted codes; never includes model text, provider messages, or credentials.
    pub fn code(self) -> &'static str {
        match self {
            Self::WireLimit => "generation_wire_limit",
            Self::DecodedLimit => "generation_decoded_limit",
            Self::JsonDepthLimit => "generation_json_depth_limit",
            Self::InvalidJson => "generation_invalid_json",
            Self::DuplicateJsonKey => "generation_duplicate_json_key",
            Self::TopLevelObjectRequired => "generation_top_level_object_required",
            Self::InvalidExtraction => "generation_invalid_extraction",
            Self::UnitsLimit => "generation_units_limit",
            Self::RepeatedUnits => "generation_repeated_units",
            Self::ProviderError => "generation_provider_error",
            Self::ToolOutput => "generation_tool_output",
            Self::MissingFinishReason => "generation_missing_finish_reason",
            Self::OutputLength => "generation_output_length",
            Self::NonStopFinish => "generation_non_stop_finish",
            Self::MissingDone => "generation_missing_done",
            Self::InvalidUsage => "generation_invalid_usage",
        }
    }
}
impl std::fmt::Display for GenerationGuardError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for GenerationGuardError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}
impl GenerationUsage {
    fn parse(value: &Value) -> Result<Self, GenerationGuardError> {
        let invalid = GenerationGuardError::InvalidUsage;
        let object = value.as_object().ok_or(invalid)?;
        let count = |field| object.get(field).and_then(Value::as_u64).ok_or(invalid);
        let usage = Self {
            prompt_tokens: count("prompt_tokens")?,
            completion_tokens: count("completion_tokens")?,
            total_tokens: count("total_tokens")?,
        };
        if usage.prompt_tokens.checked_add(usage.completion_tokens) != Some(usage.total_tokens) {
            return Err(invalid);
        }
        Ok(usage)
    }
}

/// The SSE adapter remains responsible for unique choice/index validation, error frames, and
/// duplicate keys in provider envelopes. Missing usage stays unknown; never replace it with zero.
#[derive(Debug, Default)]
pub struct CompletionEvidence<'a> {
    pub finish_reason: Option<&'a str>,
    pub sse_done: bool,
    pub usage: Option<&'a Value>,
    pub provider_error: bool,
    pub tools_present: bool,
}

#[derive(Debug)]
pub struct GuardedGeneration {
    /// Available only after protocol, entire JSON, and trusted extraction-schema validation.
    pub content: Vec<u8>,
    pub usage: Option<GenerationUsage>,
    pub completed_units: usize,
    pub wire_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectPosition {
    KeyOrEnd(bool),
    Colon,
    Value,
    CommaOrEnd,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArrayPosition {
    ValueOrEnd(bool),
    CommaOrEnd,
}
#[derive(Debug)]
enum Container {
    Object {
        position: ObjectPosition,
        key: Option<String>,
        keys: BTreeSet<String>,
        root: bool,
    },
    Array {
        position: ArrayPosition,
        units: bool,
    },
}
#[derive(Debug)]
struct Frame {
    container: Container,
    start: usize,
    direct_unit: bool,
}
#[derive(Debug)]
enum LexicalState {
    Between,
    String { start: usize, escaped: bool },
    Primitive { start: usize },
}
#[derive(Debug, Default)]
struct ValueContext {
    direct_unit: bool,
    units_field: bool,
}

/// Conservative structural loop detection: three consecutive complete, exactly equal unit JSON
/// objects. Object key order and JSON string escaping do not matter. Every field, including quote
/// occurrences, time, source identifiers, and conditions, participates in equality. Similar text or
/// different support is never considered equivalent. This guard intentionally misses less exact
/// loops; the independent 16-unit limit still stops unbounded arrays without guessing semantics.
#[derive(Debug)]
pub struct GenerationGuard {
    content: Vec<u8>,
    wire_bytes: usize,
    scanned: usize,
    lexical: LexicalState,
    stack: Vec<Frame>,
    root_started: bool,
    root_completed: bool,
    completed_units: usize,
    previous_unit: Option<Value>,
    repeated_run: usize,
    failed: Option<GenerationGuardError>,
}
impl Default for GenerationGuard {
    fn default() -> Self {
        Self::new()
    }
}
impl GenerationGuard {
    pub fn new() -> Self {
        Self {
            content: Vec::new(),
            wire_bytes: 0,
            scanned: 0,
            lexical: LexicalState::Between,
            stack: Vec::new(),
            root_started: false,
            root_completed: false,
            completed_units: 0,
            previous_unit: None,
            repeated_run: 0,
            failed: None,
        }
    }
    pub fn wire_bytes(&self) -> usize {
        self.wire_bytes
    }
    pub fn decoded_bytes(&self) -> usize {
        self.content.len()
    }
    pub fn completed_units(&self) -> usize {
        self.completed_units
    }
    fn healthy(&self) -> Result<(), GenerationGuardError> {
        self.failed.map_or(Ok(()), Err)
    }
    fn reject<T>(&mut self, error: GenerationGuardError) -> Result<T, GenerationGuardError> {
        self.failed = Some(error);
        Err(error)
    }
    pub fn record_wire(&mut self, additional_bytes: usize) -> Result<(), GenerationGuardError> {
        self.healthy()?;
        let Some(total) = self.wire_bytes.checked_add(additional_bytes) else {
            return self.reject(GenerationGuardError::WireLimit);
        };
        if total > MAX_GENERATION_WIRE_BYTES {
            return self.reject(GenerationGuardError::WireLimit);
        }
        self.wire_bytes = total;
        Ok(())
    }
    /// Accepts arbitrary UTF-8 byte fragments, including a split inside a multibyte scalar. No
    /// text is decoded until a complete JSON string or complete unit is available.
    pub fn feed_content(&mut self, bytes: &[u8]) -> Result<(), GenerationGuardError> {
        self.healthy()?;
        if bytes.len() > MAX_GENERATION_DECODED_BYTES.saturating_sub(self.content.len()) {
            return self.reject(GenerationGuardError::DecodedLimit);
        }
        self.content.extend_from_slice(bytes);
        if let Err(error) = self.scan() {
            return self.reject(error);
        }
        Ok(())
    }
    /// A transport can receive a syntactically complete object and still fail (length stop,
    /// absent DONE, invalid usage). In every failure case the owned partial bytes are discarded.
    pub fn finish(
        self,
        evidence: CompletionEvidence<'_>,
    ) -> Result<GuardedGeneration, GenerationGuardError> {
        self.finish_protocol(evidence, false)
    }

    /// Complete quote-text JSON envelope only. The caller MUST run the strict quote-text
    /// resolver against the trusted source snapshot and the ordinary core commit gates.
    /// This opt-in method does not accept an old schema or manufacture quote positions.
    pub fn finish_quote_text_envelope(
        self,
        evidence: CompletionEvidence<'_>,
    ) -> Result<GuardedGeneration, GenerationGuardError> {
        self.finish_protocol(evidence, true)
    }

    fn finish_protocol(
        mut self,
        evidence: CompletionEvidence<'_>,
        quote_text: bool,
    ) -> Result<GuardedGeneration, GenerationGuardError> {
        self.healthy()?;
        if evidence.provider_error {
            return Err(GenerationGuardError::ProviderError);
        }
        if evidence.tools_present {
            return Err(GenerationGuardError::ToolOutput);
        }
        match evidence.finish_reason {
            Some("stop") => {}
            Some("length") => return Err(GenerationGuardError::OutputLength),
            Some(_) => return Err(GenerationGuardError::NonStopFinish),
            None => return Err(GenerationGuardError::MissingFinishReason),
        }
        if !evidence.sse_done {
            return Err(GenerationGuardError::MissingDone);
        }
        let usage = evidence.usage.map(GenerationUsage::parse).transpose()?;
        if let LexicalState::Primitive { start } = self.lexical {
            self.lexical = LexicalState::Between;
            self.primitive(start, self.content.len())?;
        }
        if !self.root_completed
            || !self.stack.is_empty()
            || !matches!(self.lexical, LexicalState::Between)
        {
            return Err(GenerationGuardError::InvalidJson);
        }
        // Parse the original complete byte sequence, not a trimmed or reconstructed prefix.
        let unit_count = if quote_text {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Envelope {
                schema_version: String,
                units: Vec<Value>,
            }
            let output: Envelope = nova_core::compilation::decode_untrusted_json(&self.content)
                .map_err(|_| GenerationGuardError::InvalidExtraction)?;
            if output.schema_version != crate::quote_text_schema::SCHEMA_VERSION {
                return Err(GenerationGuardError::InvalidExtraction);
            }
            output.units.len()
        } else {
            ExtractionOutput::parse(&self.content)
                .map_err(|_| GenerationGuardError::InvalidExtraction)?
                .units
                .len()
        };
        if unit_count != self.completed_units {
            return Err(GenerationGuardError::InvalidExtraction);
        }
        Ok(GuardedGeneration {
            content: self.content,
            usage,
            completed_units: self.completed_units,
            wire_bytes: self.wire_bytes,
        })
    }

    fn scan(&mut self) -> Result<(), GenerationGuardError> {
        while self.scanned < self.content.len() {
            let index = self.scanned;
            let byte = self.content[index];
            match self.lexical {
                LexicalState::String { start, escaped } => {
                    self.scanned += 1;
                    if byte < 0x20 {
                        return Err(GenerationGuardError::InvalidJson);
                    }
                    if escaped {
                        self.lexical = LexicalState::String {
                            start,
                            escaped: false,
                        };
                    } else if byte == b'\\' {
                        self.lexical = LexicalState::String {
                            start,
                            escaped: true,
                        };
                    } else if byte == b'"' {
                        self.lexical = LexicalState::Between;
                        let value: String =
                            serde_json::from_slice(&self.content[start..self.scanned])
                                .map_err(|_| GenerationGuardError::InvalidJson)?;
                        self.string_value(value, start, self.scanned)?;
                    }
                }
                LexicalState::Primitive { start } => {
                    if json_whitespace(byte) || matches!(byte, b',' | b']' | b'}') {
                        self.lexical = LexicalState::Between;
                        self.primitive(start, index)?;
                        // Process this delimiter again in Between state.
                    } else {
                        self.scanned += 1;
                    }
                }
                LexicalState::Between => {
                    self.scanned += 1;
                    if json_whitespace(byte) {
                        continue;
                    }
                    match byte {
                        b'"' => {
                            self.lexical = LexicalState::String {
                                start: index,
                                escaped: false,
                            };
                        }
                        b'{' | b'[' => self.open(byte, index)?,
                        b'}' | b']' => self.close(byte, self.scanned)?,
                        b':' => self.colon()?,
                        b',' => self.comma()?,
                        b'-' | b'0'..=b'9' | b't' | b'f' | b'n' => {
                            self.lexical = LexicalState::Primitive { start: index };
                        }
                        _ => return Err(GenerationGuardError::InvalidJson),
                    }
                }
            }
        }
        Ok(())
    }
    fn begin_value(&mut self) -> Result<ValueContext, GenerationGuardError> {
        let Some(frame) = self.stack.last_mut() else {
            if self.root_started {
                return Err(GenerationGuardError::InvalidJson);
            }
            self.root_started = true;
            return Ok(ValueContext::default());
        };
        match &mut frame.container {
            Container::Object {
                position,
                key,
                root,
                ..
            } if *position == ObjectPosition::Value => {
                *position = ObjectPosition::CommaOrEnd;
                Ok(ValueContext {
                    direct_unit: false,
                    units_field: *root && key.as_deref() == Some("units"),
                })
            }
            Container::Array { position, units }
                if matches!(position, ArrayPosition::ValueOrEnd(_)) =>
            {
                *position = ArrayPosition::CommaOrEnd;
                Ok(ValueContext {
                    direct_unit: *units,
                    units_field: false,
                })
            }
            _ => Err(GenerationGuardError::InvalidJson),
        }
    }
    fn open(&mut self, byte: u8, start: usize) -> Result<(), GenerationGuardError> {
        let root = self.stack.is_empty();
        if root && byte != b'{' {
            return Err(GenerationGuardError::TopLevelObjectRequired);
        }
        let context = self.begin_value()?;
        if (context.units_field && byte != b'[') || (context.direct_unit && byte != b'{') {
            return Err(GenerationGuardError::InvalidExtraction);
        }
        if self.stack.len() >= MAX_JSON_DEPTH {
            return Err(GenerationGuardError::JsonDepthLimit);
        }
        let container = if byte == b'{' {
            Container::Object {
                position: ObjectPosition::KeyOrEnd(true),
                key: None,
                keys: BTreeSet::new(),
                root,
            }
        } else {
            Container::Array {
                position: ArrayPosition::ValueOrEnd(true),
                units: context.units_field,
            }
        };
        self.stack.push(Frame {
            container,
            start,
            direct_unit: context.direct_unit,
        });
        Ok(())
    }
    fn close(&mut self, byte: u8, end: usize) -> Result<(), GenerationGuardError> {
        let frame = self.stack.pop().ok_or(GenerationGuardError::InvalidJson)?;
        let valid = match (&frame.container, byte) {
            (Container::Object { position, .. }, b'}') => matches!(
                position,
                ObjectPosition::KeyOrEnd(true) | ObjectPosition::CommaOrEnd
            ),
            (Container::Array { position, .. }, b']') => matches!(
                position,
                ArrayPosition::ValueOrEnd(true) | ArrayPosition::CommaOrEnd
            ),
            _ => false,
        };
        if !valid {
            return Err(GenerationGuardError::InvalidJson);
        }
        if frame.direct_unit {
            self.complete_unit(frame.start, end)?;
        }
        if self.stack.is_empty() {
            self.root_completed = true;
        }
        Ok(())
    }
    fn string_value(
        &mut self,
        value: String,
        start: usize,
        end: usize,
    ) -> Result<(), GenerationGuardError> {
        if let Some(Frame {
            container:
                Container::Object {
                    position,
                    key,
                    keys,
                    ..
                },
            ..
        }) = self.stack.last_mut()
            && matches!(position, ObjectPosition::KeyOrEnd(_))
        {
            if !keys.insert(value.clone()) {
                return Err(GenerationGuardError::DuplicateJsonKey);
            }
            *key = Some(value);
            *position = ObjectPosition::Colon;
            return Ok(());
        }
        self.scalar(start, end)
    }
    fn primitive(&mut self, start: usize, end: usize) -> Result<(), GenerationGuardError> {
        let value: Value = serde_json::from_slice(&self.content[start..end])
            .map_err(|_| GenerationGuardError::InvalidJson)?;
        if value.is_object() || value.is_array() || value.is_string() {
            return Err(GenerationGuardError::InvalidJson);
        }
        self.scalar(start, end)
    }
    fn scalar(&mut self, _start: usize, _end: usize) -> Result<(), GenerationGuardError> {
        if self.stack.is_empty() {
            return Err(GenerationGuardError::TopLevelObjectRequired);
        }
        let context = self.begin_value()?;
        if context.direct_unit || context.units_field {
            return Err(GenerationGuardError::InvalidExtraction);
        }
        Ok(())
    }
    fn colon(&mut self) -> Result<(), GenerationGuardError> {
        match self.stack.last_mut().map(|frame| &mut frame.container) {
            Some(Container::Object { position, .. }) if *position == ObjectPosition::Colon => {
                *position = ObjectPosition::Value;
                Ok(())
            }
            _ => Err(GenerationGuardError::InvalidJson),
        }
    }
    fn comma(&mut self) -> Result<(), GenerationGuardError> {
        match self.stack.last_mut().map(|frame| &mut frame.container) {
            Some(Container::Object { position, key, .. })
                if *position == ObjectPosition::CommaOrEnd =>
            {
                *position = ObjectPosition::KeyOrEnd(false);
                *key = None;
                Ok(())
            }
            Some(Container::Array { position, .. }) if *position == ArrayPosition::CommaOrEnd => {
                *position = ArrayPosition::ValueOrEnd(false);
                Ok(())
            }
            _ => Err(GenerationGuardError::InvalidJson),
        }
    }
    fn complete_unit(&mut self, start: usize, end: usize) -> Result<(), GenerationGuardError> {
        self.completed_units += 1;
        if self.completed_units > MAX_GENERATION_UNITS {
            return Err(GenerationGuardError::UnitsLimit);
        }
        let unit: Value = serde_json::from_slice(&self.content[start..end])
            .map_err(|_| GenerationGuardError::InvalidJson)?;
        self.repeated_run = if self.previous_unit.as_ref() == Some(&unit) {
            self.repeated_run + 1
        } else {
            1
        };
        self.previous_unit = Some(unit);
        if self.repeated_run >= REPEATED_UNIT_THRESHOLD {
            return Err(GenerationGuardError::RepeatedUnits);
        }
        Ok(())
    }
}
fn json_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}
