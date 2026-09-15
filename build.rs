fn main() {
    println!("cargo:rerun-if-env-changed=MPD_BOT_RELEASE_VERSION");
    let version = std::env::var("MPD_BOT_RELEASE_VERSION")
        .unwrap_or_else(|_| std::env::var("CARGO_PKG_VERSION").expect("Cargo package version"));
    semver::Version::parse(&version)
        .expect("MPD_BOT_RELEASE_VERSION must be a valid semantic version without the v prefix");
    println!("cargo:rustc-env=MPD_BOT_VERSION={version}");
}
