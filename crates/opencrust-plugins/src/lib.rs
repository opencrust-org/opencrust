pub mod loader;
pub mod manifest;
pub mod runtime;
pub mod traits;

pub use loader::{PluginLoader, PluginRegistry};
pub use manifest::PluginManifest;
pub use runtime::WasmRuntime;
pub use traits::{Capability, Plugin, PluginInput, PluginOutput};
