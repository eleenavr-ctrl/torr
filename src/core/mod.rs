pub mod config;
pub mod db;
pub mod diagnostics;
pub mod error;
pub mod event_bus;
pub mod paths;
pub mod secrets;
pub mod storage;
pub mod sync;

/// Generates a crisp 32x32 Windows 11 style blue-cyan rounded-square icon with a play glyph.
pub fn generate_app_icon_rgba() -> Vec<u8> {
    const W: usize = 32;
    const H: usize = 32;
    let mut buf = vec![0u8; W * H * 4];

    for y in 0..H {
        for x in 0..W {
            let idx = (y * W + x) * 4;
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;

            let dx = (fx - 16.0).abs() - 8.0;
            let dy = (fy - 16.0).abs() - 8.0;
            let ox = dx.max(0.0);
            let oy = dy.max(0.0);
            let dist = (ox * ox + oy * oy).sqrt() + dx.max(dy).min(0.0) - 6.0;

            if dist <= 0.6 {
                let alpha = (0.6 - dist).clamp(0.0, 1.0);
                let t = ((fx + fy) / 64.0).clamp(0.0, 1.0);
                let mut r = t * 50.0;
                let mut g = 103.0 + t * 88.0;
                let mut b = 192.0 + t * 63.0;

                if dist > -1.2 && fy < 16.0 {
                    r = (r + 35.0).min(255.0);
                    g = (g + 35.0).min(255.0);
                    b = (b + 35.0).min(255.0);
                }

                let in_play = if (11.5..=22.8).contains(&fx) {
                    let progress = (fx - 11.5) / (22.8 - 11.5);
                    let half_h = 6.6 * (1.0 - progress);
                    (fy - 16.0).abs() <= half_h
                } else {
                    false
                };

                if in_play {
                    r = 255.0;
                    g = 255.0;
                    b = 255.0;
                }

                buf[idx] = r.clamp(0.0, 255.0) as u8;
                buf[idx + 1] = g.clamp(0.0, 255.0) as u8;
                buf[idx + 2] = b.clamp(0.0, 255.0) as u8;
                buf[idx + 3] = (alpha * 255.0) as u8;
            }
        }
    }

    buf
}
