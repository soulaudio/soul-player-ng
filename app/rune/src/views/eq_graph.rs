//! The EQ **response-curve graph** — an Ableton EQ Eight / FabFilter Pro-Q style
//! plot: a log-frequency × dB display of the combined EQ response, with a
//! draggable node per band (drag up/down to set that band's gain). The curve and
//! grid are drawn with [`rune::Shape`] in fractional coordinates; the nodes are
//! absolutely-positioned handles on top.
#![forbid(unsafe_code)]

use rune::{Color, Cursor, Cx, Drag, El, LineCap, Phase, Shape, canvas, col, el, text};
use soul_player_host::{BAND_FREQS, BANDS, GAIN_LIMIT_DB, eq_magnitude_db};

use crate::theme::Palette;
use crate::ui::type_;
use crate::{State, set_eq_band};

/// Graph height, in points.
const GRAPH_H: f32 = 160.0;
/// Fallback graph width before the first layout commits (≈ the card's inner
/// width), so the node handles land close on the very first frame / a snapshot.
const FALLBACK_W: f32 = 780.0;
/// dB half-span shown (the plot runs −`DB_SPAN`..+`DB_SPAN`).
const DB_SPAN: f32 = GAIN_LIMIT_DB;
/// Displayed frequency range, in Hz.
const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20_000.0;
/// Representative device rate for the drawn curve — its shape is near-identical
/// at 44.1/48 kHz; the real filtering uses the actual device rate.
const VIZ_FS: f32 = 48_000.0;
/// Node handle diameter, in points.
const NODE_D: f32 = 14.0;
/// Curve resolution (polyline points across the frequency range).
const CURVE_N: usize = 120;
/// Frequency axis ticks drawn as vertical grid lines + bottom labels.
const FREQ_TICKS: [(f32, &str); 3] = [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")];
/// dB axis ticks drawn as horizontal grid lines + left-edge labels.
const DB_TICKS: [(f32, &str); 3] = [(12.0, "+12"), (0.0, "0"), (-12.0, "−12")];

/// Fractional x (0..1) of a frequency on the log axis.
fn x_frac(f: f32) -> f32 {
    (f.ln() - F_MIN.ln()) / (F_MAX.ln() - F_MIN.ln())
}

/// Fractional y (0..1, top-down) of a dB value — `+DB_SPAN` at the top, 0 dB
/// centred, `−DB_SPAN` at the bottom.
fn y_frac(db: f32) -> f32 {
    (0.5 - db / (2.0 * DB_SPAN)).clamp(0.0, 1.0)
}

/// Fill the band between one contiguous curve `run` (all on a single side of the
/// baseline) and the `base` line, as a simple non-self-intersecting polygon.
fn fill_run_to_base(sh: Shape, run: &[[f32; 2]], base: f32, color: Color) -> Shape {
    if run.len() < 2 {
        return sh;
    }
    let mut poly = run.to_vec();
    poly.push([run[run.len() - 1][0], base]);
    poly.push([run[0][0], base]);
    sh.fill(poly, color)
}

/// The response curve as fractional points, log-spaced across the range.
fn curve_points(gains: &[f32; BANDS], preamp: f32) -> Vec<[f32; 2]> {
    (0..=CURVE_N)
        .map(|i| {
            let t = i as f32 / CURVE_N as f32;
            let f = F_MIN * (F_MAX / F_MIN).powf(t);
            [x_frac(f), y_frac(eq_magnitude_db(gains, preamp, f, VIZ_FS))]
        })
        .collect()
}

/// The grid + 0 dB baseline + filled area + response curve, in fractional [0,1]²
/// coordinates (a `view_box` stretches them to the element's size).
fn graph_shape(gains: &[f32; BANDS], preamp: f32, p: &Palette) -> Shape {
    let grid = p.border.alpha(0.5);
    let base = y_frac(0.0);
    let mut sh = Shape::new().view_box(1.0, 1.0);
    // Horizontal dB grid (±6, ±12 edges).
    for &db in &[-12.0, -6.0, 6.0, 12.0] {
        let y = y_frac(db);
        sh = sh.line([0.0, y], [1.0, y], 1.0, LineCap::Butt, grid);
    }
    // Vertical decade grid (100 Hz / 1 kHz / 10 kHz).
    for (f, _) in FREQ_TICKS {
        let x = x_frac(f);
        sh = sh.line([x, 0.0], [x, 1.0], 1.0, LineCap::Butt, grid);
    }
    let curve = curve_points(gains, preamp);
    // Fill the band between the curve and the 0 dB baseline. Split into contiguous
    // runs at each baseline crossing so boosts *and* cuts shade (a single polygon
    // would self-intersect at a crossing and drop the below-baseline lobe). Runs
    // meet exactly at the crossing point on the baseline, so there are no seams.
    let fill = p.primary.alpha(0.14);
    let mut run: Vec<[f32; 2]> = Vec::new();
    for w in curve.windows(2) {
        let (a, b) = (w[0], w[1]);
        if run.is_empty() {
            run.push(a);
        }
        let (sa, sb) = (a[1] - base, b[1] - base);
        if (sa < 0.0 && sb > 0.0) || (sa > 0.0 && sb < 0.0) {
            // Crossing: interpolate where the segment meets the baseline, close the
            // current run there, and start the next run from the same point.
            let t = sa.abs() / (sa.abs() + sb.abs());
            let xc = a[0] + (b[0] - a[0]) * t;
            run.push([xc, base]);
            sh = fill_run_to_base(sh, &run, base, fill);
            run = vec![[xc, base], b];
        } else {
            run.push(b);
        }
    }
    sh = fill_run_to_base(sh, &run, base, fill);
    // 0 dB baseline a touch brighter, then the response curve on top.
    sh.line([0.0, base], [1.0, base], 1.0, LineCap::Butt, p.border)
        .stroke(curve, 2.0, p.primary)
}

/// The response-curve graph: the `canvas` plus a draggable handle per band. `cx`
/// gives the graph's laid-out size, so the handles sit on the curve.
pub(crate) fn eq_graph(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let [gw, gh] = cx.layout_size("eq-graph").unwrap_or([FALLBACK_W, GRAPH_H]);
    let mut graph = col().key("eq-graph").w_full().h(GRAPH_H).child(
        canvas(graph_shape(&s.eq_gains, s.eq_preamp, p))
            .w_full()
            .h_full(),
    );
    // Axis tick labels, drawn under the nodes: frequency along the bottom, dB up
    // the left edge. Muted so the curve reads clearly over them.
    let tick = p.text_secondary.alpha(0.7);
    for (f, label) in FREQ_TICKS {
        graph = graph.child(
            el().absolute(x_frac(f) * gw + 3.0, gh - 16.0)
                .child(text(label).font(type_::CAPTION).color(tick)),
        );
    }
    for (db, label) in DB_TICKS {
        graph = graph.child(
            el().absolute(4.0, (y_frac(db) * gh - 7.0).max(1.0))
                .child(text(label).font(type_::CAPTION).color(tick)),
        );
    }
    for i in 0..BANDS {
        let node_x = x_frac(BAND_FREQS[i]) * gw - NODE_D / 2.0;
        let node_y = y_frac(s.eq_gains[i]) * gh - NODE_D / 2.0;
        let node = el()
            .key(format!("eq-node-{i}"))
            .role("button")
            .focusable(true)
            .absolute(node_x, node_y)
            .size(NODE_D, NODE_D)
            .radius(NODE_D / 2.0)
            .fill(p.primary)
            // A ring in the page colour so the handle reads clearly over the curve.
            .border_sides([2.0; 4], p.bg)
            .cursor(Cursor::Pointer)
            // Drag up/down to set the band gain; `dy` is total travel since press,
            // mapped onto the dB span. Horizontal travel is ignored (fixed-frequency
            // graphic EQ). Caches the gain at press so the drag is absolute.
            .on_drag(move |st: &mut State, d: &mut Drag, _tx| match d.phase {
                Phase::Down => st.eq_drag = Some((i, st.eq_gains[i])),
                Phase::Move => {
                    if let Some((band, start)) = st.eq_drag {
                        let ddb = -d.dy / gh.max(1.0) * (2.0 * DB_SPAN);
                        let db = (start + ddb).clamp(-GAIN_LIMIT_DB, GAIN_LIMIT_DB);
                        set_eq_band(st, band, (db * 10.0).round() / 10.0);
                    }
                }
                Phase::Up => st.eq_drag = None,
            });
        graph = graph.child(node);
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_mapping_anchors_the_frequency_and_db_extents() {
        // Frequency maps log-wise: F_MIN at the left edge, F_MAX at the right.
        assert!((x_frac(F_MIN) - 0.0).abs() < 1e-6);
        assert!((x_frac(F_MAX) - 1.0).abs() < 1e-6);
        // 1 kHz sits past centre on a 20..20k log axis.
        assert!(x_frac(1000.0) > 0.5 && x_frac(1000.0) < 0.6);

        // dB maps with 0 dB centred, +span at the top (y=0), −span at the bottom.
        assert!((y_frac(0.0) - 0.5).abs() < 1e-6);
        assert!((y_frac(DB_SPAN) - 0.0).abs() < 1e-6);
        assert!((y_frac(-DB_SPAN) - 1.0).abs() < 1e-6);
        // Out-of-range gains clamp into the plot.
        assert_eq!(y_frac(DB_SPAN * 3.0), 0.0);
        assert_eq!(y_frac(-DB_SPAN * 3.0), 1.0);
    }

    #[test]
    fn a_flat_eq_draws_a_centred_curve() {
        let flat = curve_points(&[0.0; BANDS], 0.0);
        assert_eq!(flat.len(), CURVE_N + 1);
        // Every sample sits on the 0 dB centre line, left to right.
        assert!((flat[0][0] - 0.0).abs() < 1e-6);
        assert!((flat[CURVE_N][0] - 1.0).abs() < 1e-6);
        assert!(flat.iter().all(|p| (p[1] - 0.5).abs() < 1e-6));
    }

    #[test]
    fn a_boosted_band_lifts_the_curve_above_centre() {
        // A big lift on the 1 kHz band pulls the nearest curve sample up (y < 0.5).
        let mut gains = [0.0; BANDS];
        gains[5] = GAIN_LIMIT_DB; // BAND_FREQS[5] == 1000 Hz
        let curve = curve_points(&gains, 0.0);
        let near_1k = curve
            .iter()
            .min_by(|a, b| {
                (a[0] - x_frac(1000.0))
                    .abs()
                    .total_cmp(&(b[0] - x_frac(1000.0)).abs())
            })
            .unwrap();
        assert!(near_1k[1] < 0.5, "a boost should sit above the centre line");
    }
}
