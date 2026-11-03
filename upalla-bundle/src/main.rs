//! Builds `upalla-mac` and packages it into a macOS `.app` bundle.
//!
//! This replaces the old `bundle.sh`, which generated the icon by shelling out
//! to Python and Pillow. That made packaging depend on a Python interpreter and
//! a third-party imaging library being installed and importable, which is not a
//! dependency the rest of the project has.
//!
//! Usage (from anywhere in the workspace):
//!
//! ```text
//! cargo run -p upalla-bundle --release -- [--profile dev] [--target TRIPLE] [--skip-build]
//! ```

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use image::{imageops::FilterType, ImageFormat, RgbaImage};

const APP_NAME: &str = "Upalla";
const BIN_NAME: &str = "upalla-mac";
const BUNDLE_ID: &str = "ai.upalla.app";
const DEFAULT_TARGET: &str = "aarch64-apple-darwin";
/// Where `bundle.sh` used to look for a pinned SDK. Still honoured if present,
/// but no longer required: without it we build natively instead of failing.
const LEGACY_SDK: &str = "Code/MacOSX15.5.sdk";

/// icns entry codes paired with the pixel size each holds, as PNG payloads.
///
/// This is exactly the set Apple's own `iconutil` emits with PNG data, checked
/// by encoding an `.iconset` with it and reading back what came out. Only these
/// types take PNG: the small `ic04`/`ic05` slots (16x16 and 32x32 at 1x) hold
/// RLE-compressed ARGB instead, and a PNG written there decodes as noise. The
/// old script's `icp4`/`icp5`/`icp6` entries are not emitted by `iconutil` at
/// all, and it wrote a 64x64 image into `icp6`, which macOS reads as 48x48.
///
/// Leaving out the 1x 16/32 entries costs nothing visible: macOS scales those
/// down from `ic11`, which is the same 32px artwork.
const ICNS_ENTRIES: &[(&[u8; 4], u32)] = &[
    (b"ic11", 32),  // 16x16@2x
    (b"ic12", 64),  // 32x32@2x
    (b"ic07", 128), // 128x128
    (b"ic13", 256), // 128x128@2x
    (b"ic08", 256), // 256x256
];

struct Options {
    profile: String,
    target: String,
    skip_build: bool,
}

impl Options {
    fn parse() -> Result<Self> {
        // PROFILE is still read from the environment so existing muscle memory
        // and any CI invoking the old script keep working.
        let mut opts = Options {
            profile: std::env::var("PROFILE").unwrap_or_else(|_| "release".into()),
            target: DEFAULT_TARGET.into(),
            skip_build: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--profile" => {
                    opts.profile = args.next().context("--profile needs a value")?;
                }
                "--target" => {
                    opts.target = args.next().context("--target needs a value")?;
                }
                "--skip-build" => opts.skip_build = true,
                "-h" | "--help" => {
                    println!(
                        "Usage: cargo run -p upalla-bundle --release -- \\\n  \
                         [--profile <name>] [--target <triple>] [--skip-build]"
                    );
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {other}"),
            }
        }
        Ok(opts)
    }

    /// Cargo puts `dev` builds in `target/<triple>/debug`, not `target/<triple>/dev`.
    fn profile_dir(&self) -> &str {
        if self.profile == "dev" {
            "debug"
        } else {
            &self.profile
        }
    }
}

fn main() -> Result<()> {
    let opts = Options::parse()?;
    let workspace = workspace_dir();
    let bundle = workspace.join("target").join(format!("{APP_NAME}.app"));

    let binary = build_binary(&workspace, &opts)?;

    println!("=== Creating {} ===", bundle.display());
    if bundle.exists() {
        fs::remove_dir_all(&bundle)
            .with_context(|| format!("removing old bundle at {}", bundle.display()))?;
    }
    let macos_dir = bundle.join("Contents/MacOS");
    let resources_dir = bundle.join("Contents/Resources");
    fs::create_dir_all(&macos_dir).context("creating Contents/MacOS")?;
    fs::create_dir_all(&resources_dir).context("creating Contents/Resources")?;

    let dest_binary = macos_dir.join(BIN_NAME);
    fs::copy(&binary, &dest_binary)
        .with_context(|| format!("copying {} into the bundle", binary.display()))?;
    make_executable(&dest_binary)?;

    println!("=== Generating AppIcon.icns ===");
    let icon_source = workspace.join(BIN_NAME).join("src/icon_256.png");
    let icns_path = resources_dir.join("AppIcon.icns");
    let written = write_icns(&icon_source, &icns_path)?;
    println!(
        "  {} ({written} bytes, {} sizes)",
        icns_path.display(),
        ICNS_ENTRIES.len()
    );

    fs::write(bundle.join("Contents/Info.plist"), info_plist()).context("writing Info.plist")?;

    println!("=== Bundle created at {} ===", bundle.display());
    println!("Run it with:  open {}", bundle.display());
    println!("Or directly:  {}", dest_binary.display());
    Ok(())
}

/// The workspace root, derived from this crate's location at compile time so the
/// tool works regardless of the directory it is invoked from.
fn workspace_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory always has a parent")
        .to_path_buf()
}

/// Builds `upalla-mac` and returns the path to the binary.
fn build_binary(workspace: &Path, opts: &Options) -> Result<PathBuf> {
    let binary = workspace
        .join("target")
        .join(&opts.target)
        .join(opts.profile_dir())
        .join(BIN_NAME);

    if opts.skip_build {
        if !binary.exists() {
            bail!(
                "--skip-build was given but {} does not exist; build it first",
                binary.display()
            );
        }
        println!("=== Skipping build, using {} ===", binary.display());
        return Ok(binary);
    }

    // A debug build of the denoiser runs at roughly RTF 1.9 — slower than real
    // time — so the audio thread falls behind and drop_excess() discards frames,
    // which is audible as chopped speech. Release measures around RTF 0.13.
    if opts.profile_dir() == "debug" {
        println!("warning: a debug build cannot keep up with real-time audio");
    }

    let sdk = sdk_root();
    let mut cmd = Command::new("cargo");
    match &sdk {
        // zigbuild pins the SDK so the bundle can target an OS older than this
        // machine's. Without an SDK there is nothing to pin, so an ordinary
        // build is both sufficient and one less tool to have installed.
        Some(sdk) => {
            println!(
                "=== Building {} ({}) with zigbuild, SDKROOT={} ===",
                opts.target,
                opts.profile,
                sdk.display()
            );
            cmd.arg("zigbuild").env("SDKROOT", sdk);
        }
        None => {
            println!("=== Building {} ({}) ===", opts.target, opts.profile);
            cmd.arg("build");
        }
    }
    cmd.arg("--target")
        .arg(&opts.target)
        .args(["-p", BIN_NAME])
        .current_dir(workspace);
    if opts.profile != "dev" {
        cmd.args(["--profile", &opts.profile]);
    }

    let status = cmd.status().context("running cargo to build upalla-mac")?;
    if !status.success() {
        bail!("cargo build failed with {status}");
    }
    if !binary.exists() {
        bail!("build reported success but {} is missing", binary.display());
    }
    Ok(binary)
}

/// An explicit `SDKROOT` wins; otherwise the legacy path is used only if it is
/// really there, so a missing SDK no longer breaks packaging.
fn sdk_root() -> Option<PathBuf> {
    if let Some(sdk) = std::env::var_os("SDKROOT") {
        return Some(PathBuf::from(sdk));
    }
    let legacy = PathBuf::from(std::env::var_os("HOME")?).join(LEGACY_SDK);
    legacy.is_dir().then_some(legacy)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("marking {} executable", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Writes an icns built from `source_png`, returning its size in bytes.
fn write_icns(source_png: &Path, dest: &Path) -> Result<usize> {
    let source_bytes = fs::read(source_png)
        .with_context(|| format!("reading icon source {}", source_png.display()))?;
    let image = image::load_from_memory_with_format(&source_bytes, ImageFormat::Png)
        .with_context(|| format!("decoding {}", source_png.display()))?
        .to_rgba8();

    if image.width() != image.height() {
        bail!(
            "icon source must be square, got {}x{}",
            image.width(),
            image.height()
        );
    }

    let mut body = Vec::new();
    for (code, size) in ICNS_ENTRIES {
        let png = match (*size).cmp(&image.width()) {
            // Same size as the source: reuse its bytes rather than re-encoding,
            // so the largest entry is pixel-for-pixel the artwork.
            std::cmp::Ordering::Equal => source_bytes.clone(),
            std::cmp::Ordering::Greater => bail!(
                "icon source is {}px but a {}px entry was requested; upscaling would blur it",
                image.width(),
                size
            ),
            std::cmp::Ordering::Less => encode_png(&image::imageops::resize(
                &image,
                *size,
                *size,
                FilterType::Lanczos3,
            ))?,
        };
        let entry_len = u32::try_from(8 + png.len()).context("icns entry too large")?;
        body.extend_from_slice(*code);
        body.extend_from_slice(&entry_len.to_be_bytes());
        body.extend_from_slice(&png);
    }

    let total = 8 + body.len();
    let mut icns = Vec::with_capacity(total);
    icns.extend_from_slice(b"icns");
    icns.extend_from_slice(
        &u32::try_from(total)
            .context("icns too large")?
            .to_be_bytes(),
    );
    icns.extend_from_slice(&body);

    fs::write(dest, &icns).with_context(|| format!("writing {}", dest.display()))?;
    Ok(total)
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .context("encoding a resized icon as PNG")?;
    Ok(png)
}

fn info_plist() -> String {
    // Version tracks the crate rather than being typed out again here.
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN"
 "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>{BIN_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>{BUNDLE_ID}</string>
    <key>CFBundleName</key>
    <string>{APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>{APP_NAME}</string>
    <key>CFBundleVersion</key>
    <string>{version}</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Upalla needs microphone access to process and filter your voice in real time.</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
</dict>
</plist>
"#
    )
}
