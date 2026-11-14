//! The **parametric EQ response-curve graph** — an Ableton EQ Eight / FabFilter
//! Pro-Q style plot: a log-frequency × dB display of the combined EQ response,
//! with a draggable node per band. Dragging a node moves its **frequency** (x)
//! and **gain** (y); the **mouse wheel** over the graph widens/narrows the nearest
//! band's **Q** (curvature); **double-clicking** a node cycles its filter shape.
//! Gain-less shapes (cuts / notch) have no gain to set, so their vertical drag
//! adjusts Q instead.
//!
//! The curve, grid and fill are drawn with [`rune::Shape`] in fractional
//! coordinates; the nodes are absolutely-positioned handles on top. All pointer
//! gestures live on the graph container (whose `Drag`/`WheelEvent` coordinates are
//! in its own layout points), so the handles are purely visual.
#![forbid(unsafe_code)]

use rune::{
    Color, Cursor, Cx, Drag, El, LineCap, Phase, Shape, WheelEvent, canvas, col, el, row, text,
};
use soul_player_host::{
    BANDS, Band, BandKind, FREQ_MAX, FREQ_MIN, GAIN_LIMIT_DB, Q_MAX, Q_MIN, band_magnitude_db,
    eq_magnitude_db,
};

use crate::theme::Palette;
use crate::ui::{radius, space, type_};
use crate::{State, cycle_eq_band_kind, set_eq_band, set_eq_band_enabled, set_eq_band_q};

/// Graph height, in points.
const GRAPH_H: f32 = 190.0;
/// Fallback graph width before the first layout commits.
const FALLBACK_W: f32 = 780.0;
/// dB half-span shown (the plot runs −`DB_SPAN`..+`DB_SPAN`).
const DB_SPAN: f32 = GAIN_LIMIT_DB;
/// Representative device rate for the drawn curve — its shape is near-identical at
/// 44.1/48 kHz; the real filtering uses the actual device rate.
const VIZ_FS: f32 = 48_000.0;
/// Node handle diameter, in points.
const NODE_D: f32 = 15.0;
/// How near (points) a press must land to a node to count as grabbing/cycling it.
const GRAB_PX: f32 = 20.0;
/// Curve resolution (polyline points across the frequency range).
const CURVE_N: usize = 160;
/// Geometric Q step per wheel notch (Shift = fine).
const Q_STEP: f32 = 1.15;
const Q_STEP_FINE: f32 = 1.05;
/// Frequency axis ticks: vertical grid lines + bottom labels.
const FREQ_TICKS: [(f32, &str); 3] = [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")];
/// dB axis ticks: horizontal grid lines + left-edge labels.
const DB_TICKS: [(f32, &str); 3] = [(12.0, "+12"), (0.0, "0"), (-12.0, "−12")];

/// Fractional x (0..1) of a frequency on the log axis.
fn x_frac(f: f32) -> f32 {
    (f.ln() - FREQ_MIN.ln()) / (FREQ_MAX.ln() - FREQ_MIN.ln())
}

/// The frequency (Hz) at fractional x (the inverse of [`x_frac`]).
fn freq_at(xf: f32) -> f32 {
    FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(xf.clamp(0.0, 1.0))
}

/// Fractional y (0..1, top-down) of a dB value — `+DB_SPAN` at the top, 0 dB
/// centred, `−DB_SPAN` at the bottom.
fn y_frac(db: f32) -> f32 {
    (0.5 - db / (2.0 * DB_SPAN)).clamp(0.0, 1.0)
}

/// The gain (dB) at fractional y (the inverse of [`y_frac`]).
fn db_at(yf: f32) -> f32 {
    (0.5 - yf.clamp(0.0, 1.0)) * 2.0 * DB_SPAN
}

/// The Q at fractional y for a gain-less band: the top is narrow (`Q_MAX`), the
/// bottom wide (`Q_MIN`), on a log scale.
fn q_at(yf: f32) -> f32 {
    Q_MAX * (Q_MIN / Q_MAX).powf(yf.clamp(0.0, 1.0))
}

/// A node's fractional y: its gain for gain shapes, the centre line for cuts/notch.
fn node_yf(band: &Band) -> f32 {
    if band.kind.uses_gain() {
        y_frac(band.gain_db)
    } else {
        y_frac(0.0)
    }
}

/// The index of the band whose node is nearest fractional x.
fn nearest_band(bands: &[Band; BANDS], xf: f32) -> usize {
    (0..BANDS)
        .min_by(|&a, &b| {
            (x_frac(bands[a].freq) - xf)
                .abs()
                .total_cmp(&(x_frac(bands[b].freq) - xf).abs())
        })
        .unwrap_or(0)
}

/// A short human label for a filter shape.
fn kind_label(kind: BandKind) -> &'static str {
    match kind {
        BandKind::LowShelf => "Low shelf",
        BandKind::Peaking => "Bell",
        BandKind::HighShelf => "High shelf",
        BandKind::LowCut => "Low cut",
        BandKind::HighCut => "High cut",
        BandKind::Notch => "Notch",
    }
}

/// `"250 Hz"` / `"4.5 kHz"`.
fn fmt_hz(f: f32) -> String {
    if f >= 1000.0 {
        format!("{:.1} kHz", f / 1000.0)
    } else {
        format!("{f:.0} Hz")
    }
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

/// A response curve as fractional points, log-spaced across the range, from a
/// `freq -> dB` function.
fn curve_points(mag_db: impl Fn(f32) -> f32) -> Vec<[f32; 2]> {
    (0..=CURVE_N)
        .map(|i| {
            let t = i as f32 / CURVE_N as f32;
            let f = freq_at(t);
            [t, y_frac(mag_db(f))]
        })
        .collect()
}

/// The grid + 0 dB baseline + filled area + (selected band + overall) response
/// curves, in fractional [0,1]² coordinates.
fn graph_shape(bands: &[Band; BANDS], preamp: f32, selected: Option<usize>, p: &Palette) -> Shape {
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
    let curve = curve_points(|f| eq_magnitude_db(bands, preamp, f, VIZ_FS));
    // Fill the band between the curve and the 0 dB baseline, split into contiguous
    // runs at each baseline crossing so boosts *and* cuts shade without the
    // self-intersection a single polygon would produce.
    let fill = p.primary.alpha(0.14);
    let mut run: Vec<[f32; 2]> = Vec::new();
    for w in curve.windows(2) {
        let (a, b) = (w[0], w[1]);
        if run.is_empty() {
            run.push(a);
        }
        let (sa, sb) = (a[1] - base, b[1] - base);
        if (sa < 0.0 && sb > 0.0) || (sa > 0.0 && sb < 0.0) {
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
    // 0 dB baseline a touch brighter.
    sh = sh.line([0.0, base], [1.0, base], 1.0, LineCap::Butt, p.border);
    // The selected band's own (thin) response, so you see its shape in isolation.
    if let Some(i) = selected {
        let band = bands[i];
        if band.enabled {
            let bc = curve_points(|f| band_magnitude_db(&band, f, VIZ_FS));
            sh = sh.stroke(bc, 1.0, p.accent.alpha(0.85));
        }
    }
    // The overall curve, thick, on top.
    sh.stroke(curve, 2.0, p.primary)
}

/// A small readout of the selected band (shape · frequency · gain · Q) plus a
/// per-band bypass toggle.
fn readout(i: usize, band: &Band, p: &Palette) -> El {
    let txt = if band.kind.uses_gain() {
        format!(
            "{} · {} · {:+.1} dB · Q {:.2}",
            kind_label(band.kind),
            fmt_hz(band.freq),
            band.gain_db,
            band.q
        )
    } else {
        format!(
            "{} · {} · Q {:.2}",
            kind_label(band.kind),
            fmt_hz(band.freq),
            band.q
        )
    };
    let enabled = band.enabled;
    let chip = text(if enabled { "On" } else { "Bypassed" }.to_string())
        .font(type_::LABEL)
        .px(space::SM)
        .py(space::XXS)
        .radius(radius::SM)
        .key("eq-band-bypass")
        .role("button")
        .focusable(true)
        .cursor(Cursor::Pointer)
        .on_press(move |st: &mut State| set_eq_band_enabled(st, i, !enabled));
    let chip = if enabled {
        chip.fill(p.primary.alpha(0.16)).color(p.primary)
    } else {
        chip.fill(p.muted).color(p.text_secondary)
    };
    // A floating pill (in the darker page colour) so the readout reads clearly over
    // the curve and clears the axis labels.
    row()
        .key("eq-readout")
        .absolute(space::SM, space::SM)
        .gap(space::SM)
        .center_y()
        .fill(p.bg.alpha(0.92))
        .px(space::SM)
        .py(space::XXS)
        .radius(radius::SM)
        .border_sides([1.0; 4], p.border.alpha(0.6))
        .child(text(txt).font(type_::CAPTION).color(p.fg))
        .child(chip)
}

/// The response-curve graph: the `canvas`, axis labels, and a draggable handle per
/// band. All gestures live on the container; `cx` gives its laid-out size so the
/// handles sit on the curve.
pub(crate) fn eq_graph(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let [gw, gh] = cx.layout_size("eq-graph").unwrap_or([FALLBACK_W, GRAPH_H]);
    let mut graph = col()
        .key("eq-graph")
        .w_full()
        .h(GRAPH_H)
        .child(
            canvas(graph_shape(&s.eq_bands, s.eq_preamp, s.eq_selected, p))
                .w_full()
                .h_full(),
        )
        // Drag a handle: x = frequency, y = gain (or Q for a gain-less shape).
        // Double-click a handle cycles its filter shape. The nearest band to the
        // press point is the one grabbed.
        .on_drag(move |st: &mut State, d: &mut Drag, _tx| {
            let xf = (d.x / gw.max(1.0)).clamp(0.0, 1.0);
            let yf = (d.y / gh.max(1.0)).clamp(0.0, 1.0);
            match d.phase {
                Phase::Down => {
                    let i = nearest_band(&st.eq_bands, xf);
                    st.eq_selected = Some(i);
                    // Grab only if the press is near the handle; a double-click on
                    // it cycles the shape instead of starting a drag.
                    let band = st.eq_bands[i];
                    let near = {
                        let dx = x_frac(band.freq) * gw - d.x;
                        let dy = node_yf(&band) * gh - d.y;
                        (dx * dx + dy * dy).sqrt() <= GRAB_PX
                    };
                    if d.clicks >= 2 && near {
                        cycle_eq_band_kind(st, i);
                        st.eq_drag = None;
                    } else {
                        st.eq_drag = Some((i, band));
                    }
                }
                Phase::Move => {
                    if let Some((i, start)) = st.eq_drag {
                        let mut b = start;
                        b.freq = freq_at(xf);
                        if b.kind.uses_gain() {
                            b.gain_db = db_at(yf);
                        } else {
                            b.q = q_at(yf);
                        }
                        set_eq_band(st, i, b);
                    }
                }
                Phase::Up => st.eq_drag = None,
            }
        })
        // Wheel over the graph adjusts the nearest band's Q (curvature); Shift fines
        // the step. Consume it so the settings page doesn't scroll.
        .on_wheel(move |st: &mut State, e: WheelEvent, _tx| {
            let xf = (e.x / gw.max(1.0)).clamp(0.0, 1.0);
            let i = nearest_band(&st.eq_bands, xf);
            st.eq_selected = Some(i);
            let step = if e.mods.shift { Q_STEP_FINE } else { Q_STEP };
            let factor = if e.dy < 0.0 { step } else { 1.0 / step };
            set_eq_band_q(st, i, st.eq_bands[i].q * factor);
            true
        });
    // Axis tick labels, under the nodes: frequency along the bottom, dB up the left.
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
    // Band handles (visual only — the container owns the gestures).
    for i in 0..BANDS {
        let band = s.eq_bands[i];
        let selected = s.eq_selected == Some(i);
        let node_x = x_frac(band.freq).clamp(0.0, 1.0) * gw - NODE_D / 2.0;
        let node_y = node_yf(&band) * gh - NODE_D / 2.0;
        let color = if !band.enabled {
            p.muted
        } else if selected {
            p.accent
        } else {
            p.primary
        };
        let node = el()
            .key(format!("eq-node-{i}"))
            .absolute(node_x, node_y)
            .size(NODE_D, NODE_D)
            .radius(NODE_D / 2.0)
            .fill(color)
            // A ring in the page colour so the handle reads clearly over the curve.
            .border_sides([if selected { 2.5 } else { 2.0 }; 4], p.bg)
            .cursor(Cursor::Pointer);
        graph = graph.child(node);
    }
    if let Some(i) = s.eq_selected {
        graph = graph.child(readout(i, &s.eq_bands[i], p));
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use soul_player_host::default_bands;

    #[test]
    fn frequency_axis_is_an_invertible_log_map() {
        assert!((x_frac(FREQ_MIN) - 0.0).abs() < 1e-6);
        assert!((x_frac(FREQ_MAX) - 1.0).abs() < 1e-6);
        // 1 kHz sits past centre on a 20..20k log axis.
        assert!(x_frac(1000.0) > 0.5 && x_frac(1000.0) < 0.6);
        // round-trips
        for &f in &[50.0, 440.0, 1000.0, 8000.0] {
            assert!((freq_at(x_frac(f)) - f).abs() < f * 1e-3);
        }
    }

    #[test]
    fn gain_axis_centres_zero_and_clamps() {
        assert!((y_frac(0.0) - 0.5).abs() < 1e-6);
        assert!((y_frac(DB_SPAN) - 0.0).abs() < 1e-6);
        assert!((y_frac(-DB_SPAN) - 1.0).abs() < 1e-6);
        assert_eq!(y_frac(DB_SPAN * 3.0), 0.0);
        // db_at inverts y_frac inside range
        assert!((db_at(y_frac(6.0)) - 6.0).abs() < 1e-3);
        assert!((db_at(y_frac(-9.0)) + 9.0).abs() < 1e-3);
    }

    #[test]
    fn q_maps_narrow_at_top_wide_at_bottom() {
        assert!((q_at(0.0) - Q_MAX).abs() < 1e-3);
        assert!((q_at(1.0) - Q_MIN).abs() < 1e-3);
        assert!(
            q_at(0.3) > q_at(0.7),
            "higher up the plot = narrower (bigger Q)"
        );
    }

    #[test]
    fn nearest_band_picks_the_closest_in_log_frequency() {
        let bands = default_bands();
        // default band 0 is the 80 Hz low shelf, band 7 the 15 kHz high shelf.
        assert_eq!(nearest_band(&bands, x_frac(85.0)), 0);
        assert_eq!(nearest_band(&bands, x_frac(14_000.0)), 7);
        // 2 kHz is band index 4 in the default layout.
        assert_eq!(nearest_band(&bands, x_frac(2000.0)), 4);
    }

    #[test]
    fn a_boosted_band_lifts_the_overall_curve_above_centre() {
        let mut bands = default_bands();
        bands[4].gain_db = GAIN_LIMIT_DB; // 2 kHz bell
        let curve = curve_points(|f| eq_magnitude_db(&bands, 0.0, f, VIZ_FS));
        let near_2k = curve
            .iter()
            .min_by(|a, b| {
                (a[0] - x_frac(2000.0))
                    .abs()
                    .total_cmp(&(b[0] - x_frac(2000.0)).abs())
            })
            .unwrap();
        assert!(near_2k[1] < 0.5, "a boost should sit above the centre line");
    }
}
