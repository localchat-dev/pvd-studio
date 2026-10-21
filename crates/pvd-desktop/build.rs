use std::{env, path::PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=assets/pvd-studio.rc");
    println!("cargo::rerun-if-changed=assets/pvd-studio.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let target = env::var("TARGET").expect("Cargo must provide TARGET");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let resource = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("pvd-studio.res");
    let mut compiler = find_msvc_tools::find(&target, "rc.exe")
        .expect("pvd-desktop Windows builds require an installed MSVC Windows SDK resource compiler (rc.exe)");
    let status = compiler
        .current_dir(manifest.join("assets"))
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg("pvd-studio.rc")
        .status()
        .expect("failed to run the Windows SDK resource compiler");
    assert!(status.success(), "Windows icon resource compilation failed");
    println!(
        "cargo::rustc-link-arg-bin=pvd-desktop={}",
        resource.display()
    );
}
