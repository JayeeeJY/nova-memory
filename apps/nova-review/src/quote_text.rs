//! Offline, opt-in translation of unique quote text into the existing core wire format.
//! No model, storage, network, commit, normalization, or semantic approval occurs here.
//! Hashes bind caller-supplied bytes; they do not authenticate a source or prove entailment.
use nova_core::{
    compilation::{SemanticContent, SemanticOrigin, ValidTime, decode_untrusted_json},
    intake::{EXTRACTION_VERSION, ExtractedUnit, ExtractionOutput, MAX_MODEL_BYTES, QuoteAnchor},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use serde_json::{Map, Value};
use std::fmt;

pub const QUOTE_TEXT_VERSION: &str = "nova.extraction.quote-text.v0.1";
pub const MAX_SOURCE_BYTES: usize = 128 * 1024;
const MAX_UNITS: usize = 16;
const MAX_QUOTES: usize = 16;
const MAX_QUOTE_BYTES: usize = 4096;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteTextOutput {
    schema_version: String,
    units: Vec<QuoteTextUnit>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteTextUnit {
    origin: SemanticOrigin,
    content: SemanticContent,
    occurred_at: Option<String>,
    #[serde(deserialize_with = "strict_valid_time")]
    valid_time: ValidTime,
    quotes: Vec<QuoteText>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteText {
    quote: String,
}

// serde's internally tagged unit variant can discard fields even with deny_unknown_fields.
// Check the closed wire keys before constructing the unchanged core ValidTime type.
fn strict_valid_time<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ValidTime, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let object = value
        .as_object()
        .ok_or_else(|| serde::de::Error::custom("invalid valid_time shape"))?;
    let allowed: &[&str] = match object.get("kind").and_then(serde_json::Value::as_str) {
        Some("unknown") => &["kind"],
        Some("interval") => &["kind", "start", "end"],
        _ => return Err(serde::de::Error::custom("invalid valid_time kind")),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(serde::de::Error::custom("unknown valid_time field"));
    }
    serde_json::from_value(value).map_err(|_| serde::de::Error::custom("invalid valid_time shape"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteTextError {
    SourceLimit,
    RawLimit,
    InvalidShape,
    NumericPrecision,
    UnsupportedVersion,
    UnitLimit,
    OriginNotAllowed,
    QuoteCount,
    QuoteBounds,
    QuoteAbsent,
    QuoteAmbiguous,
    ResolvedLimit,
    CoreDecode,
}

impl QuoteTextError {
    /// Stable diagnostics contain no source, quote, path, or imported field value.
    pub fn code(self) -> &'static str {
        match self {
            Self::SourceLimit => "quote_text_source_limit",
            Self::RawLimit => "quote_text_raw_limit",
            Self::InvalidShape => "quote_text_invalid_shape",
            Self::NumericPrecision => "quote_text_numeric_precision",
            Self::UnsupportedVersion => "quote_text_unsupported_version",
            Self::UnitLimit => "quote_text_unit_limit",
            Self::OriginNotAllowed => "quote_text_origin_not_allowed",
            Self::QuoteCount => "quote_text_quote_count",
            Self::QuoteBounds => "quote_text_quote_bounds",
            Self::QuoteAbsent => "quote_text_quote_absent",
            Self::QuoteAmbiguous => "quote_text_quote_ambiguous",
            Self::ResolvedLimit => "quote_text_resolved_limit",
            Self::CoreDecode => "quote_text_core_decode",
        }
    }
}

impl fmt::Display for QuoteTextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for QuoteTextError {}

/// Zero-based Unicode scalar offsets match the core EvidenceSpan convention.
/// End offsets are exclusive. The indices identify the unchanged unit and quote order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuoteProof {
    pub unit_index: usize,
    pub quote_index: usize,
    pub start_char: usize,
    pub end_char: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticAcceptance {
    PendingIndependentReview,
}

/// Immutable resolved bytes are the only bytes covered by resolved_sha256.
/// Callers must still bind the source snapshot and execute the ordinary core transaction.
#[derive(Debug)]
pub struct ResolvedExtraction {
    extraction: ExtractionOutput,
    resolved_bytes: Vec<u8>,
    source_sha256: String,
    raw_sha256: String,
    resolved_sha256: String,
    proofs: Vec<QuoteProof>,
}

impl ResolvedExtraction {
    pub fn extraction(&self) -> &ExtractionOutput {
        &self.extraction
    }
    pub fn resolved_bytes(&self) -> &[u8] {
        &self.resolved_bytes
    }
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }
    pub fn raw_sha256(&self) -> &str {
        &self.raw_sha256
    }
    pub fn resolved_sha256(&self) -> &str {
        &self.resolved_sha256
    }
    pub fn proofs(&self) -> &[QuoteProof] {
        &self.proofs
    }
    pub fn semantic_acceptance(&self) -> SemanticAcceptance {
        SemanticAcceptance::PendingIndependentReview
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Exact decimal value, without floating-point arithmetic or allocating exponent-sized zeros.
/// The strict decoder has already checked JSON grammar; exponent overflow still fails closed.
fn decimal_value(token: &str) -> Option<(bool, String, i64)> {
    let negative = token.starts_with('-');
    let token = token.strip_prefix('-').unwrap_or(token);
    let (mantissa, exponent) = match token.find(['e', 'E']) {
        Some(i) => (&token[..i], token[i + 1..].parse::<i64>().ok()?),
        None => (token, 0),
    };
    let fractional = mantissa.find('.').map_or(0, |i| mantissa.len() - i - 1);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some((negative, "0".into(), 0));
    }
    let significant = digits.trim_end_matches('0');
    let trailing = digits.len() - significant.len();
    let exponent = exponent
        .checked_sub(i64::try_from(fractional).ok()?)?
        .checked_add(i64::try_from(trailing).ok()?)?;
    Some((negative, significant.into(), exponent))
}

/// serde_json's default Number may round large integers/long decimals to f64. Reject
/// a changed decimal value before exposing the typed object. Strings, including escaped
/// quote text and number-looking text, are never inspected or rewritten as numbers.
fn numeric_round_trip(raw: &[u8]) -> Result<(), QuoteTextError> {
    let mut i = 0;
    let mut in_string = false;
    while i < raw.len() {
        if in_string {
            match raw[i] {
                b'\\' => i += 1, // Skip one JSON escape; grammar was already validated.
                b'"' => in_string = false,
                _ => {}
            }
            i += 1;
        } else if raw[i] == b'"' {
            in_string = true;
            i += 1;
        } else if raw[i] == b'-' || raw[i].is_ascii_digit() {
            let start = i;
            i += 1;
            while i < raw.len() && (raw[i].is_ascii_digit() || b".eE+-".contains(&raw[i])) {
                i += 1;
            }
            let token = std::str::from_utf8(&raw[start..i])
                .map_err(|_| QuoteTextError::NumericPrecision)?;
            let number: serde_json::Number =
                serde_json::from_str(token).map_err(|_| QuoteTextError::NumericPrecision)?;
            let before = decimal_value(token).ok_or(QuoteTextError::NumericPrecision)?;
            let after =
                decimal_value(&number.to_string()).ok_or(QuoteTextError::NumericPrecision)?;
            if before != after {
                return Err(QuoteTextError::NumericPrecision);
            }
        } else {
            i += 1;
        }
    }
    Ok(())
}

/// Resolve only the new explicit quote-text contract. Old occurrence-bearing output is
/// rejected, not repaired. A unique literal match permits occurrence zero; overlapping
/// matches also count as ambiguity. Semantic fields are moved without rewriting them.
pub fn resolve(source_text: &str, raw: &[u8]) -> Result<ResolvedExtraction, QuoteTextError> {
    resolve_wire(source_text, raw, false)
}

/// Convert explicit key/value rows to the existing conditions map without changing either.
pub fn resolve_condition_list(
    source_text: &str,
    raw: &[u8],
) -> Result<ResolvedExtraction, QuoteTextError> {
    resolve_wire(source_text, raw, true)
}

fn condition_map(input: Value) -> Result<Map<String, Value>, QuoteTextError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Condition {
        key: String,
        value: Value,
    }
    let rows: Vec<Condition> =
        serde_json::from_value(input).map_err(|_| QuoteTextError::InvalidShape)?;
    if rows.len() > 16 {
        return Err(QuoteTextError::InvalidShape);
    }
    let mut result = Map::new();
    for Condition { key, value } in rows {
        let qualified = key.split_once(':').is_some_and(|(namespace, name)| {
            [namespace, name].iter().all(|part| {
                part.len() <= 64
                    && part.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                    && part.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.".contains(&b)
                    })
            })
        });
        if !qualified || result.insert(key, value).is_some() {
            return Err(QuoteTextError::InvalidShape);
        }
    }
    Ok(result)
}

fn resolve_wire(
    source_text: &str,
    raw: &[u8],
    condition_list: bool,
) -> Result<ResolvedExtraction, QuoteTextError> {
    if source_text.len() > MAX_SOURCE_BYTES {
        return Err(QuoteTextError::SourceLimit);
    }
    if raw.len() > MAX_MODEL_BYTES {
        return Err(QuoteTextError::RawLimit);
    }
    let incoming: QuoteTextOutput = if condition_list {
        let mut wire: Value =
            decode_untrusted_json(raw).map_err(|_| QuoteTextError::InvalidShape)?;
        let units = wire.get_mut("units").and_then(Value::as_array_mut)
            .ok_or(QuoteTextError::InvalidShape)?;
        if units.len() > MAX_UNITS {
            return Err(QuoteTextError::UnitLimit);
        }
        for unit in units {
            let conditions = unit.pointer_mut("/content/conditions")
                .ok_or(QuoteTextError::InvalidShape)?;
            *conditions = Value::Object(condition_map(conditions.take())?);
        }
        serde_json::from_value(wire).map_err(|_| QuoteTextError::InvalidShape)?
    } else {
        decode_untrusted_json(raw).map_err(|_| QuoteTextError::InvalidShape)?
    };
    numeric_round_trip(raw)?;
    if incoming.schema_version != QUOTE_TEXT_VERSION {
        return Err(QuoteTextError::UnsupportedVersion);
    }
    if incoming.units.len() > MAX_UNITS {
        return Err(QuoteTextError::UnitLimit);
    }
    let mut proofs = Vec::new();
    let mut units = Vec::with_capacity(incoming.units.len());
    for (unit_index, unit) in incoming.units.into_iter().enumerate() {
        if !matches!(
            unit.origin,
            SemanticOrigin::Extracted
                | SemanticOrigin::Inferred
                | SemanticOrigin::ExternalClaim
                | SemanticOrigin::AgentExperience
        ) {
            return Err(QuoteTextError::OriginNotAllowed);
        }
        if unit.quotes.is_empty() || unit.quotes.len() > MAX_QUOTES {
            return Err(QuoteTextError::QuoteCount);
        }
        let mut quotes = Vec::with_capacity(unit.quotes.len());
        for (quote_index, quote) in unit.quotes.into_iter().enumerate() {
            if quote.quote.trim().is_empty() || quote.quote.len() > MAX_QUOTE_BYTES {
                return Err(QuoteTextError::QuoteBounds);
            }
            let start_byte = source_text
                .find(&quote.quote)
                .ok_or(QuoteTextError::QuoteAbsent)?;
            // Advance one Unicode scalar rather than quote.len(), so overlapping matches
            // ("aaa" / "aa", or "中中中" / "中中") cannot silently become occurrence zero.
            let next_boundary = start_byte + quote.quote.chars().next().unwrap().len_utf8();
            if source_text[next_boundary..].contains(&quote.quote) {
                return Err(QuoteTextError::QuoteAmbiguous);
            }
            let start_char = source_text[..start_byte].chars().count();
            proofs.push(QuoteProof {
                unit_index,
                quote_index,
                start_char,
                end_char: start_char + quote.quote.chars().count(),
                start_byte,
                end_byte: start_byte + quote.quote.len(),
            });
            quotes.push(QuoteAnchor {
                quote: quote.quote,
                occurrence: 0,
            });
        }
        units.push(ExtractedUnit {
            origin: unit.origin,
            content: unit.content,
            occurred_at: unit.occurred_at,
            valid_time: unit.valid_time,
            quotes,
        });
    }
    let extraction = ExtractionOutput {
        schema_version: EXTRACTION_VERSION.into(),
        units,
    };
    let resolved_bytes = serde_json::to_vec(&extraction).map_err(|_| QuoteTextError::CoreDecode)?;
    if resolved_bytes.len() > MAX_MODEL_BYTES {
        return Err(QuoteTextError::ResolvedLimit);
    }
    // Reuse the unchanged public core wire decoder; this is not a commit or entailment check.
    let extraction =
        ExtractionOutput::parse(&resolved_bytes).map_err(|_| QuoteTextError::CoreDecode)?;
    Ok(ResolvedExtraction {
        extraction,
        source_sha256: hash(source_text.as_bytes()),
        raw_sha256: hash(raw),
        resolved_sha256: hash(&resolved_bytes),
        resolved_bytes,
        proofs,
    })
}
