//! Bounded, declarative structure definitions. Validation does not run a model or infer facts.
use crate::{MemoryType, NovaError, PrivacyLevel, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructureFieldKind {
    Text,
    LongText,
    Date,
    Number,
    Enum,
    Multiselect,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureField {
    pub id: String,
    pub label: String,
    pub kind: StructureFieldKind,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub source_required: bool,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub max_length: Option<usize>,
    #[serde(default)]
    pub minimum: Option<f64>,
    #[serde(default)]
    pub maximum: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureTemplate {
    pub template_id: String,
    pub owner_id: String,
    pub revision: u64,
    pub title: String,
    pub memory_type: MemoryType,
    pub privacy_level: PrivacyLevel,
    pub guidance: String,
    #[serde(default)]
    pub fields: Vec<StructureField>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureInput {
    pub template_id: String,
    pub template_revision: u64,
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
    /// Exact quotations from the supplied source. Presence is not semantic entailment.
    #[serde(default)]
    pub evidence: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredMemory {
    pub template: StructureTemplate,
    pub values: BTreeMap<String, Value>,
    pub evidence: BTreeMap<String, String>,
    /// Retained when an owner subsequently corrects the summary using another source.
    pub source_artifact_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureDiagnostic {
    pub field_id: String,
    pub code: String,
    pub severity: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureValidation {
    pub valid: bool,
    pub engine: String,
    pub model: Option<String>,
    pub template_id: String,
    pub template_revision: u64,
    pub diagnostics: Vec<StructureDiagnostic>,
}
impl StructureTemplate {
    pub fn validate_definition(&self) -> Result<()> {
        let invalid = || NovaError::Validation("invalid structure definition".into());
        if self.title.trim().is_empty()
            || self.title.len() > 240
            || self.guidance.len() > 2000
            || self.fields.len() > 32
        {
            return Err(invalid());
        }
        let mut ids = BTreeSet::new();
        for f in &self.fields {
            if f.id.is_empty()
                || f.id.len() > 64
                || !f.id.as_bytes()[0].is_ascii_lowercase()
                || !f
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                || [
                    "owner",
                    "owner_id",
                    "source",
                    "source_id",
                    "source_artifact_id",
                    "status",
                    "revision",
                    "version",
                    "schema_version",
                    "permissions",
                    "permission_scope",
                    "privacy_level",
                    "receipt",
                    "authorization",
                    "confirmed",
                    "memory_id",
                    "proposal_id",
                    "template_id",
                    "source_artifact_ids",
                    "receipt_id",
                    "purpose_limits",
                    "model_eligibility",
                    "assertion_origin",
                    "proposed_origin",
                    "review_tier",
                    "review_revision",
                    "created_at",
                    "updated_at",
                    "observed_at",
                    "valid_from",
                    "valid_to",
                    "deleted_at",
                    "content_hash",
                    "confidence",
                    "permission",
                    "approval_id",
                ]
                .contains(&f.id.as_str())
                || !ids.insert(&f.id)
                || f.label.trim().is_empty()
                || f.label.len() > 120
                || f.description.len() > 1000
                || f.max_length.is_some_and(|n| n == 0 || n > 8000)
                || f.minimum.is_some_and(|n| !n.is_finite())
                || f.maximum.is_some_and(|n| !n.is_finite())
                || matches!((f.minimum, f.maximum), (Some(a), Some(b)) if a > b)
            {
                return Err(invalid());
            }
            let choices = matches!(
                f.kind,
                StructureFieldKind::Enum | StructureFieldKind::Multiselect
            );
            if choices != !f.options.is_empty()
                || f.options.len() > 50
                || f.options
                    .iter()
                    .any(|s| s.trim().is_empty() || s.len() > 120)
                || f.options.iter().collect::<BTreeSet<_>>().len() != f.options.len()
                || (f.kind != StructureFieldKind::Number
                    && (f.minimum.is_some() || f.maximum.is_some()))
                || (!matches!(
                    f.kind,
                    StructureFieldKind::Text | StructureFieldKind::LongText
                ) && f.max_length.is_some())
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
    /// Pure validation of caller-provided values. Null is an explicitly unknown value,
    /// including for required fields; omitted required fields are incomplete input.
    pub fn validate_sample(
        &self,
        source: &str,
        values: &BTreeMap<String, Value>,
        evidence: &BTreeMap<String, String>,
    ) -> Result<StructureValidation> {
        self.validate_definition()?;
        if source.len() > 65536
            || values.len() > 32
            || evidence.len() > 32
            || serde_json::to_vec(values)?.len() > 65536
            || evidence.values().any(|s| s.len() > 8000)
        {
            return Err(NovaError::Validation("structure sample bounds".into()));
        }
        let mut diagnostics = vec![];
        let mut add = |id: &str, code: &str, severity: &str| {
            diagnostics.push(StructureDiagnostic {
                field_id: id.into(),
                code: code.into(),
                severity: severity.into(),
            })
        };
        for id in values
            .keys()
            .chain(evidence.keys())
            .collect::<BTreeSet<_>>()
        {
            if !self.fields.iter().any(|f| &f.id == id) {
                add(id, "unknown_field", "error");
            }
        }
        for f in &self.fields {
            let Some(v) = values.get(&f.id) else {
                if f.required {
                    add(&f.id, "required_field_missing", "error");
                } else {
                    add(&f.id, "optional_field_missing", "info");
                }
                continue;
            };
            if v.is_null() {
                add(&f.id, "value_unknown", "warning");
                if evidence.contains_key(&f.id) {
                    add(&f.id, "evidence_without_value", "error");
                }
                continue;
            }
            let valid = match f.kind {
                StructureFieldKind::Text | StructureFieldKind::LongText => {
                    v.as_str().is_some_and(|s| {
                        !s.trim().is_empty()
                            && s.chars().count()
                                <= f.max_length
                                    .unwrap_or(if f.kind == StructureFieldKind::Text {
                                        500
                                    } else {
                                        8000
                                    })
                    })
                }
                StructureFieldKind::Date => v.as_str().is_some_and(valid_date),
                StructureFieldKind::Number => v.as_f64().is_some_and(|n| {
                    n.is_finite()
                        && f.minimum.is_none_or(|min| n >= min)
                        && f.maximum.is_none_or(|max| n <= max)
                }),
                StructureFieldKind::Enum => {
                    v.as_str().is_some_and(|s| f.options.iter().any(|o| o == s))
                }
                StructureFieldKind::Multiselect => v.as_array().is_some_and(|a| {
                    a.len() <= f.options.len()
                        && a.iter()
                            .all(|v| v.as_str().is_some_and(|s| f.options.iter().any(|o| o == s)))
                        && a.iter()
                            .filter_map(Value::as_str)
                            .collect::<BTreeSet<_>>()
                            .len()
                            == a.len()
                }),
            };
            if !valid {
                add(&f.id, "invalid_value", "error");
            }
            match evidence.get(&f.id) {
                Some(quote) if quote.trim().is_empty() || !source.contains(quote.as_str()) => {
                    add(&f.id, "evidence_not_in_source", "error")
                }
                Some(_) => add(&f.id, "quotation_found_semantics_unverified", "info"),
                None if f.source_required => add(&f.id, "source_required", "error"),
                None => add(&f.id, "no_source_quotation", "warning"),
            }
        }
        for id in evidence.keys() {
            if !values.contains_key(id) {
                add(id, "evidence_without_value", "error");
            }
        }
        Ok(StructureValidation {
            valid: !diagnostics.iter().any(|d| d.severity == "error"),
            engine: "manual_validation".into(),
            model: None,
            template_id: self.template_id.clone(),
            template_revision: self.revision,
            diagnostics,
        })
    }
}
fn valid_date(s: &str) -> bool {
    if s.len() != 10
        || !s.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return false;
    }
    let year = s[..4].parse::<i32>().unwrap_or(0);
    let month = s[5..7].parse::<u8>().unwrap_or(0);
    let day = s[8..].parse::<u8>().unwrap_or(0);
    time::Month::try_from(month)
        .ok()
        .is_some_and(|m| time::Date::from_calendar_date(year, m, day).is_ok())
}
