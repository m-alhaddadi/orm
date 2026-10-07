fn main() { build_record(); }

/// Embed the exact build configuration as `ORM_BUILD_RECORD` JSON for `profile_metadata()`.
fn build_record() {
    use std::{env, path::Path, process::Command};
    let dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let output = |program: &str, args: &[&str]| Command::new(program).args(args).current_dir(&dir).output().ok()
        .filter(|o| o.status.success()).and_then(|o| String::from_utf8(o.stdout).ok()).map(|s| s.trim().to_owned());
    let mut features: Vec<String> = env::vars().filter_map(|(k, _)| k.strip_prefix("CARGO_FEATURE_").map(|f| f.to_lowercase().replace('_', "-"))).collect();
    features.sort();
    let rustc = output(&env::var("RUSTC").unwrap_or_else(|_| "rustc".into()), &["-V"]).unwrap_or_default();
    let revision = env::var("ORM_BUILD_REVISION").ok().or_else(|| output("git", &["rev-parse", "HEAD"])).unwrap_or_else(|| "unknown".into());
    // Rebuild on a new commit; a missing path would rerun this script on every build.
    let mut watched = vec![output("git", &["rev-parse", "--git-path", "HEAD"])];
    if let Some(head) = output("git", &["symbolic-ref", "-q", "HEAD"]) { watched.push(output("git", &["rev-parse", "--git-path", &head])); }
    for path in watched.into_iter().flatten().map(|p| Path::new(&dir).join(p)).filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=ORM_BUILD_REVISION");
    let quoted: Vec<String> = features.iter().map(|f| format!("{f:?}")).collect();
    println!("cargo:rustc-env=ORM_BUILD_RECORD={{\"features\":[{}],\"rustc\":{rustc:?},\"target\":{:?},\"profile\":{:?},\"revision\":{revision:?}}}",
        quoted.join(","), env::var("TARGET").unwrap_or_default(), env::var("PROFILE").unwrap_or_default());
}
