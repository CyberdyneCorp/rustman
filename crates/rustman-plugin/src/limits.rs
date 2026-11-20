//! Per-call resource limits (plugins spec, "Resource limits"; design D4).

use std::time::Duration;

use serde::Deserialize;

use crate::error::LoadError;

/// The execution time budget of one call (wall time spent running the
/// plugin, host calls excluded) when the workspace sets none.
pub const DEFAULT_CPU_MS: u64 = 100;
/// The linear memory limit when the workspace sets none.
pub const DEFAULT_MEMORY_MIB: u64 = 64;
/// The largest execution time budget a workspace may set.
pub const MAX_CPU_MS: u64 = 10_000;
/// The largest memory limit a workspace may set.
pub const MAX_MEMORY_MIB: u64 = 1024;

/// The limits of every call into one plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub cpu_ms: u64,
    pub memory_mib: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            cpu_ms: DEFAULT_CPU_MS,
            memory_mib: DEFAULT_MEMORY_MIB,
        }
    }
}

/// `limits = { cpu_ms, memory_mib }` as written; missing keys take the
/// defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsTable {
    pub cpu_ms: Option<u64>,
    pub memory_mib: Option<u64>,
}

impl LimitsTable {
    /// The limits, refusing zero and values above the maximums.
    pub fn validate(self, plugin: &str) -> Result<Limits, LoadError> {
        let limits = Limits {
            cpu_ms: self.cpu_ms.unwrap_or(DEFAULT_CPU_MS),
            memory_mib: self.memory_mib.unwrap_or(DEFAULT_MEMORY_MIB),
        };
        if limits.cpu_ms > MAX_CPU_MS {
            let message = format!("execution time limit exceeds maximum ({MAX_CPU_MS} ms)");
            return Err(LoadError::invalid(plugin, message));
        }
        if limits.memory_mib > MAX_MEMORY_MIB {
            let message = format!("memory limit exceeds maximum ({MAX_MEMORY_MIB} MiB)");
            return Err(LoadError::invalid(plugin, message));
        }
        if limits.cpu_ms == 0 || limits.memory_mib == 0 {
            return Err(LoadError::invalid(plugin, "limits must be at least 1"));
        }
        Ok(limits)
    }
}

impl Limits {
    pub fn cpu(self) -> Duration {
        Duration::from_millis(self.cpu_ms)
    }

    pub fn memory_bytes(self) -> usize {
        usize::try_from(self.memory_mib * 1024 * 1024).unwrap_or(usize::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(cpu_ms: Option<u64>, memory_mib: Option<u64>) -> LimitsTable {
        LimitsTable { cpu_ms, memory_mib }
    }

    #[test]
    fn defaults_apply_to_missing_keys() {
        assert_eq!(table(None, None).validate("p").unwrap(), Limits::default());
        let raised = table(Some(10_000), Some(1024)).validate("p").unwrap();
        assert_eq!(raised.cpu(), Duration::from_secs(10));
        assert_eq!(raised.memory_bytes(), 1024 * 1024 * 1024);
    }

    #[test]
    fn limit_above_maximum() {
        assert_eq!(
            table(None, Some(4096))
                .validate("p")
                .unwrap_err()
                .to_string(),
            "plugin p: memory limit exceeds maximum (1024 MiB)"
        );
        assert_eq!(
            table(Some(10_001), None)
                .validate("p")
                .unwrap_err()
                .to_string(),
            "plugin p: execution time limit exceeds maximum (10000 ms)"
        );
        assert!(table(Some(0), None).validate("p").is_err());
    }
}
