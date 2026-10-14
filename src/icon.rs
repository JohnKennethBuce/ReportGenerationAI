//! The app icon, drawn in code (no image files needed): a rounded clock face.

/// RGBA pixels for a `size`×`size` icon. Grey when paused, teal when tracking.
pub fn rgba(size: u32, paused: bool) -> Vec<u8> {
    let (r, g, b) = if paused { (128u8, 132u8, 140u8) } else { (20u8, 184u8, 166u8) };
    let s = size as f32;
    let c = (s - 1.0) / 2.0;
    let radius = s * 0.47;
    let stroke = (s / 14.0).max(1.6);

    // Clock hands as line segments from the centre.
    let hands = [(c, c - radius * 0.58), (c + radius * 0.45, c + radius * 0.12)];

    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32, y as f32);
            let d = ((fx - c).powi(2) + (fy - c).powi(2)).sqrt();
            let disc = (radius - d + 0.5).clamp(0.0, 1.0); // anti-aliased edge
            if disc <= 0.0 {
                px.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let mut white: f32 = 0.0;
            for &(hx, hy) in &hands {
                let dist = dist_to_segment(fx, fy, c, c, hx, hy);
                white = white.max((stroke / 2.0 - dist + 0.5).clamp(0.0, 1.0));
            }
            let mix = |base: u8| (base as f32 * (1.0 - white) + 255.0 * white) as u8;
            px.extend_from_slice(&[mix(r), mix(g), mix(b), (disc * 255.0) as u8]);
        }
    }
    px
}

fn dist_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) };
    let (cx, cy) = (ax + t * dx, ay + t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}
