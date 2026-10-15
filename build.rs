use std::{env, path::PathBuf, process::Command};

const WALLET_CORE_SHA256: &str = "247ba1f4ef257b6db805a540a7a6ac2e6bdd5138642b98727cadc19e7ecc6fdb";

fn main() {
    assert_eq!(
        env::var("TARGET").as_deref(),
        Ok("aarch64-apple-darwin"),
        "uwlt requires macOS arm64"
    );
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let native = root.join("target/wallet-core-4.8.4");
    let library = native.join("libWalletCore.a");
    println!("cargo:rerun-if-changed={}", library.display());
    println!("cargo:rerun-if-changed=native/vault.m");
    println!("cargo:rerun-if-changed=scripts/setup-wallet-core.sh");
    assert!(
        library.is_file(),
        "Wallet Core 4.8.4 missing; run: bash scripts/setup-wallet-core.sh"
    );
    let digest = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(&library)
        .output()
        .expect("run shasum to authenticate Wallet Core");
    assert!(digest.status.success(), "cannot hash Wallet Core archive");
    let digest = String::from_utf8(digest.stdout).expect("shasum UTF-8 output");
    assert_eq!(
        digest.split_whitespace().next(),
        Some(WALLET_CORE_SHA256),
        "Wallet Core archive checksum mismatch; run: bash scripts/setup-wallet-core.sh"
    );

    cc::Build::new()
        .file("native/vault.m")
        .flag("-fobjc-arc")
        .flag("-fblocks")
        .flag("-mmacosx-version-min=11.0")
        .compile("uwlt_vault");

    println!("cargo:rustc-link-search=native={}", native.display());
    println!("cargo:rustc-link-lib=static=WalletCore");
    println!("cargo:rustc-link-lib=c++");
    println!("cargo:rustc-link-lib=z");
    println!("cargo:rustc-link-lib=objc");
    for framework in ["Foundation", "Security", "LocalAuthentication"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
