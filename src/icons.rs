//! Calendar artwork shared by the packaged ICO and native-size validation.
pub fn pixels(size: u32) -> Vec<u8> {
    let mut pixels = vec![0; (size * size * 4) as usize];
    let inset = (size / 16).max(1);
    for y in inset..size - inset {
        for x in inset..size - inset {
            let header = y < size * 3 / 8;
            let cell = |position: u32, coordinate: u32| {
                let start = size * coordinate / 32;
                (start..(size * (coordinate + 1) / 32).max(start + 1)).contains(&position)
            };
            let dot = [10, 16, 22].iter().any(|column| cell(x, *column))
                && [17, 23].iter().any(|row| cell(y, *row));
            let color = if header || dot {
                [76, 104, 232, 255]
            } else {
                [234, 239, 255, 255]
            };
            let at = ((y * size + x) * 4) as usize;
            pixels[at..at + 4].copy_from_slice(&color);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_icons_have_opaque_artwork_and_transparent_edges() {
        for size in [16, 20, 24, 32, 40, 48, 64, 128, 256] {
            let pixels = super::pixels(size);
            assert_eq!(pixels.len(), (size * size * 4) as usize);
            assert_eq!(pixels[3], 0);
            let visible = pixels.chunks_exact(4).filter(|p| p[3] == 255).count();
            assert!(visible >= (size * size / 2) as usize);
        }
    }
}
