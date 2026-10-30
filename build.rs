use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
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
