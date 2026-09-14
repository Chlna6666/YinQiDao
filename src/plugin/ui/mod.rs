//! Runtime-neutral plugin UI extension contracts.
//!
//! This layer owns only validated contribution metadata and declarative view/theme models. It must
//! not depend on GPUI or Wasmtime. GPUI renders validated snapshots in `src/ui`; the Component
//! adapter converts generated WIT values into these semantic types before they enter the registry.

pub(crate) mod manifest;
pub(crate) mod registry;
pub(crate) mod schema;
pub(crate) mod theme;
