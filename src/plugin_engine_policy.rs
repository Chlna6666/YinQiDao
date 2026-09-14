use std::time::Duration;

use anyhow::{Result, bail};

const MIB: usize = 1024 * 1024;
const GIB: usize = 1024 * MIB;

/// Runtime-neutral Wasmtime Engine/Store policy.
///
/// This module intentionally contains no Wasmtime types. The eventual engine adapter maps these
/// limits into `Config`, `StoreLimitsBuilder`, fuel and epoch settings so security policy remains
/// reviewable without depending on generated bindings or Wasmtime API details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginEnginePolicy {
    /// Maximum linear-memory bytes available to one plugin Store.
    pub max_memory_bytes: usize,
    /// Maximum total table elements available to one Store.
    pub max_table_elements: usize,
    /// Maximum number of core memories created inside one Store.
    pub max_memories: usize,
    /// Maximum number of core tables created inside one Store.
    pub max_tables: usize,
    /// Maximum number of core/component instances owned by one Store.
    pub max_instances: usize,
    /// Maximum warm instances retained for one plugin/provider route.
    pub max_pooled_instances_per_route: usize,
    /// Fuel replenished for one guest export call when Wasmtime fuel accounting is enabled.
    pub fuel_per_call: u64,
    /// Host epoch increment cadence. This is independent of the GPUI/realtime audio thread.
    pub epoch_tick_interval: Duration,
    /// Deadline applied to one guest export. Runtime-neutral Host timeout uses the same target.
    pub guest_call_deadline: Duration,
    /// Hard limit for one serialized compiled component artifact before it is accepted into cache.
    pub max_compiled_artifact_bytes: usize,
}

impl Default for PluginEnginePolicy {
    fn default() -> Self {
        Self {
            // Music-service plugins exchange metadata/JSON and descriptors, not decoded PCM or
            // artwork/audio blobs. 64 MiB leaves headroom without making one plugin a memory sink.
            max_memory_bytes: 64 * MIB,
            max_table_elements: 16 * 1024,
            max_memories: 2,
            max_tables: 4,
            max_instances: 32,
            max_pooled_instances_per_route: 2,
            fuel_per_call: 50_000_000,
            epoch_tick_interval: Duration::from_millis(10),
            guest_call_deadline: Duration::from_secs(30),
            max_compiled_artifact_bytes: 256 * MIB,
        }
    }
}

impl PluginEnginePolicy {
    /// Validate policy before constructing a Wasmtime Engine. Values are intentionally bounded even
    /// before settings/UI expose them, preventing accidental future configuration from effectively
    /// disabling the sandbox.
    pub fn validate(&self) -> Result<()> {
        if self.max_memory_bytes == 0 || self.max_memory_bytes > 512 * MIB {
            bail!("插件 Store memory limit 必须在 1..=512 MiB");
        }
        if self.max_table_elements == 0 || self.max_table_elements > 1_000_000 {
            bail!("插件 Store table element limit 非法");
        }
        if self.max_memories == 0 || self.max_memories > 16 {
            bail!("插件 Store memory count limit 非法");
        }
        if self.max_tables == 0 || self.max_tables > 32 {
            bail!("插件 Store table count limit 非法");
        }
        if self.max_instances == 0 || self.max_instances > 256 {
            bail!("插件 Store instance count limit 非法");
        }
        if self.max_pooled_instances_per_route == 0
            || self.max_pooled_instances_per_route > self.max_instances
        {
            bail!("插件 route 实例池上限必须在 1..=max_instances");
        }
        if self.fuel_per_call == 0 {
            bail!("插件 guest call fuel 不能为 0");
        }
        if self.epoch_tick_interval.is_zero() {
            bail!("插件 epoch tick interval 不能为 0");
        }
        if self.guest_call_deadline.is_zero()
            || self.guest_call_deadline < self.epoch_tick_interval
        {
            bail!("插件 guest call deadline 必须至少覆盖一个 epoch tick");
        }
        if self.max_compiled_artifact_bytes == 0 || self.max_compiled_artifact_bytes > GIB {
            bail!("插件 compiled artifact limit 必须在 1 byte..=1 GiB");
        }
        Ok(())
    }

    /// Number of Host epoch increments allowed before one guest call reaches its deadline.
    /// Rounds up so sub-tick remainder never shortens the configured wall-clock budget.
    pub fn epoch_deadline_ticks(&self) -> u64 {
        let tick_ns = self.epoch_tick_interval.as_nanos().max(1);
        let deadline_ns = self.guest_call_deadline.as_nanos();
        let ticks = deadline_ns.div_ceil(tick_ns).max(1);
        ticks.min(u128::from(u64::MAX)) as u64
    }

    pub fn max_memory_mib(&self) -> usize {
        self.max_memory_bytes / MIB
    }

    pub fn max_compiled_artifact_mib(&self) -> usize {
        self.max_compiled_artifact_bytes / MIB
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_valid_and_deadline_rounds_up() {
        let policy = PluginEnginePolicy::default();
        policy.validate().expect("default policy");
        assert_eq!(policy.epoch_deadline_ticks(), 3_000);
    }

    #[test]
    fn epoch_deadline_never_rounds_down() {
        let policy = PluginEnginePolicy {
            epoch_tick_interval: Duration::from_millis(7),
            guest_call_deadline: Duration::from_millis(20),
            ..PluginEnginePolicy::default()
        };
        assert_eq!(policy.epoch_deadline_ticks(), 3);
    }

    #[test]
    fn invalid_pool_size_is_rejected() {
        let policy = PluginEnginePolicy {
            max_instances: 1,
            max_pooled_instances_per_route: 2,
            ..PluginEnginePolicy::default()
        };
        assert!(policy.validate().is_err());
    }
}
