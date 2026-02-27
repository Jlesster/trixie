// box_drawing.rs — synthetic pixel rendering for box/block/braille characters

pub fn render_box_char(ch: char, cell_w: u32, cell_h: u32) -> Option<Vec<u8>> {
    let cp = ch as u32;
    match cp {
        0x2500..=0x257F => Some(draw_box(cp, cell_w, cell_h)),
        0x2580..=0x259F => Some(draw_block(cp, cell_w, cell_h)),
        0x2800..=0x28FF => Some(draw_braille(cp, cell_w, cell_h)),
        0xE0B0 => Some(draw_pl_right_solid(cell_w, cell_h)),
        0xE0B2 => Some(draw_pl_left_solid(cell_w, cell_h)),
        0xE0B1 => Some(draw_pl_right_hollow(cell_w, cell_h)),
        0xE0B3 => Some(draw_pl_left_hollow(cell_w, cell_h)),
        _ => None,
    }
}

// ── buffer ────────────────────────────────────────────────────────────────────

fn buf(w: u32, h: u32) -> Vec<u8> {
    vec![0u8; (w * h * 4) as usize]
}

fn set(p: &mut Vec<u8>, w: u32, h: u32, x: i32, y: i32, a: u8) {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
        return;
    }
    let i = (y as u32 * w + x as u32) as usize * 4;
    if a > p[i + 3] {
        p[i] = 0xFF;
        p[i + 1] = 0xFF;
        p[i + 2] = 0xFF;
        p[i + 3] = a;
    }
}

fn set_f(p: &mut Vec<u8>, w: u32, h: u32, x: i32, y: i32, a: f32) {
    set(p, w, h, x, y, (a.clamp(0.0, 1.0) * 255.0) as u8);
}

fn rect(p: &mut Vec<u8>, w: u32, h: u32, x0: i32, y0: i32, x1: i32, y1: i32) {
    for y in y0.max(0)..y1.min(h as i32) {
        for x in x0.max(0)..x1.min(w as i32) {
            set(p, w, h, x, y, 0xFF);
        }
    }
}

// ── thickness ─────────────────────────────────────────────────────────────────

fn norm(cell_h: u32) -> u32 {
    ((cell_h as f32 * 0.08 + 0.5).floor() as u32).max(1)
}

fn thk(cell_h: u32) -> u32 {
    ((cell_h as f32 * 0.18 + 0.5).floor() as u32).max(2)
}

// ── strokes ───────────────────────────────────────────────────────────────────

fn hl(p: &mut Vec<u8>, w: u32, h: u32, cy: i32, t: u32) {
    let d = t as i32 / 2;
    rect(p, w, h, 0, cy - d, w as i32, cy - d + t as i32);
}
fn vl(p: &mut Vec<u8>, w: u32, h: u32, cx: i32, t: u32) {
    let d = t as i32 / 2;
    rect(p, w, h, cx - d, 0, cx - d + t as i32, h as i32);
}
fn hl_l(p: &mut Vec<u8>, w: u32, h: u32, cy: i32, t: u32) {
    let d = t as i32 / 2;
    rect(p, w, h, 0, cy - d, w as i32 / 2 + d, cy - d + t as i32);
}
fn hl_r(p: &mut Vec<u8>, w: u32, h: u32, cy: i32, t: u32) {
    let d = t as i32 / 2;
    rect(
        p,
        w,
        h,
        w as i32 / 2 - d,
        cy - d,
        w as i32,
        cy - d + t as i32,
    );
}
fn vl_t(p: &mut Vec<u8>, w: u32, h: u32, cx: i32, t: u32) {
    let d = t as i32 / 2;
    rect(p, w, h, cx - d, 0, cx - d + t as i32, h as i32 / 2 + d);
}
fn vl_b(p: &mut Vec<u8>, w: u32, h: u32, cx: i32, t: u32) {
    let d = t as i32 / 2;
    rect(
        p,
        w,
        h,
        cx - d,
        h as i32 / 2 - d,
        cx - d + t as i32,
        h as i32,
    );
}

// ── double lines ──────────────────────────────────────────────────────────────

fn dbl_gap(t: u32) -> i32 {
    (t as i32 * 2).max(3)
}

fn dbl_hl(p: &mut Vec<u8>, w: u32, h: u32, cy: i32, t: u32) {
    let d = dbl_gap(t);
    hl(p, w, h, cy - d, t);
    hl(p, w, h, cy + d, t);
}
fn dbl_vl(p: &mut Vec<u8>, w: u32, h: u32, cx: i32, t: u32) {
    let d = dbl_gap(t);
    vl(p, w, h, cx - d, t);
    vl(p, w, h, cx + d, t);
}
fn dbl_hl_l(p: &mut Vec<u8>, w: u32, h: u32, cy: i32, t: u32) {
    let d = dbl_gap(t);
    hl_l(p, w, h, cy - d, t);
    hl_l(p, w, h, cy + d, t);
}
fn dbl_hl_r(p: &mut Vec<u8>, w: u32, h: u32, cy: i32, t: u32) {
    let d = dbl_gap(t);
    hl_r(p, w, h, cy - d, t);
    hl_r(p, w, h, cy + d, t);
}
fn dbl_vl_t(p: &mut Vec<u8>, w: u32, h: u32, cx: i32, t: u32) {
    let d = dbl_gap(t);
    vl_t(p, w, h, cx - d, t);
    vl_t(p, w, h, cx + d, t);
}
fn dbl_vl_b(p: &mut Vec<u8>, w: u32, h: u32, cx: i32, t: u32) {
    let d = dbl_gap(t);
    vl_b(p, w, h, cx - d, t);
    vl_b(p, w, h, cx + d, t);
}

// ── rounded corners (cubic Bézier, kitty-style) ───────────────────────────────

#[inline]
fn bezier2(
    t: f32,
    start: (f32, f32),
    c1: (f32, f32),
    c2: (f32, f32),
    end: (f32, f32),
) -> (f32, f32) {
    let u = 1.0 - t;
    let uu = u * u;
    let uuu = uu * u;
    let tt = t * t;
    let ttt = tt * t;
    let x = uuu * start.0 + 3.0 * uu * t * c1.0 + 3.0 * u * tt * c2.0 + ttt * end.0;
    let y = uuu * start.1 + 3.0 * uu * t * c1.1 + 3.0 * u * tt * c2.1 + ttt * end.1;
    (x, y)
}

fn draw_bezier_curve(
    p: &mut Vec<u8>,
    w: u32,
    h: u32,
    t: u32,
    start: (f32, f32),
    c1: (f32, f32),
    c2: (f32, f32),
    end: (f32, f32),
) {
    let num_samples = h as usize * 4;
    let delta = (t / 2) as i32;
    let extra = (t % 2) as i32;
    for i in 0..=num_samples {
        let tv = i as f32 / num_samples as f32;
        let (fx, fy) = bezier2(tv, start, c1, c2, end);
        let ix = fx as i32;
        let iy = fy as i32;
        for dy in -delta..(delta + extra) {
            for dx in -delta..(delta + extra) {
                set(p, w, h, ix + dx, iy + dy, 0xFF);
            }
        }
    }
}

fn rounded_corner(p: &mut Vec<u8>, w: u32, h: u32, t: u32, which: u32) {
    let wi = w as f32;
    let hi = h as f32;
    let cx = wi / 2.0;
    let cy = hi / 2.0;

    let (start, end, c1, c2) = match which {
        0x256D => ((cx, hi), (wi, cy), (cx, cy + 1.0), (wi * 0.75, cy)),
        0x256E => ((0.0, cy), (cx, hi), (wi * 0.25, cy), (cx, cy + 1.0)),
        0x256F => ((0.0, cy), (cx, 0.0), (wi * 0.25, cy), (cx, cy - 1.0)),
        _ => ((cx, 0.0), (wi, cy), (cx, cy - 1.0), (wi * 0.75, cy)),
    };

    draw_bezier_curve(p, w, h, t, start, c1, c2, end);
}

// ── dashed / diagonal ─────────────────────────────────────────────────────────

fn dashed_h(p: &mut Vec<u8>, w: u32, h: u32, cy: i32, t: u32, n: u32) {
    let seg = w / (n * 2);
    let d = t as i32 / 2;
    for i in 0..n {
        let x0 = (i * 2 * seg) as i32;
        rect(p, w, h, x0, cy - d, x0 + seg as i32, cy - d + t as i32);
    }
}
fn dashed_v(p: &mut Vec<u8>, w: u32, h: u32, cx: i32, t: u32, n: u32) {
    let seg = h / (n * 2);
    let d = t as i32 / 2;
    for i in 0..n {
        let y0 = (i * 2 * seg) as i32;
        rect(p, w, h, cx - d, y0, cx - d + t as i32, y0 + seg as i32);
    }
}
fn diag(p: &mut Vec<u8>, w: u32, h: u32, down_right: bool, t: u32) {
    let half = (t as i32).saturating_sub(1) / 2;
    for x in 0..w as i32 {
        let y = if down_right {
            x * h as i32 / w as i32
        } else {
            h as i32 - 1 - x * h as i32 / w as i32
        };
        for d in -half..=half {
            set(p, w, h, x, y + d, 0xFF);
        }
    }
}
fn diag_seg(p: &mut Vec<u8>, w: u32, h: u32, x0: i32, y0: i32, x1: i32, y1: i32, t: u32) {
    let dx = (x1 - x0).abs();
    let dy = (y1 - y0).abs();
    let steps = dx.max(dy).max(1);
    let half = (t as i32).saturating_sub(1) / 2;
    for i in 0..=steps {
        let x = x0 + (x1 - x0) * i / steps;
        let y = y0 + (y1 - y0) * i / steps;
        for tt in -half..=half {
            if dx >= dy {
                set(p, w, h, x, y + tt, 0xFF);
            } else {
                set(p, w, h, x + tt, y, 0xFF);
            }
        }
    }
}

// ── box drawing ───────────────────────────────────────────────────────────────

fn draw_box(cp: u32, w: u32, h: u32) -> Vec<u8> {
    let mut p = buf(w, h);
    let n = norm(h);
    let k = thk(h);
    let cx = w as i32 / 2;
    let cy = h as i32 / 2;
    match cp {
        0x2500 => hl(&mut p, w, h, cy, n),
        0x2501 => hl(&mut p, w, h, cy, k),
        0x2502 => vl(&mut p, w, h, cx, n),
        0x2503 => vl(&mut p, w, h, cx, k),
        0x2504 | 0x2508 | 0x254C => dashed_h(&mut p, w, h, cy, n, 2),
        0x2505 | 0x2509 | 0x254D => dashed_h(&mut p, w, h, cy, k, 2),
        0x2506 | 0x250A => dashed_v(&mut p, w, h, cx, n, 2),
        0x2507 | 0x250B => dashed_v(&mut p, w, h, cx, k, 2),
        // light corners
        0x250C => {
            hl_r(&mut p, w, h, cy, n);
            vl_b(&mut p, w, h, cx, n);
        }
        0x2510 => {
            hl_l(&mut p, w, h, cy, n);
            vl_b(&mut p, w, h, cx, n);
        }
        0x2514 => {
            hl_r(&mut p, w, h, cy, n);
            vl_t(&mut p, w, h, cx, n);
        }
        0x2518 => {
            hl_l(&mut p, w, h, cy, n);
            vl_t(&mut p, w, h, cx, n);
        }
        // thick corners
        0x250F => {
            hl_r(&mut p, w, h, cy, k);
            vl_b(&mut p, w, h, cx, k);
        }
        0x2513 => {
            hl_l(&mut p, w, h, cy, k);
            vl_b(&mut p, w, h, cx, k);
        }
        0x2517 => {
            hl_r(&mut p, w, h, cy, k);
            vl_t(&mut p, w, h, cx, k);
        }
        0x251B => {
            hl_l(&mut p, w, h, cy, k);
            vl_t(&mut p, w, h, cx, k);
        }
        // mixed corners
        0x250D => {
            hl_r(&mut p, w, h, cy, k);
            vl_b(&mut p, w, h, cx, n);
        }
        0x2511 => {
            hl_l(&mut p, w, h, cy, k);
            vl_b(&mut p, w, h, cx, n);
        }
        0x2515 => {
            hl_r(&mut p, w, h, cy, k);
            vl_t(&mut p, w, h, cx, n);
        }
        0x2519 => {
            hl_l(&mut p, w, h, cy, k);
            vl_t(&mut p, w, h, cx, n);
        }
        0x250E => {
            hl_r(&mut p, w, h, cy, n);
            vl_b(&mut p, w, h, cx, k);
        }
        0x2512 => {
            hl_l(&mut p, w, h, cy, n);
            vl_b(&mut p, w, h, cx, k);
        }
        0x2516 => {
            hl_r(&mut p, w, h, cy, n);
            vl_t(&mut p, w, h, cx, k);
        }
        0x251A => {
            hl_l(&mut p, w, h, cy, n);
            vl_t(&mut p, w, h, cx, k);
        }
        // light T-junctions
        0x251C => {
            hl_r(&mut p, w, h, cy, n);
            vl(&mut p, w, h, cx, n);
        }
        0x2524 => {
            hl_l(&mut p, w, h, cy, n);
            vl(&mut p, w, h, cx, n);
        }
        0x252C => {
            hl(&mut p, w, h, cy, n);
            vl_b(&mut p, w, h, cx, n);
        }
        0x2534 => {
            hl(&mut p, w, h, cy, n);
            vl_t(&mut p, w, h, cx, n);
        }
        0x253C => {
            hl(&mut p, w, h, cy, n);
            vl(&mut p, w, h, cx, n);
        }
        // thick T-junctions
        0x2523 => {
            hl_r(&mut p, w, h, cy, k);
            vl(&mut p, w, h, cx, k);
        }
        0x252B => {
            hl_l(&mut p, w, h, cy, k);
            vl(&mut p, w, h, cx, k);
        }
        0x2533 => {
            hl(&mut p, w, h, cy, k);
            vl_b(&mut p, w, h, cx, k);
        }
        0x253B => {
            hl(&mut p, w, h, cy, k);
            vl_t(&mut p, w, h, cx, k);
        }
        0x254B => {
            hl(&mut p, w, h, cy, k);
            vl(&mut p, w, h, cx, k);
        }
        // mixed T-junctions
        0x251D => {
            hl_r(&mut p, w, h, cy, k);
            vl(&mut p, w, h, cx, n);
        }
        0x2525 => {
            hl_l(&mut p, w, h, cy, k);
            vl(&mut p, w, h, cx, n);
        }
        0x252F => {
            hl(&mut p, w, h, cy, k);
            vl_b(&mut p, w, h, cx, n);
        }
        0x2537 => {
            hl(&mut p, w, h, cy, k);
            vl_t(&mut p, w, h, cx, n);
        }
        0x253F => {
            hl(&mut p, w, h, cy, k);
            vl(&mut p, w, h, cx, n);
        }
        0x2520 => {
            hl_r(&mut p, w, h, cy, n);
            vl(&mut p, w, h, cx, k);
        }
        0x2528 => {
            hl_l(&mut p, w, h, cy, n);
            vl(&mut p, w, h, cx, k);
        }
        0x2530 => {
            hl(&mut p, w, h, cy, n);
            vl_b(&mut p, w, h, cx, k);
        }
        0x2538 => {
            hl(&mut p, w, h, cy, n);
            vl_t(&mut p, w, h, cx, k);
        }
        0x2542 => {
            hl(&mut p, w, h, cy, n);
            vl(&mut p, w, h, cx, k);
        }
        // double lines
        0x2550 => dbl_hl(&mut p, w, h, cy, n),
        0x2551 => dbl_vl(&mut p, w, h, cx, n),
        0x2554 => {
            dbl_hl_r(&mut p, w, h, cy, n);
            dbl_vl_b(&mut p, w, h, cx, n);
        }
        0x2557 => {
            dbl_hl_l(&mut p, w, h, cy, n);
            dbl_vl_b(&mut p, w, h, cx, n);
        }
        0x255A => {
            dbl_hl_r(&mut p, w, h, cy, n);
            dbl_vl_t(&mut p, w, h, cx, n);
        }
        0x255D => {
            dbl_hl_l(&mut p, w, h, cy, n);
            dbl_vl_t(&mut p, w, h, cx, n);
        }
        0x2560 => {
            dbl_hl_r(&mut p, w, h, cy, n);
            dbl_vl(&mut p, w, h, cx, n);
        }
        0x2563 => {
            dbl_hl_l(&mut p, w, h, cy, n);
            dbl_vl(&mut p, w, h, cx, n);
        }
        0x2566 => {
            dbl_hl(&mut p, w, h, cy, n);
            dbl_vl_b(&mut p, w, h, cx, n);
        }
        0x2569 => {
            dbl_hl(&mut p, w, h, cy, n);
            dbl_vl_t(&mut p, w, h, cx, n);
        }
        0x256C => {
            dbl_hl(&mut p, w, h, cy, n);
            dbl_vl(&mut p, w, h, cx, n);
        }
        // mixed single/double
        0x255E | 0x255F => {
            hl_r(&mut p, w, h, cy, n);
            dbl_vl(&mut p, w, h, cx, n);
        }
        0x2561 | 0x2562 => {
            hl_l(&mut p, w, h, cy, n);
            dbl_vl(&mut p, w, h, cx, n);
        }
        0x2564 | 0x2565 => {
            dbl_hl(&mut p, w, h, cy, n);
            vl_b(&mut p, w, h, cx, n);
        }
        0x2567 | 0x2568 => {
            dbl_hl(&mut p, w, h, cy, n);
            vl_t(&mut p, w, h, cx, n);
        }
        // diagonals
        0x2571 => diag(&mut p, w, h, false, n),
        0x2572 => diag(&mut p, w, h, true, n),
        0x2573 => {
            diag(&mut p, w, h, false, n);
            diag(&mut p, w, h, true, n);
        }
        // half stubs
        0x2574 => hl_l(&mut p, w, h, cy, n),
        0x2575 => vl_t(&mut p, w, h, cx, n),
        0x2576 => hl_r(&mut p, w, h, cy, n),
        0x2577 => vl_b(&mut p, w, h, cx, n),
        0x2578 => hl_l(&mut p, w, h, cy, k),
        0x2579 => vl_t(&mut p, w, h, cx, k),
        0x257A => hl_r(&mut p, w, h, cy, k),
        0x257B => vl_b(&mut p, w, h, cx, k),
        0x257C => {
            hl_l(&mut p, w, h, cy, n);
            hl_r(&mut p, w, h, cy, k);
        }
        0x257D => {
            vl_t(&mut p, w, h, cx, n);
            vl_b(&mut p, w, h, cx, k);
        }
        0x257E => {
            hl_l(&mut p, w, h, cy, k);
            hl_r(&mut p, w, h, cy, n);
        }
        0x257F => {
            vl_t(&mut p, w, h, cx, k);
            vl_b(&mut p, w, h, cx, n);
        }
        // rounded corners
        0x256D => rounded_corner(&mut p, w, h, n, 0x256D),
        0x256E => rounded_corner(&mut p, w, h, n, 0x256E),
        0x256F => rounded_corner(&mut p, w, h, n, 0x256F),
        0x2570 => rounded_corner(&mut p, w, h, n, 0x2570),
        _ => {
            hl(&mut p, w, h, cy, n);
            vl(&mut p, w, h, cx, n);
        }
    }
    p
}

// ── block elements ────────────────────────────────────────────────────────────

fn draw_block(cp: u32, w: u32, h: u32) -> Vec<u8> {
    let mut p = buf(w, h);
    let wi = w as i32;
    let hi = h as i32;
    match cp {
        0x2580 => rect(&mut p, w, h, 0, 0, wi, hi / 2),
        0x2584 => rect(&mut p, w, h, 0, hi / 2, wi, hi),
        0x2588 => rect(&mut p, w, h, 0, 0, wi, hi),
        0x258C => rect(&mut p, w, h, 0, 0, wi / 2, hi),
        0x2590 => rect(&mut p, w, h, wi / 2, 0, wi, hi),
        0x2581..=0x2587 => {
            let n = (cp - 0x2580) as i32;
            rect(&mut p, w, h, 0, hi - (hi * n + 4) / 8, wi, hi);
        }
        0x2594 => rect(&mut p, w, h, 0, 0, wi, (hi + 7) / 8),
        0x2595 => rect(&mut p, w, h, wi - (wi + 7) / 8, 0, wi, hi),
        0x2589..=0x258F => {
            let n = (0x2590 - cp) as i32;
            rect(&mut p, w, h, 0, 0, wi - (wi * n + 4) / 8, hi);
        }
        0x2596 => rect(&mut p, w, h, 0, hi / 2, wi / 2, hi),
        0x2597 => rect(&mut p, w, h, wi / 2, hi / 2, wi, hi),
        0x2598 => rect(&mut p, w, h, 0, 0, wi / 2, hi / 2),
        0x2599 => {
            rect(&mut p, w, h, 0, 0, wi / 2, hi);
            rect(&mut p, w, h, wi / 2, hi / 2, wi, hi);
        }
        0x259A => {
            rect(&mut p, w, h, 0, 0, wi / 2, hi / 2);
            rect(&mut p, w, h, wi / 2, hi / 2, wi, hi);
        }
        0x259B => {
            rect(&mut p, w, h, 0, 0, wi, hi / 2);
            rect(&mut p, w, h, 0, hi / 2, wi / 2, hi);
        }
        0x259C => {
            rect(&mut p, w, h, 0, 0, wi, hi / 2);
            rect(&mut p, w, h, wi / 2, hi / 2, wi, hi);
        }
        0x259D => rect(&mut p, w, h, wi / 2, 0, wi, hi / 2),
        0x259E => {
            rect(&mut p, w, h, 0, hi / 2, wi / 2, hi);
            rect(&mut p, w, h, wi / 2, 0, wi, hi / 2);
        }
        0x259F => {
            rect(&mut p, w, h, wi / 2, 0, wi, hi);
            rect(&mut p, w, h, 0, hi / 2, wi / 2, hi);
        }
        0x2591 => shade(&mut p, w, h, 64),
        0x2592 => shade(&mut p, w, h, 128),
        0x2593 => shade(&mut p, w, h, 192),
        _ => rect(&mut p, w, h, 0, 0, wi, hi),
    }
    p
}

fn shade(p: &mut Vec<u8>, w: u32, h: u32, a: u8) {
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            if (x + y) % 2 == 0 {
                set(p, w, h, x, y, a);
            }
        }
    }
}

// ── braille ───────────────────────────────────────────────────────────────────

fn draw_braille(cp: u32, w: u32, h: u32) -> Vec<u8> {
    let mut p = buf(w, h);
    let bits = cp - 0x2800;
    if bits == 0 {
        return p;
    }
    let r = (w as f32 * 0.10).max(1.5);
    let xs = [w as f32 * 0.30, w as f32 * 0.70];
    let ys = [
        h as f32 * 0.15,
        h as f32 * 0.38,
        h as f32 * 0.62,
        h as f32 * 0.85,
    ];
    let dots: [(usize, usize); 8] = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    for bit in 0u32..8 {
        if bits & (1 << bit) == 0 {
            continue;
        }
        let (col, row) = dots[bit as usize];
        circle_aa(&mut p, w, h, xs[col], ys[row], r);
    }
    p
}

fn circle_aa(p: &mut Vec<u8>, w: u32, h: u32, cx: f32, cy: f32, r: f32) {
    let x0 = (cx - r - 1.0).floor() as i32;
    let x1 = (cx + r + 1.0).ceil() as i32;
    let y0 = (cy - r - 1.0).floor() as i32;
    let y1 = (cy + r + 1.0).ceil() as i32;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            let a = (r + 0.5 - d).clamp(0.0, 1.0);
            if a > 0.0 {
                set_f(p, w, h, x, y, a);
            }
        }
    }
}

// ── powerline ─────────────────────────────────────────────────────────────────

fn draw_pl_right_solid(w: u32, h: u32) -> Vec<u8> {
    let mut p = buf(w, h);
    let wi = w as f32;
    let hi = h as f32;
    let half = hi * 0.5;
    for y in 0..h as i32 {
        let dist = (y as f32 - half).abs();
        let x_max = (wi - dist * wi / half).ceil() as i32;
        for x in 0..x_max.max(0).min(w as i32) {
            set(&mut p, w, h, x, y, 0xFF);
        }
    }
    p
}

fn draw_pl_left_solid(w: u32, h: u32) -> Vec<u8> {
    let mut p = buf(w, h);
    let wi = w as f32;
    let hi = h as f32;
    let half = hi * 0.5;
    for y in 0..h as i32 {
        let dist = (y as f32 - half).abs();
        let x_min = (dist * wi / half).floor() as i32;
        for x in x_min.max(0)..w as i32 {
            set(&mut p, w, h, x, y, 0xFF);
        }
    }
    p
}

fn draw_pl_right_hollow(w: u32, h: u32) -> Vec<u8> {
    let mut p = buf(w, h);
    let t = norm(h);
    let wi = w as i32;
    let hi = h as i32;
    diag_seg(&mut p, w, h, 0, 0, wi - 1, hi / 2, t);
    diag_seg(&mut p, w, h, 0, hi - 1, wi - 1, hi / 2, t);
    p
}

fn draw_pl_left_hollow(w: u32, h: u32) -> Vec<u8> {
    let mut p = buf(w, h);
    let t = norm(h);
    let wi = w as i32;
    let hi = h as i32;
    diag_seg(&mut p, w, h, wi - 1, 0, 0, hi / 2, t);
    diag_seg(&mut p, w, h, wi - 1, hi - 1, 0, hi / 2, t);
    p
}
