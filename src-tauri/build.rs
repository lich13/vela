fn main() {
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads/main");
    let revision = std::env::var("GITHUB_SHA").ok().or_else(|| {
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
    });
    println!(
        "cargo:rustc-env=VELA_REVISION={}",
        revision.as_deref().unwrap_or("unknown").trim()
    );
    build_power();
    build_quick();
    tauri_build::build()
}

fn build_quick() {
    println!("cargo:rerun-if-changed=native/quick_panel.swift");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let run = |program: &str, args: &[&str]| {
        let result = std::process::Command::new(program)
            .args(args)
            .output()
            .expect("native quick panel build tool");
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        result
    };
    let target = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64-apple-macosx12.0",
        _ => "arm64-apple-macosx12.0",
    };
    let sdk = String::from_utf8(run("xcrun", &["--sdk", "macosx", "--show-sdk-path"]).stdout)
        .expect("macosx sdk path");
    let sdk = sdk.trim();
    let swiftc =
        String::from_utf8(run("xcrun", &["--find", "swiftc"]).stdout).expect("swiftc path");
    let swift_lib = std::path::Path::new(swiftc.trim())
        .parent()
        .and_then(std::path::Path::parent)
        .expect("toolchain usr")
        .join("lib/swift/macosx");
    let lib = out.join("libvela_quick.a");
    run(
        "xcrun",
        &[
            "swiftc",
            "-emit-library",
            "-static",
            "-O",
            "-whole-module-optimization",
            "-parse-as-library",
            "-swift-version",
            "5",
            "-target",
            target,
            "-sdk",
            sdk,
            "native/quick_panel.swift",
            "-o",
            lib.to_str().unwrap(),
        ],
    );
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-search=native={}", swift_lib.display());
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    println!("cargo:rustc-link-lib=static=vela_quick");
    let compat56 = swift_lib.join("libswiftCompatibility56.a");
    if compat56.exists() {
        println!(
            "cargo:rustc-link-arg=-Wl,-force_load,{}",
            compat56.display()
        );
    }
    if swift_lib.join("libswiftCompatibilityPacks.a").exists() {
        println!("cargo:rustc-link-lib=static=swiftCompatibilityPacks");
    }
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=SwiftUI");
}

fn build_power() {
    println!("cargo:rerun-if-changed=native/power_client.m");
    println!("cargo:rerun-if-changed=native/power_helper.m");
    println!("cargo:rerun-if-changed=native/power_install.h");
    println!("cargo:rerun-if-changed=native/power_storage_tests.h");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let helper = out.join("power-helper");
    let object = out.join("power-client.o");
    let run = |program: &str, args: &[&str]| {
        let result = std::process::Command::new(program)
            .args(args)
            .output()
            .expect("native power build tool");
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        result
    };
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64",
        _ => "arm64",
    };
    let common = [
        "clang",
        "-arch",
        arch,
        "-mmacosx-version-min=12.0",
        "-fobjc-arc",
        "-fblocks",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-Wno-unused-parameter",
        "-Wno-incompatible-pointer-types",
    ];
    let mut args = common.to_vec();
    args.extend([
        "native/power_helper.m",
        "-framework",
        "Foundation",
        "-framework",
        "Security",
        "-o",
        helper.to_str().unwrap(),
    ]);
    run("xcrun", &args);
    run(
        "/usr/bin/codesign",
        &[
            "--force",
            "--sign",
            "-",
            "--identifier",
            "com.lich13.vela.power-helper",
            helper.to_str().unwrap(),
        ],
    );
    let info = run(
        "/usr/bin/codesign",
        &["-dv", "--verbose=4", helper.to_str().unwrap()],
    );
    let stderr = String::from_utf8_lossy(&info.stderr);
    let hash = stderr
        .lines()
        .find_map(|l| l.strip_prefix("CDHash="))
        .expect("helper CDHash");
    println!("cargo:rustc-env=VELA_POWER_HELPER={}", helper.display());
    println!("cargo:rustc-env=VELA_POWER_HASH={hash}");
    let mut args = common.to_vec();
    args.extend([
        "-c",
        "native/power_client.m",
        "-o",
        object.to_str().unwrap(),
    ]);
    run("xcrun", &args);
    run(
        "ar",
        &[
            "rcs",
            out.join("libvela_power.a").to_str().unwrap(),
            object.to_str().unwrap(),
        ],
    );
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=vela_power");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=Security");
    // Expose a safe, standalone simulator for the native helper logic to cargo tests.
    let test = out.join("power-helper-tests");
    if std::env::var("PROFILE").as_deref() != Ok("release") {
        let mut args = common.to_vec();
        args.extend([
            "-DPOWER_TEST",
            "-Wno-unused-function",
            "-Wno-unused-variable",
            "native/power_helper.m",
            "-framework",
            "Foundation",
            "-framework",
            "Security",
            "-o",
            test.to_str().unwrap(),
        ]);
        run("xcrun", &args);
    }
    println!("cargo:rustc-env=VELA_POWER_TEST={}", test.display());
}
