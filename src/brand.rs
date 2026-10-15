//! Everything that spells out the app's name goes through here, so renaming
//! the app is a one-line change to `name` in Cargo.toml.

/// Binary / package name, e.g. `nvtui`.
pub const APP_NAME: &str = env!("CARGO_PKG_NAME");
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Name of an environment variable namespaced to this app: `env_var("NVML_PATH")`
/// gives `NVTUI_NVML_PATH`.
pub fn env_var(suffix: &str) -> String {
    format!("{}_{}", APP_NAME.to_uppercase().replace('-', "_"), suffix)
}

/// Read an app-namespaced environment variable.
pub fn env(suffix: &str) -> Option<String> {
    std::env::var(env_var(suffix)).ok().filter(|v| !v.is_empty())
}

/// Prometheus metric namespace. Defaults to the app name, but dashboards key
/// on it, so it can be pinned here independently of a rename.
pub const METRIC_NS: &str = env!("CARGO_PKG_NAME");
