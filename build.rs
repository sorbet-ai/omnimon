// The version string: the package version, plus a build tag when
// `<NAME>_BUILD` is set (CI sets OMNIMON_BUILD=nightly.<sha> for nightly
// builds), so `--version`, `--json` and recordings say which commit they are.
fn main() {
    let name = std::env::var("CARGO_PKG_NAME").unwrap().to_uppercase().replace('-', "_");
    let var = format!("{name}_BUILD");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed={var}");
    let mut version = std::env::var("CARGO_PKG_VERSION").unwrap();
    if let Ok(tag) = std::env::var(&var)
        && !tag.is_empty()
    {
        version = format!("{version}+{tag}");
    }
    println!("cargo:rustc-env=APP_VERSION={version}");
}
