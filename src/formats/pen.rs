//! Brother PP1 ("Skitch") PEN stitch stream: what the machine receives over
//! Bluetooth.
//!
//! Format and lock-stitch behaviour learned from Respira's PEN encoder
//! (https://github.com/jhbruhn/respira, Apache-2.0, Copyright Jan-Henrik Bruhn),
//! itself modelled on Brother's PesxToPen. Each stitch is 4 bytes: X and Y as
//! little-endian 16-bit absolute coordinates (0.1 mm) shifted left by 3, with
//! flags in the low 3 bits.

use crate::design::Design;

/// Y flag: move without stitching.
pub const FEED: u16 = 0x01;
/// Y flag: cut thread.
pub const CUT: u16 = 0x02;
/// X flag value: last stitch of a color.
pub const COLOR_END: u16 = 0x03;
/// X flag value: last stitch of the pattern.
pub const DATA_END: u16 = 0x05;

/// Jumps longer than this (units) get lock stitches and a cut, whether or
/// not the plan asked for one. (A trimmed move gets them at any length.)
const FEED_LENGTH: f64 = 50.0;
const TARGET_LENGTH: f64 = 8.0;
const MAX_POINTS: usize = 5;
/// Lock stitch offset from the anchor point, units.
const LOCK_SIZE: f64 = 2.0;

/// Input stitch in Respira's representation: absolute position (units),
/// command bits, color block index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InStitch {
    pub x: f64,
    pub y: f64,
    pub cmd: u32,
    pub color: usize,
}

/// Command bits (pyembroidery/pystitch values, as used by Respira).
pub const MOVE: u32 = 0x10;
pub const TRIM: u32 = 0x20;
pub const END: u32 = 0x100;

#[derive(Clone, Debug)]
pub struct PenData {
    pub bytes: Vec<u8>,
    /// (min_x, min_y, max_x, max_y) of sewn stitches, units.
    pub bounds: (i32, i32, i32, i32),
    pub stitches: usize,
    /// For each stitch the machine sews (each 4-byte record): how many of
    /// the design's stitches are sewn once it's there. Maps the machine's
    /// progress back onto the design.
    pub design_at: Vec<u32>,
    /// The color blocks, as the machine sews them.
    pub blocks: Vec<PenBlock>,
}

/// One color of a PEN pattern: its records `start..end` and which of the
/// design's blocks (threads) it sews.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PenBlock {
    pub start: usize,
    pub end: usize,
    pub block: usize,
}

impl PenData {
    /// Which color block record `k` is in.
    pub fn block_at(&self, k: usize) -> Option<usize> {
        self.blocks.iter().position(|b| k >= b.start && k < b.end)
    }

    /// The thread (color block) the machine is on at record `k`. While it
    /// waits for a thread change (`changing`), the one to change to: the
    /// thread starting nearest `k`. (A Skitch reports the next thread's
    /// first stitch there; this doesn't depend on which side it reports.)
    pub fn thread_at(&self, k: usize, changing: bool) -> Option<usize> {
        let last = self.blocks.len().checked_sub(1)?;
        if changing && last > 0 {
            return (1..=last).min_by_key(|&b| self.blocks[b].start.abs_diff(k));
        }
        Some(
            self.block_at(k)
                .unwrap_or(if k >= self.stitches { last } else { 0 }),
        )
    }
}

/// Flatten a design into Respira-style input stitches: each run after the
/// first starts with a MOVE to its first point (with TRIM if the plan cuts there).
pub fn stitches(d: &Design) -> Vec<InStitch> {
    stitches_counted(d).0
}

/// [`stitches`], with how many of the design's stitches each one has
/// reached (counting the repeats it drops).
fn stitches_counted(d: &Design) -> (Vec<InStitch>, Vec<u32>) {
    let mut out: Vec<InStitch> = Vec::new();
    let mut count: Vec<u32> = Vec::new();
    let mut seen = 0u32;
    for (bi, blk) in d.blocks.iter().enumerate() {
        for (ri, run) in blk.runs.iter().enumerate() {
            let Some(&first) = run.pts.first() else {
                continue;
            };
            let mut rest = &run.pts[..];
            if !out.is_empty() {
                let mut cmd = if ri > 0 && run.trim_before {
                    MOVE | TRIM
                } else {
                    MOVE
                };
                // The run before came to a move and nothing else (a speck
                // whose points round to one spot): drop it. The encoder
                // ends a thread, and the data, on a stitch; after a lone
                // long move it would write neither.
                if let Some(prev) = out.pop_if(|s| s.cmd & MOVE != 0) {
                    cmd |= prev.cmd & TRIM;
                    count.pop();
                }
                out.push(InStitch {
                    x: first.x.round(),
                    y: first.y.round(),
                    cmd,
                    color: bi,
                });
                seen += 1;
                count.push(seen);
                rest = &run.pts[1..];
            }
            for q in rest {
                seen += 1;
                let (x, y) = (q.x.round(), q.y.round());
                // Respira drops exact repeats of the previous position.
                if out.last().is_some_and(|s| s.x == x && s.y == y) {
                    if let Some(c) = count.last_mut() {
                        *c = seen;
                    }
                    continue;
                }
                out.push(InStitch {
                    x,
                    y,
                    cmd: 0,
                    color: bi,
                });
                count.push(seen);
            }
        }
    }
    // Likewise a move the design ends on.
    if out.pop_if(|s| s.cmd & MOVE != 0).is_some() {
        count.pop();
        if let Some(c) = count.last_mut() {
            *c = seen;
        }
    }
    (out, count)
}

/// JS `Math.round` (ties toward +∞), so coordinates match Respira bit for bit.
fn js_round(v: f64) -> i32 {
    (v + 0.5).floor() as i32
}

/// Direction (magnitude 8) from stitch `i` toward up to 5 nearby non-MOVE stitches.
fn lock_direction(ss: &[InStitch], i: usize, ahead: bool) -> (f64, f64) {
    let (mut ax, mut ay) = (0.0f64, 0.0f64);
    let (mut best_len, mut bx, mut by) = (0.0f64, 0.0f64, 0.0f64);
    let max_iter = if ahead {
        MAX_POINTS.min(ss.len() - i - 1)
    } else {
        MAX_POINTS.min(i)
    };
    let (cx, cy) = (js_round(ss[i].x) as f64, js_round(ss[i].y) as f64);
    for k in 0..max_iter {
        let idx = if ahead { i + k + 1 } else { i - k - 1 };
        let s = ss[idx];
        if s.cmd & MOVE != 0 {
            continue;
        }
        ax += js_round(s.x) as f64 - cx;
        ay += js_round(s.y) as f64 - cy;
        let len = (ax * ax + ay * ay).sqrt();
        if len > best_len {
            best_len = len;
            bx = ax;
            by = ay;
        }
        if len >= TARGET_LENGTH {
            return (ax * 8.0 / len, ay * 8.0 / len);
        }
    }
    if best_len > 0.1 {
        return (bx * 8.0 / best_len, by * 8.0 / best_len);
    }
    let m = 8.0 / 2f64.sqrt();
    (m, m)
}

fn push(out: &mut Vec<u8>, x: i32, y: i32, xflag: u16, yflag: u16) {
    let xe = (((x << 3) & 0xFFFF) as u16 & 0xFFF8) | xflag;
    let ye = ((y << 3) & 0xFFFF) as u16 | yflag;
    out.extend_from_slice(&xe.to_le_bytes());
    out.extend_from_slice(&ye.to_le_bytes());
}

fn locks(out: &mut Vec<u8>, x: i32, y: i32, dir: (f64, f64)) {
    let (dx, dy) = (dir.0 * LOCK_SIZE / 8.0, dir.1 * LOCK_SIZE / 8.0);
    for i in 0..8 {
        let s = if i % 2 == 0 { 1.0 } else { -1.0 };
        push(
            out,
            js_round(x as f64 + dx * s),
            js_round(y as f64 + dy * s),
            0,
            0,
        );
    }
}

/// Encode a design for the Skitch.
pub fn encode(d: &Design) -> PenData {
    let (ss, count) = stitches_counted(d);
    let mut pen = encode_stitches(&ss);
    // `design_at` holds input indices so far: through to design stitches.
    for v in &mut pen.design_at {
        *v = count.get(*v as usize).copied().unwrap_or(0);
    }
    pen
}

/// Port of Respira's `encodeStitchesToPen`, kept structurally identical so the
/// output matches it byte for byte, but for one thing: a short trimmed move
/// is locked like a long one.
pub fn encode_stitches(ss: &[InStitch]) -> PenData {
    let mut out = Vec::with_capacity(ss.len() * 4 + 256);
    // Per record: the input stitch it came from, and its color.
    let mut src: Vec<(u32, usize)> = Vec::with_capacity(ss.len() + 64);
    let mut tag = |out: &Vec<u8>, i: usize, color: usize| {
        while src.len() < out.len() / 4 {
            src.push((i as u32, color));
        }
    };
    let (mut minx, mut miny, mut maxx, mut maxy) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    let (mut px, mut py) = (0i32, 0i32);
    let mut i = 0usize;
    while i < ss.len() {
        let s = ss[i];
        let (x, y) = (js_round(s.x), js_round(s.y));
        let cmd = s.cmd;
        let color = s.color;
        if cmd & MOVE == 0 {
            minx = minx.min(x);
            maxx = maxx.max(x);
            miny = miny.min(y);
            maxy = maxy.max(y);
        }
        let first = i == 0;

        if cmd & MOVE != 0 {
            let dist = (((x - px) as f64).powi(2) + ((y - py) as f64).powi(2)).sqrt();
            // Every cut is locked on both sides, however short the hop. (Here
            // we leave Respira, which cuts a short trimmed move bare: the
            // loose tail then pulls back up as the next area starts and
            // shreds at the needle bar guide.)
            if !first && (dist > FEED_LENGTH || cmd & TRIM != 0) {
                locks(&mut out, px, py, lock_direction(ss, i - 1, false));
                tag(&out, i.saturating_sub(1), color);
                push(&mut out, x, y, 0, FEED | CUT);
                locks(&mut out, x, y, lock_direction(ss, i, true));
                tag(&out, i, color);
                px = x;
                py = y;
                i += 1;
                continue;
            }
        }

        let mut yflag = 0;
        if cmd & MOVE != 0 {
            yflag |= FEED;
        }
        if cmd & TRIM != 0 {
            yflag |= CUT;
        }
        let last = i == ss.len() - 1 || cmd & END != 0;
        let next = ss.get(i + 1);
        let color_change = !last && next.is_some_and(|n| n.color != color);
        push(&mut out, x, y, if last { DATA_END } else { 0 }, yflag);
        px = x;
        py = y;

        if first {
            locks(&mut out, x, y, lock_direction(ss, i, true));
        }
        tag(&out, i, color);

        if color_change {
            let n = *next.unwrap();
            let (nx, ny) = (js_round(n.x), js_round(n.y));
            let next_is_jump = n.cmd & MOVE != 0;
            locks(&mut out, x, y, lock_direction(ss, i, true));
            push(&mut out, x, y, COLOR_END, CUT);
            tag(&out, i, color);
            if next_is_jump {
                i += 1;
            }
            if nx != x || ny != y {
                push(&mut out, nx, ny, 0, FEED);
            }
            // Mirrors Respira: `i` was already advanced past the jump, so this
            // looks one stitch further ahead than the jump target. Harmless (it
            // only picks the tie-in direction), and it keeps the output identical.
            let next_idx = if next_is_jump { i + 2 } else { i + 1 };
            let dir_idx = if next_idx < ss.len() { next_idx } else { i };
            locks(&mut out, nx, ny, lock_direction(ss, dir_idx, true));
            tag(&out, i, n.color);
            px = nx;
            py = ny;
        }
        if cmd & END != 0 {
            break;
        }
        i += 1;
    }
    let bounds = if minx == i32::MAX {
        (0, 0, 0, 0)
    } else {
        (minx, miny, maxx, maxy)
    };
    tag(&out, ss.len().saturating_sub(1), 0);
    // Input index → stitches of it reached (one per input stitch here;
    // `encode` maps them through to the design's own count).
    let design_at = src.iter().map(|&(i, _)| i).collect();
    let mut blocks: Vec<PenBlock> = Vec::new();
    for (k, &(_, c)) in src.iter().enumerate() {
        match blocks.last_mut() {
            Some(b) if b.block == c => b.end = k + 1,
            _ => blocks.push(PenBlock {
                start: k,
                end: k + 1,
                block: c,
            }),
        }
    }
    PenData {
        stitches: out.len() / 4,
        bytes: out,
        bounds,
        design_at,
        blocks,
    }
}

/// Decode PEN bytes back into (x, y, x_flags, y_flags), for tests and inspection.
pub fn decode(bytes: &[u8]) -> Vec<(i32, i32, u16, u16)> {
    bytes
        .chunks_exact(4)
        .map(|c| {
            let x = u16::from_le_bytes([c[0], c[1]]);
            let y = u16::from_le_bytes([c[2], c[3]]);
            ((x as i16 >> 3) as i32, (y as i16 >> 3) as i32, x & 7, y & 7)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::{Block, Run};
    use crate::geom::P;

    fn st(x: f64, y: f64, cmd: u32, color: usize) -> InStitch {
        InStitch { x, y, cmd, color }
    }

    /// Expected output worked out by hand from Respira's encodeStitchesToPen.
    #[test]
    fn matches_hand_computed_respira_sequence() {
        let input = [
            st(0.0, 0.0, 0, 0),
            st(10.0, 0.0, 0, 0),
            st(20.0, 0.0, 0, 0),
            st(20.0, 10.0, 0, 1),
        ];
        let got = decode(&encode_stitches(&input).bytes);
        let mut want: Vec<(i32, i32, u16, u16)> = vec![(0, 0, 0, 0)];
        // Tie-in at the start, pointing toward the next stitch (+x).
        for _ in 0..4 {
            want.extend([(2, 0, 0, 0), (-2, 0, 0, 0)]);
        }
        want.push((10, 0, 0, 0));
        want.push((20, 0, 0, 0));
        // Color change: tie-off toward the next stitch (+y), stop+cut, move, tie-in.
        for _ in 0..4 {
            want.extend([(20, 2, 0, 0), (20, -2, 0, 0)]);
        }
        want.push((20, 0, COLOR_END, CUT));
        want.push((20, 10, 0, FEED));
        // Nothing ahead to aim at: diagonal fallback (±1.41 → rounds to 21/19, 11/9).
        for _ in 0..4 {
            want.extend([(21, 11, 0, 0), (19, 9, 0, 0)]);
        }
        want.push((20, 10, DATA_END, 0));
        assert_eq!(got, want);
    }

    #[test]
    fn every_cut_is_locked_both_sides_long_jump_or_short_trim() {
        let input = [
            st(0.0, 0.0, 0, 0),
            st(10.0, 0.0, 0, 0),
            st(100.0, 0.0, MOVE, 0), // 9 mm jump: tie-off, cut, tie-in
            st(110.0, 0.0, 0, 0),
            st(120.0, 0.0, MOVE | TRIM, 0), // 1 mm trimmed move: the same
            st(130.0, 0.0, 0, 0),
        ];
        let got = decode(&encode_stitches(&input).bytes);
        let cuts: Vec<_> = got.iter().filter(|s| s.3 & CUT != 0).collect();
        assert_eq!(
            cuts,
            vec![&(100, 0, 0, FEED | CUT), &(120, 0, 0, FEED | CUT)]
        );
        // 1 + 8 (start) + 1, then twice 8 (tie-off) + 1 (cut) + 8 (tie-in) + 1.
        assert_eq!(got.len(), 46);
        assert_eq!(got.last().unwrap().2, DATA_END);
        // Round each cut: eight stitches where the thread ends (±0.2 mm
        // back along what was sewn), eight where it starts again.
        for k in (0..got.len()).filter(|&k| got[k].3 & CUT != 0) {
            let (from, to) = (got[k - 9], got[k]);
            for (j, s) in got[k - 8..k].iter().enumerate() {
                let dx = if j % 2 == 0 { -2 } else { 2 };
                assert_eq!(*s, (from.0 + dx, from.1, 0, 0), "tie-off at {k}");
            }
            for (j, s) in got[k + 1..k + 9].iter().enumerate() {
                let dx = if j % 2 == 0 { 2 } else { -2 };
                assert_eq!(*s, (to.0 + dx, to.1, 0, 0), "tie-in at {k}");
            }
        }
    }

    #[test]
    fn a_short_move_the_plan_does_not_trim_is_not_cut() {
        let input = [
            st(0.0, 0.0, 0, 0),
            st(10.0, 0.0, 0, 0),
            st(40.0, 0.0, MOVE, 0), // 3 mm, no trim: carried across
            st(50.0, 0.0, 0, 0),
        ];
        let got = decode(&encode_stitches(&input).bytes);
        assert!(got.iter().all(|s| s.3 & CUT == 0));
        // 1 + 8 (start) + 1 + 1 (feed) + 1
        assert_eq!(got.len(), 12);
        assert_eq!(got[10], (40, 0, 0, FEED));
    }

    #[test]
    fn negative_coordinates_wrap_like_js() {
        let got = decode(&encode_stitches(&[st(-37.0, -1.0, 0, 0), st(-20.0, -1.0, 0, 0)]).bytes);
        assert_eq!(got[0], (-37, -1, 0, 0));
        assert_eq!(got.last().unwrap(), &(-20, -1, DATA_END, 0));
    }

    /// A run that comes to one point is only a move: the stream must still
    /// end each thread and the data.
    #[test]
    fn a_speck_far_away_doesnt_swallow_the_end_markers() {
        let line = |x: f64, y: f64| Run {
            pts: vec![P::new(x, y), P::new(x + 20.0, y)],
            trim_before: false,
        };
        let speck = |x: f64, y: f64| Run {
            pts: vec![P::new(x, y), P::new(x + 0.2, y)],
            trim_before: true,
        };
        let block = |runs: Vec<Run>| Block {
            color: [0, 0, 0],
            runs,
        };
        let d = Design {
            name: "t".into(),
            blocks: vec![
                block(vec![line(0.0, 0.0), speck(300.0, 0.0)]),
                block(vec![line(0.0, 100.0), speck(300.0, 100.0)]),
            ],
            ..Design::default()
        };
        let pen = encode(&d);
        let dec = decode(&pen.bytes);
        assert_eq!(dec.iter().filter(|x| x.2 == COLOR_END).count(), 1);
        assert_eq!(dec.last().unwrap().2, DATA_END);
        assert_eq!(pen.blocks.len(), 2);
        assert_eq!(*pen.design_at.last().unwrap() as usize, d.stitch_count());
    }

    #[test]
    fn design_flattening_marks_trims_and_colors() {
        let d = Design {
            name: "t".into(),
            blocks: vec![
                Block {
                    color: [255, 0, 0],
                    runs: vec![
                        Run {
                            pts: vec![P::new(-100.0, 0.0), P::new(-80.0, 0.0), P::new(-60.0, 5.0)],
                            trim_before: false,
                        },
                        Run {
                            pts: vec![P::new(40.0, 40.0), P::new(60.0, 40.0)],
                            trim_before: true,
                        },
                    ],
                },
                Block {
                    color: [0, 0, 255],
                    runs: vec![Run {
                        pts: vec![P::new(0.0, -50.0), P::new(20.0, -50.0)],
                        trim_before: false,
                    }],
                },
            ],
            ..Design::default()
        };
        let s = stitches(&d);
        assert_eq!(s[3], st(40.0, 40.0, MOVE | TRIM, 0));
        assert_eq!(s[5], st(0.0, -50.0, MOVE, 1));
        let pen = encode(&d);
        assert_eq!(pen.bounds, (-100, -50, 60, 40));
        let dec = decode(&pen.bytes);
        assert_eq!(dec.iter().filter(|x| x.2 == COLOR_END).count(), 1);
        assert_eq!(dec.iter().filter(|x| x.2 == DATA_END).count(), 1);
        // Every record maps back onto the design, never going backwards,
        // reaching all of it at the end.
        assert_eq!(pen.design_at.len(), dec.len());
        assert!(pen.design_at.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(*pen.design_at.last().unwrap() as usize, d.stitch_count());
        // Two colors: the first ends with its COLOR_END record.
        assert_eq!(pen.blocks.len(), 2);
        assert_eq!((pen.blocks[0].block, pen.blocks[1].block), (0, 1));
        assert_eq!(dec[pen.blocks[0].end - 1].2, COLOR_END);
        assert_eq!(pen.blocks[1].end, dec.len());
        assert_eq!(pen.block_at(pen.blocks[1].start), Some(1));
        // Waiting for a thread change on either side of the boundary: the
        // next thread either way.
        let b1 = pen.blocks[1].start;
        assert_eq!(pen.thread_at(b1, true), Some(1));
        assert_eq!(pen.thread_at(b1 - 1, true), Some(1));
        assert_eq!(pen.thread_at(b1 - 1, false), Some(0));
    }
}
