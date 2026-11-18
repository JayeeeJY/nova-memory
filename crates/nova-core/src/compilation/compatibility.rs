use super::*;
use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractTarget {
    CompilationV01,
    LegacyCanonicalV02,
    LegacyPortableV04,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityDisposition {
    Supported,
    OpaqueRoundTripOnly,
    Rejected,
}

/// Consumer declarations must come from a trusted adapter, never model output.
#[derive(Debug, Clone, Default)]
pub struct ConsumerCapabilities {
    pub modules: BTreeSet<ModuleRef>,
    pub extension_keys: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityReport {
    pub target: ContractTarget,
    pub disposition: CompatibilityDisposition,
    pub unknown_modules: BTreeSet<ModuleRef>,
    pub opaque_extension_keys: BTreeSet<String>,
    pub reasons: BTreeSet<String>,
}

impl CompilationSnapshot {
    /// Assess representation compatibility only; this grants neither read access
    /// nor permission to activate a module, import, disclose or run its content.
    pub fn compatibility_for(
        &self,
        expected: &AssetScope,
        target: ContractTarget,
        consumer: &ConsumerCapabilities,
    ) -> Result<CompatibilityReport> {
        self.validate(expected)?;
        let unknown_modules: BTreeSet<_> = self
            .modules
            .iter()
            .map(|m| m.module.clone())
            .chain(self.units.iter().flat_map(|u| u.modules.iter().cloned()))
            .filter(|m| !consumer.modules.contains(m))
            .collect();
        let opaque_extension_keys: BTreeSet<_> = self
            .units
            .iter()
            .flat_map(|u| u.extensions.keys())
            .filter(|key| !consumer.extension_keys.contains(*key))
            .cloned()
            .collect();
        let mut reasons = BTreeSet::new();
        let disposition = if target != ContractTarget::CompilationV01 {
            reasons.insert("no_automatic_legacy_conversion".into());
            reasons.insert("automatic_processing_is_not_user_confirmation".into());
            if self
                .units
                .iter()
                .any(|u| !u.content.conditions.is_empty() || u.valid_time != ValidTime::Unknown)
            {
                reasons.insert("time_or_conditions_require_explicit_mapping".into());
            }
            if !self.relations.is_empty() {
                reasons.insert("relations_require_explicit_mapping".into());
            }
            if !unknown_modules.is_empty() || !opaque_extension_keys.is_empty() {
                reasons.insert("extensions_cannot_be_silently_dropped".into());
            }
            CompatibilityDisposition::Rejected
        } else if !unknown_modules.is_empty() || !opaque_extension_keys.is_empty() {
            reasons.insert("unknown_semantics_preserved_as_inert_data".into());
            CompatibilityDisposition::OpaqueRoundTripOnly
        } else {
            CompatibilityDisposition::Supported
        };
        Ok(CompatibilityReport {
            target,
            disposition,
            unknown_modules,
            opaque_extension_keys,
            reasons,
        })
    }
}
