use std::process::Command;

/// "2026-11-07" as "7 Nov 2026" (in English, whatever the system's language). Anything else is
/// returned as it is.
fn readable_date(iso: &str) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let parts: Vec<&str> = iso.split('-').collect();
    match parts[..] {
        [year, month, day] => match (month.parse::<usize>(), day.parse::<u32>()) {
            (Ok(m @ 1..=12), Ok(d)) => format!("{d} {} {year}", MONTHS[m - 1]),
            _ => iso.to_string(),
        },
        _ => iso.to_string(),
    }
}

fn main() {
    // The UI, compiled with Slint's Cosmic widget style.
    let config = slint_build::CompilerConfiguration::new().with_style("cosmic".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint UI failed to compile");

    // "445e6ee · 17 Nov 2026": the commit this was built from and its date, for the About
    // dialog. "dev" outside a git checkout.
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let build = match (git(&["rev-parse", "--short", "HEAD"]), git(&["log", "-1", "--format=%cs"])) {
        (Some(commit), Some(date)) => format!("{commit} · {}", readable_date(&date)),
        _ => "dev".to_string(),
    };
    println!("cargo:rustc-env=GIO_BUILD={build}");
    // Rebuild the stamp when the checked-out commit changes.
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/refs/heads");
    }

    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows") && target.contains("msvc") {
        // Embed the application manifest (per-monitor DPI awareness) with the MSVC linker.
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res/app.manifest");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());

        embed_icon_and_version();
    }
}

/// The exe's icon (the Gravingine logo, drawn by src/logo.rs, so Explorer, the taskbar
/// pin and shortcuts show it) and its version information (file Properties > Details).
fn embed_icon_and_version() {
    println!("cargo:rerun-if-changed=src/logo.rs");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out.join("app.ico");
    std::fs::write(&ico, logo::ico(&[16, 20, 24, 32, 40, 48, 64, 96, 128, 256])).unwrap();

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let numbers: Vec<u16> = version.split(['.', '-', '+']).filter_map(|p| p.parse().ok()).chain([0; 4]).take(4).collect();
    let comma = numbers.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
    // ASCII only: the resource compiler reads this file in the system code page.
    let rc = format!(
        r#"1 ICON "{ico}"

1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Gravitide Pvt Ltd"
      VALUE "FileDescription", "Gravingine Image Optimizer"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "gravingine-image-optimizer"
      VALUE "LegalCopyright", "Copyright (c) 2026 Gravitide Pvt Ltd. MIT license."
      VALUE "OriginalFilename", "gravingine-image-optimizer.exe"
      VALUE "ProductName", "Gravingine Image Optimizer"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = ico.display().to_string().replace('\\', "/"),
    );
    let rc_path = out.join("app.rc");
    std::fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_optional().unwrap();
}

#[path = "src/logo.rs"]
#[allow(dead_code)]
mod logo;
