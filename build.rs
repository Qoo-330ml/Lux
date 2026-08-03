fn main() {
    println!("cargo:rerun-if-changed=migrations");
    println!("cargo:rerun-if-changed=web/dist");
    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    println!("cargo:rerun-if-changed=.git/packed-refs");

    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            if let Ok(commit) = String::from_utf8(output.stdout) {
                let commit = commit.trim();
                if !commit.is_empty() {
                    println!("cargo:rustc-env=LUX_GIT_COMMIT={commit}");
                }
            }
        }
    }
}
