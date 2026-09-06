use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=assets/windows/app.rc");
    println!("cargo:rerun-if-changed=assets/windows/icon.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must provide CARGO_MANIFEST_DIR"),
    );
    let out_dir =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"));
    let rc_file = manifest_dir.join("assets/windows/app.rc");
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let resource = match target_env.as_str() {
        "msvc" => compile_msvc_resource(&manifest_dir, &rc_file, &out_dir, &target_arch),
        "gnullvm" => compile_llvm_resource(&manifest_dir, &rc_file, &out_dir),
        _ => compile_gnu_resource(&manifest_dir, &rc_file, &out_dir, &target_arch),
    }
    .unwrap_or_else(|error| panic!("failed to compile Windows application resources: {error}"));

    println!(
        "cargo:rustc-link-arg-bin=yin_qi_dao={}",
        resource.display()
    );
}

fn compile_msvc_resource(
    manifest_dir: &Path,
    rc_file: &Path,
    out_dir: &Path,
    target_arch: &str,
) -> Result<PathBuf, String> {
    let output = out_dir.join("yin_qi_dao.res");
    let mut compilers = vec![PathBuf::from("rc.exe")];
    compilers.extend(find_windows_sdk_rc(target_arch));

    let mut errors = Vec::new();
    for compiler in compilers {
        match Command::new(&compiler)
            .current_dir(manifest_dir)
            .arg("/nologo")
            .arg(format!("/fo{}", output.display()))
            .arg(rc_file)
            .status()
        {
            Ok(status) if status.success() => return Ok(output),
            Ok(status) => errors.push(format!(
                "{} exited with {status}",
                compiler.display()
            )),
            Err(error) => errors.push(format!("{}: {error}", compiler.display())),
        }
    }

    compile_llvm_resource(manifest_dir, rc_file, out_dir).map_err(|llvm_error| {
        format!(
            "RC.EXE unavailable ({}) and llvm-rc fallback failed ({llvm_error})",
            errors.join("; ")
        )
    })
}

fn compile_llvm_resource(
    manifest_dir: &Path,
    rc_file: &Path,
    out_dir: &Path,
) -> Result<PathBuf, String> {
    let output = out_dir.join("yin_qi_dao.res");
    let status = Command::new("llvm-rc")
        .current_dir(manifest_dir)
        .arg("/nologo")
        .arg(format!("/fo{}", output.display()))
        .arg(rc_file)
        .status()
        .map_err(|error| format!("failed to start llvm-rc: {error}"))?;

    if status.success() {
        Ok(output)
    } else {
        Err(format!("llvm-rc exited with {status}"))
    }
}

fn compile_gnu_resource(
    manifest_dir: &Path,
    rc_file: &Path,
    out_dir: &Path,
    target_arch: &str,
) -> Result<PathBuf, String> {
    let output = out_dir.join("yin_qi_dao-resource.o");
    let mut compilers = Vec::new();
    match target_arch {
        "x86_64" => compilers.push("x86_64-w64-mingw32-windres"),
        "x86" => compilers.push("i686-w64-mingw32-windres"),
        "aarch64" => compilers.push("aarch64-w64-mingw32-windres"),
        _ => {}
    }
    compilers.push("windres");

    let mut errors = Vec::new();
    for compiler in compilers {
        match Command::new(compiler)
            .current_dir(manifest_dir)
            .arg("--input")
            .arg(rc_file)
            .arg("--output")
            .arg(&output)
            .arg("--output-format=coff")
            .status()
        {
            Ok(status) if status.success() => return Ok(output),
            Ok(status) => errors.push(format!("{compiler} exited with {status}")),
            Err(error) => errors.push(format!("{compiler}: {error}")),
        }
    }

    Err(format!("no working windres compiler found: {}", errors.join("; ")))
}

fn find_windows_sdk_rc(target_arch: &str) -> Vec<PathBuf> {
    let arch = match target_arch {
        "x86_64" => "x64",
        "x86" => "x86",
        "aarch64" => "arm64",
        _ => target_arch,
    };

    let mut roots = Vec::new();
    for variable in ["WindowsSdkVerBinPath", "WindowsSdkBinPath"] {
        if let Some(path) = env::var_os(variable) {
            roots.push(PathBuf::from(path));
        }
    }
    if let Some(program_files) = env::var_os("ProgramFiles(x86)") {
        roots.push(
            PathBuf::from(program_files)
                .join("Windows Kits")
                .join("10")
                .join("bin"),
        );
    }

    let mut candidates = Vec::new();
    for root in roots {
        let direct = root.join(arch).join("rc.exe");
        if direct.is_file() {
            candidates.push(direct);
        }

        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        let mut version_dirs = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        version_dirs.sort_by(|left, right| right.cmp(left));

        for version_dir in version_dirs {
            let candidate = version_dir.join(arch).join("rc.exe");
            if candidate.is_file() {
                candidates.push(candidate);
            }
        }
    }

    candidates
}
