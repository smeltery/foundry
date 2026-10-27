use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=FOUNDRY_BUILD_VERSION");
    let version = env::var("FOUNDRY_BUILD_VERSION")
        .unwrap_or_else(|_| env::var("CARGO_PKG_VERSION").expect("Cargo package version"));
    let parts: Vec<_> = version.split('.').collect();
    assert!(
        parts.len() == 3
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.bytes().all(|byte| byte.is_ascii_digit())
                    && (part.len() == 1 || !part.starts_with('0'))
                    && part.parse::<u64>().is_ok()
            }),
        "FOUNDRY_BUILD_VERSION must be a stable major.minor.patch version"
    );
    println!("cargo:rustc-env=FOUNDRY_VERSION={version}");
}
