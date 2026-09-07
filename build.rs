use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must provide CARGO_MANIFEST_DIR"),
    );
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"));

    generate_lucide_assets(&manifest_dir, &out_dir)
        .unwrap_or_else(|error| panic!("failed to generate Lucide assets: {error}"));

    println!("cargo:rerun-if-changed=assets/windows/app.rc");
    println!("cargo:rerun-if-changed=assets/windows/icon.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

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

fn generate_lucide_assets(manifest_dir: &Path, out_dir: &Path) -> Result<(), String> {
    let src_dir = manifest_dir.join("src");
    let icons_dir = manifest_dir.join("crates/lucide-gpui/icons");
    let mut source_files = Vec::new();
    collect_rust_sources(&src_dir, &mut source_files)?;
    source_files.sort();

    // Watch the directory for newly added source files, and every existing source file for edits.
    println!("cargo:rerun-if-changed={}", src_dir.display());

    let mut icons = BTreeSet::new();
    for source_path in source_files {
        println!("cargo:rerun-if-changed={}", source_path.display());
        let source = fs::read_to_string(&source_path)
            .map_err(|error| format!("failed to read {}: {error}", source_path.display()))?;
        collect_icon_calls(&source, &mut icons);
    }

    let mut generated = String::from("static ASSETS: &[lucide_gpui::StaticAsset] = &[\n");
    for icon in icons {
        let file_name = format!("{}.svg", icon.replace('_', "-"));
        let svg_path = icons_dir.join(&file_name);
        if !svg_path.is_file() {
            return Err(format!(
                "icon!({icon}) maps to missing Lucide asset {}",
                svg_path.display()
            ));
        }

        println!("cargo:rerun-if-changed={}", svg_path.display());
        generated.push_str(&format!(
            "    lucide_gpui::StaticAsset::new(\n        \"lucide/{icon}.svg\",\n        include_bytes!(concat!(\n            env!(\"CARGO_MANIFEST_DIR\"),\n            \"/crates/lucide-gpui/icons/{file_name}\"\n        )),\n    ),\n"
        ));
    }
    generated.push_str(
        "];\n\npub(crate) fn install() {\n    lucide_gpui::install_assets(ASSETS);\n}\n",
    );

    let output_path = out_dir.join("lucide_assets.rs");
    fs::write(&output_path, generated)
        .map_err(|error| format!("failed to write {}: {error}", output_path.display()))
}

fn collect_rust_sources(dir: &Path, sources: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("failed to read source directory {}: {error}", dir.display()))?;

    for entry in entries {
        let entry = entry
            .map_err(|error| format!("failed to read entry in {}: {error}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, sources)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            sources.push(path);
        }
    }

    Ok(())
}

fn collect_icon_calls(source: &str, icons: &mut BTreeSet<String>) {
    let bytes = source.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            index = skip_line_comment(bytes, index + 2);
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            index = skip_block_comment(bytes, index + 2);
            continue;
        }
        if let Some(end) = raw_string_end(bytes, index) {
            index = end;
            continue;
        }
        if bytes[index] == b'"' {
            index = skip_quoted_string(bytes, index + 1);
            continue;
        }
        if matches!(bytes[index], b'b' | b'c') && bytes.get(index + 1) == Some(&b'"') {
            index = skip_quoted_string(bytes, index + 2);
            continue;
        }
        if bytes[index] == b'\''
            && let Some(end) = char_literal_end(source, index)
        {
            index = end;
            continue;
        }

        if is_ident_start(bytes[index]) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_ident_continue(bytes[index]) {
                index += 1;
            }

            if &source[start..index] == "icon"
                && let Some((name, end)) = parse_icon_invocation(source, index)
            {
                icons.insert(name);
                index = end;
            }
            continue;
        }

        index += 1;
    }
}

fn parse_icon_invocation(source: &str, mut index: usize) -> Option<(String, usize)> {
    let bytes = source.as_bytes();
    index = skip_whitespace(bytes, index);
    if bytes.get(index) != Some(&b'!') {
        return None;
    }
    index = skip_whitespace(bytes, index + 1);
    if bytes.get(index) != Some(&b'(') {
        return None;
    }
    index = skip_whitespace(bytes, index + 1);

    let start = index;
    if !bytes.get(index).copied().is_some_and(is_ident_start) {
        return None;
    }
    index += 1;
    while index < bytes.len() && is_ident_continue(bytes[index]) {
        index += 1;
    }
    let name = source[start..index].to_owned();

    index = skip_whitespace(bytes, index);
    if bytes.get(index) != Some(&b')') {
        return None;
    }

    Some((name, index + 1))
}

fn skip_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    index
}

fn skip_line_comment(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    index
}

fn skip_block_comment(bytes: &[u8], mut index: usize) -> usize {
    let mut depth = 1_u32;
    while index + 1 < bytes.len() {
        if bytes[index..].starts_with(b"/*") {
            depth = depth.saturating_add(1);
            index += 2;
        } else if bytes[index..].starts_with(b"*/") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
        } else {
            index += 1;
        }
    }
    bytes.len()
}

fn skip_quoted_string(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index = (index + 2).min(bytes.len()),
            b'"' => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    if bytes.get(index) == Some(&b'b') || bytes.get(index) == Some(&b'c') {
        if bytes.get(index + 1) != Some(&b'r') {
            return None;
        }
        index += 1;
    }
    if bytes.get(index) != Some(&b'r') {
        return None;
    }
    index += 1;

    let mut hashes = 0_usize;
    while bytes.get(index) == Some(&b'#') {
        hashes += 1;
        index += 1;
    }
    if bytes.get(index) != Some(&b'"') {
        return None;
    }
    index += 1;

    while index < bytes.len() {
        if bytes[index] == b'"' {
            let mut end = index + 1;
            let mut matched_hashes = 0;
            while matched_hashes < hashes && bytes.get(end) == Some(&b'#') {
                matched_hashes += 1;
                end += 1;
            }
            if matched_hashes == hashes {
                return Some(end);
            }
        }
        index += 1;
    }

    Some(bytes.len())
}

fn char_literal_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut index = start + 1;

    if bytes.get(index) == Some(&b'\\') {
        index += 1;
        match bytes.get(index).copied()? {
            b'x' => {
                let first = *bytes.get(index + 1)?;
                let second = *bytes.get(index + 2)?;
                if !first.is_ascii_hexdigit() || !second.is_ascii_hexdigit() {
                    return None;
                }
                index += 3;
            }
            b'u' => {
                index += 1;
                if bytes.get(index) != Some(&b'{') {
                    return None;
                }
                index += 1;
                while bytes.get(index).is_some_and(|byte| *byte != b'}') {
                    index += 1;
                }
                if bytes.get(index) != Some(&b'}') {
                    return None;
                }
                index += 1;
            }
            _ => index += 1,
        }
    } else {
        let ch = source.get(index..)?.chars().next()?;
        if matches!(ch, '\n' | '\r' | '\'') {
            return None;
        }
        index += ch.len_utf8();
    }

    (bytes.get(index) == Some(&b'\'')).then_some(index + 1)
}

#[inline]
fn is_ident_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

#[inline]
fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
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
