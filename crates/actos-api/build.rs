//! Derleme anında git SHA'sını binary'ye gömer (`GET /version` için).

use std::process::Command;

fn main() {
    // Konteyner derlemesinde `.git` yok (bkz. `.dockerignore` — build
    // context'e girmesi hem gereksiz hem de imaja sızma riski). O yüzden
    // ortamdan verilebiliyor: Dockerfile `ARG ACTOS_GIT_SHA` alıyor, CI de
    // commit SHA'sını oraya geçiriyor. Bu olmadan dağıtılmış bir sunucuda
    // `GET /version` "unknown" der ve "hangi commit canlıda?" sorusunun
    // cevabı kaybolur.
    let sha = std::env::var("ACTOS_GIT_SHA")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(git_sha)
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=ACTOS_GIT_SHA={sha}");
    println!("cargo:rerun-if-env-changed=ACTOS_GIT_SHA");
    // Yeni commit atıldığında build script yeniden çalışsın.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
}

fn git_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!sha.is_empty()).then_some(sha)
}
