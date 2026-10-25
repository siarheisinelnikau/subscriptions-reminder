#[path = "src/icons.rs"]
mod icons;
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icons.rs");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    bundle_google_client();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let path = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("app.ico");
    // Keep native tray/DPI variants instead of stretching one 32px bitmap.
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
    let mut images = Vec::new();
    for size in sizes {
        let rgba = icons::pixels(size);
        let mask_stride = size.div_ceil(32) * 4;
        let mut image = Vec::new();
        for value in [40u32, size, size * 2] {
            image.extend(value.to_le_bytes());
        }
        image.extend(1u16.to_le_bytes());
        image.extend(32u16.to_le_bytes());
        for value in [0u32, size * size * 4, 0, 0, 0, 0] {
            image.extend(value.to_le_bytes());
        }
        for y in (0..size).rev() {
            for x in 0..size {
                let at = ((y * size + x) * 4) as usize;
                image.extend([rgba[at + 2], rgba[at + 1], rgba[at], rgba[at + 3]]);
            }
        }
        for y in (0..size).rev() {
            let mut mask = vec![0u8; mask_stride as usize];
            for x in 0..size {
                if rgba[((y * size + x) * 4 + 3) as usize] == 0 {
                    mask[(x / 8) as usize] |= 0x80 >> (x % 8);
                }
            }
            image.extend(mask);
        }
        images.push(image);
    }
    let mut ico = vec![0, 0, 1, 0];
    ico.extend((sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (size, image) in sizes.into_iter().zip(&images) {
        ico.extend([size as u8, size as u8, 0, 0, 1, 0, 32, 0]);
        ico.extend((image.len() as u32).to_le_bytes());
        ico.extend(offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        ico.extend(image);
    }
    fs::write(&path, ico).expect("Cannot write application icon");
    let manifest = path.with_extension("manifest");
    fs::write(
        &manifest,
        include_str!("assets/app.manifest").replace("@VERSION@", env!("CARGO_PKG_VERSION")),
    )
    .expect("Cannot prepare the common-controls activation manifest");
    winresource::WindowsResource::new()
        .set_manifest_file(manifest.to_str().unwrap())
        .set_icon(path.to_str().unwrap())
        .set("ProductName", "Subscriptions Reminder")
        .set(
            "FileDescription",
            "Subscription tracking and background reminders",
        )
        .set("LegalCopyright", "Copyright (c) 2026 siarheisinelnikau")
        .compile()
        .expect("Cannot compile Windows application resources");
}

fn bundle_google_client() {
    const LOCAL_CLIENT: &str = "src/drive/google-oauth.json";
    println!("cargo:rerun-if-changed={LOCAL_CLIENT}");
    println!("cargo:rerun-if-env-changed=SUBSCRIPTIONS_GOOGLE_OAUTH_JSON");
    println!("cargo:rerun-if-env-changed=SUBSCRIPTIONS_REQUIRE_GOOGLE_OAUTH");
    let supplied = env::var("SUBSCRIPTIONS_GOOGLE_OAUTH_JSON")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let configuration = supplied.unwrap_or_else(|| match fs::read_to_string(LOCAL_CLIENT) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "null".into(),
        Err(_) => panic!("Cannot read the local Google OAuth client configuration"),
    });
    let document: serde_json::Value = serde_json::from_str(&configuration).unwrap_or_else(|_| {
        panic!("Invalid Google OAuth client JSON; configuration values were not logged")
    });
    let required = env::var("SUBSCRIPTIONS_REQUIRE_GOOGLE_OAUTH").as_deref() == Ok("1");
    if required {
        let client = document.get("installed");
        let configured = client
            .and_then(|value| value.get("client_id"))
            .and_then(|value| value.as_str())
            .is_some_and(|id| !id.is_empty() && id.ends_with(".apps.googleusercontent.com"))
            && client
                .and_then(|value| value.get("client_secret"))
                .and_then(|value| value.as_str())
                .is_some_and(|secret| !secret.is_empty());
        assert!(
            configured,
            "Release Google sign-in requires the GOOGLE_DESKTOP_OAUTH_JSON repository Actions secret containing the Desktop client JSON"
        );
    }
    let destination = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("google-oauth.json");
    fs::write(destination, configuration).expect("Cannot prepare the bundled Google OAuth client");
}
