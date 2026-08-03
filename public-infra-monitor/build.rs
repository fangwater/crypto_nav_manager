use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=bpf/network.bpf.c");
    println!("cargo:rerun-if-env-changed=PUBLIC_INFRA_VMLINUX_BTF");
    if env::var_os("CARGO_FEATURE_BPF").is_none() {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let btf = env::var_os("PUBLIC_INFRA_VMLINUX_BTF")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/kernel/btf/vmlinux"));
    let vmlinux = out_dir.join("vmlinux.h");
    let header = Command::new("bpftool")
        .args(["btf", "dump", "file"])
        .arg(&btf)
        .args(["format", "c"])
        .output()
        .expect("run bpftool to generate vmlinux.h");
    if !header.status.success() {
        panic!(
            "bpftool failed to generate vmlinux.h from {}: {}",
            btf.display(),
            String::from_utf8_lossy(&header.stderr)
        );
    }
    fs::write(&vmlinux, header.stdout).expect("write generated vmlinux.h");

    let object = out_dir.join("network.bpf.o");
    let target_arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86",
        Ok("aarch64") => "arm64",
        Ok(other) => panic!("unsupported BPF target architecture {other}"),
        Err(error) => panic!("CARGO_CFG_TARGET_ARCH is unavailable: {error}"),
    };
    let target_arch_define = format!("-D__TARGET_ARCH_{target_arch}");
    let status = Command::new("clang")
        .args([
            "-g",
            "-O2",
            "-target",
            "bpf",
            "-Wall",
            "-Werror",
            &target_arch_define,
            "-I",
        ])
        .arg(&out_dir)
        .args(["-c", "bpf/network.bpf.c", "-o"])
        .arg(&object)
        .status()
        .expect("run clang for BPF object");
    assert!(status.success(), "clang failed to compile network.bpf.c");
}
