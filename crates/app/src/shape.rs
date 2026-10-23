//! Anti-aliased shapes from signed distance functions, and small easing
//! helpers, shared by the progress badge and the welcome window.

use std::f32::consts::TAU;
use std::time::Duration;

pub fn ratio(elapsed: Duration, total: Duration) -> f32 {
    (elapsed.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
}

pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Pixel coverage from a signed distance (negative inside), anti-aliased
/// over one pixel.
pub fn coverage_of(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

pub fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

pub fn lerp2(a: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// Signed distance to a rounded rectangle at (x, y) sized w x h.
pub fn rounded_rect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let qx = (px - cx).abs() - (w / 2.0 - r);
    let qy = (py - cy).abs() - (h / 2.0 - r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

/// Distance to the segment a–b.
pub fn capsule(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let len2 = abx * abx + aby * aby;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    dist(p, (a.0 + abx * t, a.1 + aby * t))
}

/// Distance to the arc of radius `r` around `c` from angle `start` through
/// `sweep` (clockwise on screen), with round ends.
pub fn arc_distance(p: (f32, f32), c: (f32, f32), r: f32, start: f32, sweep: f32) -> f32 {
    let angle = (p.1 - c.1).atan2(p.0 - c.0);
    let rel = (angle - start).rem_euclid(TAU);
    if rel <= sweep {
        (dist(p, c) - r).abs()
    } else {
        let end = start + sweep;
        let e1 = (c.0 + r * start.cos(), c.1 + r * start.sin());
        let e2 = (c.0 + r * end.cos(), c.1 + r * end.sin());
        dist(p, e1).min(dist(p, e2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn shapes_have_their_edges_where_expected() {
        // inside, on the edge, outside a 100 x 20 pill at the origin
        assert!(rounded_rect(50.0, 10.0, 0.0, 0.0, 100.0, 20.0, 10.0) < 0.0);
        assert!(rounded_rect(100.0, 10.0, 0.0, 0.0, 100.0, 20.0, 10.0).abs() < 1e-4);
        assert!(rounded_rect(110.0, 10.0, 0.0, 0.0, 100.0, 20.0, 10.0) > 0.0);
        assert!((capsule((5.0, 3.0), (0.0, 0.0), (10.0, 0.0)) - 3.0).abs() < 1e-4);
        // a quarter arc from angle 0: on it, and off its end
        let c = (0.0, 0.0);
        assert!(arc_distance((0.0, 10.0), c, 10.0, 0.0, PI / 2.0) < 1e-4);
        assert!(arc_distance((-10.0, 0.0), c, 10.0, 0.0, PI / 2.0) > 10.0);
    }
}
