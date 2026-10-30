use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    bundle_google_client();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let path = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("app.ico");
    let mut image = vec![];
    for value in [40u32, 32, 64] {
        image.extend(value.to_le_bytes());
    }
    image.extend(1u16.to_le_bytes());
    image.extend(32u16.to_le_bytes());
    for value in [0u32, 4096, 0, 0, 0, 0] {
        image.extend(value.to_le_bytes());
    }
    for y in (0..32).rev() {
        for x in 0..32 {
            let pixel = if (3..29).contains(&x) && (4..29).contains(&y) {
                if y < 12 || ((x == 10 || x == 16 || x == 22) && (y == 17 || y == 23)) {
                    [232, 104, 76, 255]
                } else {
                    [255, 239, 234, 255]
                }
            } else {
                [0, 0, 0, 0]
            };
            image.extend(pixel);
        }
    }
    image.extend([0u8; 128]);
    let mut ico = vec![0, 0, 1, 0, 1, 0, 32, 32, 0, 0, 1, 0, 32, 0];
    ico.extend((image.len() as u32).to_le_bytes());
    ico.extend(22u32.to_le_bytes());
    ico.extend(image);
    fs::write(&path, ico).expect("Cannot write application icon");
    winresource::WindowsResource::new()
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
