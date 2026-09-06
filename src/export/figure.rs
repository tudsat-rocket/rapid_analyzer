//! The exported graph itself: what a figure is made of, and how it is drawn.
//!
//! This is the part that knows what a graph looks like, and nothing about what
//! it is being drawn *into* -- every mark goes through [`Canvas`], which SVG
//! (`super::svg`) and PNG (`super::raster`) implement in their own terms. One
//! layout, one set of ticks, one legend, two files that agree with each other.
//!
//! Coordinates are "figure units": CSS pixels, 96 to the inch, y down. That is
//! what an SVG `viewBox` is in, and the raster backend gets there by scaling by
//! `dpi / 96`. Font sizes are in the same unit (a point is 96/72 of one).

use egui::{Align2, Color32, Pos2, Rect, Vec2, pos2, vec2};

/// Where a figure's ink goes. Positions and sizes are in figure units.
pub trait Canvas {
    /// The whole drawable area.
    fn size(&self) -> Vec2;

    fn fill_rect(&mut self, rect: Rect, color: Color32);

    fn stroke_rect(&mut self, rect: Rect, color: Color32, width: f32);

    /// An open polyline. Fewer than two points draws nothing.
    fn polyline(&mut self, points: &[Pos2], color: Color32, width: f32);

    /// One cell of a grid of them, filled flat.
    ///
    /// Unlike [`Canvas::fill_rect`] the edges are *not* anti-aliased: two
    /// cells sharing an edge would each cover their side of it partially, and
    /// the background would show through the seam as a hairline. A heat map
    /// made of a few hundred of those is a picture of stripes.
    fn fill_cell(&mut self, rect: Rect, color: Color32);

    /// A rectangle filled with a top-to-bottom gradient: `stops` are
    /// `(distance down the rect, 0..1; colour)`, in order. Its edges are
    /// crisp, for the same reason [`Canvas::fill_cell`]'s are.
    ///
    /// This exists for the tank strip, where the colour between two sensors is
    /// an interpolation and drawing it as a stack of flat bands would put
    /// edges in the picture that the data does not have. A gradient stays an
    /// interpolation in the SVG too, at whatever resolution it is printed at.
    fn vertical_gradient(&mut self, rect: Rect, stops: &[(f32, Color32)]);

    /// One line of text, positioned by which of its corners/edges `align`
    /// names sitting at `anchor`.
    fn text(&mut self, anchor: Pos2, align: Align2, text: &str, size: f32, color: Color32);

    /// One line of text turned a quarter turn anticlockwise -- reading bottom
    /// to top -- centred on `center`. The y-axis label, in other words.
    fn text_rotated(&mut self, center: Pos2, text: &str, size: f32, color: Color32);

    /// Size `text` would take up, for working out how much room to leave it.
    fn measure(&mut self, text: &str, size: f32) -> Vec2;

    /// Confines subsequent drawing to `rect`, or lifts the restriction.
    fn clip(&mut self, rect: Option<Rect>);
}

/// One series in an exported panel. Values are final: normalization, and the
/// right axis' own scale, are the figure builder's business, not the drawing's.
pub struct Series {
    pub label: String,
    pub color: Color32,
    pub right_axis: bool,
    /// `[master time (UTC seconds), value]`, time-ordered.
    pub points: Vec<[f64; 2]>,
}

/// One pane in the figure. A figure with several stacks them on a shared time
/// axis, which is the whole reason for exporting more than one at once -- and
/// why a tank is a panel here rather than a picture of its own: the point of
/// putting it under a chamber pressure is reading the two against each other.
pub struct Panel {
    pub title: String,
    pub content: Content,
}

pub enum Content {
    Graph(Graph),
    Tank(Tank),
}

impl Panel {
    fn graph(&self) -> Option<&Graph> {
        match &self.content {
            Content::Graph(graph) => Some(graph),
            Content::Tank(_) => None,
        }
    }
}

/// Any number of series against one or two value axes.
pub struct Graph {
    pub series: Vec<Series>,
    pub left: (f64, f64),
    /// `None` when nothing is drawn against a second axis.
    pub right: Option<(f64, f64)>,
    pub left_label: String,
    pub right_label: String,
}

impl Graph {
    fn has_right(&self) -> bool {
        self.right.is_some() && self.series.iter().any(|s| s.right_axis)
    }
}

/// The tank wall as the pane draws it: one temperature per sensor per moment,
/// colour for temperature, height up the panel.
///
/// The field is [`crate::tank::Field`] itself rather than a copy of it, so the
/// figure is sampled by the same code the pane samples with -- including the
/// hold that decides where the strip has holes in it.
pub struct Tank {
    pub field: crate::tank::Field,
    /// Ends of the colour ramp, in °C.
    pub lo_c: f64,
    pub hi_c: f64,
    /// One flat rectangle per sensor per moment, instead of shading between
    /// neighbours.
    pub blocks: bool,
    /// A line at each sensor's height.
    pub grid: bool,
}

pub struct Figure {
    pub panels: Vec<Panel>,
    /// The exported window, in master (UTC) seconds.
    pub range: (f64, f64),
    /// The playhead, if it is to be marked and falls inside `range`.
    pub cursor: Option<f64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LegendPos {
    Off,
    TopLeft,
    TopRight,
    Below,
}

impl LegendPos {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::TopLeft => "top left",
            Self::TopRight => "top right",
            Self::Below => "below the graph",
        }
    }
}

/// Everything about a figure that is a matter of taste rather than of data.
pub struct Style {
    pub palette: Palette,
    /// Body text size, in figure units.
    pub font: f32,
    pub line_width: f32,
    pub padding: f32,
    pub grid: bool,
    pub legend: LegendPos,
    pub titles: bool,
    pub cursor: bool,
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub plot_bg: Color32,
    /// Inside the tank, where nothing was measured. Distinct from the page,
    /// or a dropout would look like a hole in the *vessel* rather than in the
    /// data.
    pub hole: Color32,
    pub frame: Color32,
    pub grid: Color32,
    pub text: Color32,
    pub weak_text: Color32,
    pub cursor: Color32,
}

impl Palette {
    /// The default, and the one a report wants: black on white.
    pub fn light() -> Self {
        Self {
            bg: Color32::WHITE,
            plot_bg: Color32::WHITE,
            hole: Color32::from_gray(0xEE),
            frame: Color32::from_gray(0x55),
            grid: Color32::from_gray(0xDD),
            text: Color32::from_gray(0x20),
            weak_text: Color32::from_gray(0x60),
            cursor: Color32::from_rgb(0xFF, 0x5C, 0x3D),
        }
    }

    /// For a slide, or a report that is read on a screen.
    pub fn dark() -> Self {
        Self {
            bg: Color32::from_gray(0x1B),
            plot_bg: Color32::from_gray(0x11),
            hole: Color32::from_gray(0x0A),
            frame: Color32::from_gray(0x77),
            grid: Color32::from_gray(0x33),
            text: Color32::from_gray(0xEE),
            weak_text: Color32::from_gray(0xAA),
            cursor: Color32::from_rgb(0xFF, 0x5C, 0x3D),
        }
    }

    /// Same colours, but with nothing painted behind the graph.
    pub fn transparent(mut self) -> Self {
        self.bg = Color32::TRANSPARENT;
        self.plot_bg = Color32::TRANSPARENT;
        self.hole = Color32::TRANSPARENT;
        self
    }
}

/// Smallest panel worth drawing into, in figure units. Below this the margins
/// alone would exceed the panel and the plot rect would come out inside out.
const MIN_PLOT_SIDE: f32 = 24.0;

/// How much wider than measured a string is given room to be.
///
/// An SVG's text is drawn by the viewer, in a font we do not have and cannot
/// measure. Leaving every label a few percent of slack is what keeps a tick
/// label off the graph and a legend inside its box there -- and it is applied
/// to *both* backends, so the raster preview shows the layout the SVG gets.
const TEXT_SLACK: f32 = 1.06;

/// Room to leave for `text`: what it measures, and a little more.
fn text_room(canvas: &mut dyn Canvas, text: &str, size: f32) -> f32 {
    canvas.measure(text, size).x * TEXT_SLACK + 1.0
}

/// Draws `figure` onto `canvas`.
pub fn draw(canvas: &mut dyn Canvas, figure: &Figure, style: &Style) {
    let size = canvas.size();
    canvas.fill_rect(Rect::from_min_size(Pos2::ZERO, size), style.palette.bg);
    if figure.panels.is_empty() {
        return;
    }

    let pad = style.padding;
    let content = Rect::from_min_max(pos2(pad, pad), pos2(size.x - pad, size.y - pad));
    if content.width() < MIN_PLOT_SIDE || content.height() < MIN_PLOT_SIDE {
        return;
    }

    let m = Metrics::new(canvas, style, figure);
    let n = figure.panels.len();

    // --- vertical: the panels stack, and only the bottom one carries the
    // time axis, which is what makes them comparable at a glance.
    let panel_gap = m.gap * 2.0;
    let panel_h = ((content.height() - m.x_axis_block - panel_gap * (n - 1) as f32) / n as f32).max(MIN_PLOT_SIDE);

    // Ticks are chosen for a provisional height (before the legend, which is
    // sized against the plot *width*, which is not known yet). A tick set is
    // valid at any height; only its spacing is tuned, so one pass is enough.
    let provisional_h = panel_h - m.title_block;
    let ticks: Vec<ValueTicks> = figure
        .panels
        .iter()
        .map(|panel| match panel.graph() {
            Some(graph) => ValueTicks {
                left: value_ticks(graph.left, provisional_h, m.line_h * 2.6),
                right: graph.right.map(|r| value_ticks(r, provisional_h, m.line_h * 2.6)),
            },
            // A tank's vertical axis is the ten sensors, which are always the
            // same ten and need no choosing.
            None => ValueTicks {
                left: Ticks {
                    step: 1.0,
                    labels: Vec::new(),
                },
                right: None,
            },
        })
        .collect();

    // --- horizontal: every panel shares one plot rect width, or the stack
    // would be a set of graphs that merely happen to be near each other.
    let mut left_margin: f32 = m.half_time_label;
    let mut right_margin: f32 = m.half_time_label;
    for (panel, t) in figure.panels.iter().zip(&ticks) {
        match &panel.content {
            Content::Graph(graph) => {
                left_margin = left_margin.max(m.axis_margin(canvas, &t.left, &graph.left_label));
                if let (Some(right), true) = (&t.right, graph.has_right()) {
                    right_margin = right_margin.max(m.axis_margin(canvas, right, &graph.right_label));
                }
            }
            Content::Tank(tank) => {
                left_margin = left_margin.max(m.tank_left_margin(canvas));
                right_margin = right_margin.max(m.tank_right_margin(canvas, tank));
            }
        }
    }
    if content.width() - left_margin - right_margin < MIN_PLOT_SIDE {
        // A pane too narrow for its own axis labels: keep the graph, lose the
        // numbers, rather than drawing a rect inside out.
        left_margin = m.gap;
        right_margin = m.gap;
    }
    let plot_left = content.left() + left_margin;
    let plot_right = content.right() - right_margin;
    if plot_right - plot_left < MIN_PLOT_SIDE {
        return;
    }

    let time = time_ticks(figure.range, plot_right - plot_left, m.time_label_w + m.gap * 2.0);

    for (i, (panel, t)) in figure.panels.iter().zip(&ticks).enumerate() {
        let top = content.top() + i as f32 * (panel_h + panel_gap);
        let legend_h = match (style.legend, panel.graph()) {
            (LegendPos::Below, Some(graph)) => legend_below_height(canvas, graph, &m, plot_right - plot_left),
            _ => 0.0,
        };
        let plot_top = top + m.title_block;
        let plot_bottom = (top + panel_h - legend_h).max(plot_top + MIN_PLOT_SIDE);
        let plot = Rect::from_min_max(pos2(plot_left, plot_top), pos2(plot_right, plot_bottom));

        if style.titles && !panel.title.is_empty() {
            canvas.text(
                pos2(plot.left(), top),
                Align2::LEFT_TOP,
                &panel.title,
                m.title_font,
                style.palette.text,
            );
        }

        match &panel.content {
            Content::Graph(graph) => draw_graph(canvas, figure, graph, t, &time, plot, &m),
            Content::Tank(tank) => draw_tank(canvas, figure, tank, plot, &m),
        }

        // After the lines: a legend that went under them would be unreadable
        // exactly where the graph is busiest. A tank has no series legend --
        // the colour bar beside it is the legend.
        if let Some(graph) = panel.graph() {
            match style.legend {
                LegendPos::Off => {}
                LegendPos::TopLeft => draw_legend_inside(canvas, graph, &m, plot, false),
                LegendPos::TopRight => draw_legend_inside(canvas, graph, &m, plot, true),
                // On the bottom panel the time axis is between the graph and
                // the legend, so the legend goes below that rather than over it.
                LegendPos::Below => {
                    let below_axis = if i + 1 == n { m.x_axis_block } else { 0.0 };
                    draw_legend_below(canvas, graph, &m, plot, legend_h, below_axis)
                }
            }
        }

        // The time axis belongs to the stack, not to a panel, so it is drawn
        // once under the last one.
        if i + 1 == n {
            draw_time_axis(canvas, figure, &time, plot, &m);
        }
    }
}

/// The handful of sizes every part of the layout is expressed in, and the
/// style they were worked out from -- the two always travel together.
struct Metrics<'a> {
    style: &'a Style,
    gap: f32,
    tick_len: f32,
    line_h: f32,
    title_font: f32,
    title_block: f32,
    x_axis_block: f32,
    time_label_w: f32,
    half_time_label: f32,
}

impl<'a> Metrics<'a> {
    fn new(canvas: &mut dyn Canvas, style: &'a Style, figure: &Figure) -> Self {
        let font = style.font;
        let gap = (font * 0.5).max(2.0);
        let line_h = canvas.measure("Xg", font).y;
        let title_font = font * 1.1;
        let has_title = style.titles && figure.panels.iter().any(|p| !p.title.is_empty());
        let title_block = if has_title {
            canvas.measure("Xg", title_font).y + gap * 0.8
        } else {
            0.0
        };
        let tick_len = (font * 0.35).max(1.5);
        // Widest a time label gets at this zoom, measured on a real one so
        // "17:41:58.250" reserves more room than "17:41".
        let sample = crate::timeline::format_axis_time(figure.range.0, span_step(figure.range));
        let time_label_w = text_room(canvas, &sample, font);
        Self {
            style,
            gap,
            tick_len,
            line_h,
            title_font,
            title_block,
            // tick marks, tick labels, then the date the clock times belong to
            x_axis_block: tick_len + gap * 0.4 + line_h + gap * 0.3 + line_h,
            time_label_w,
            half_time_label: time_label_w * 0.5,
        }
    }

    /// Room the sensor names down the left of a tank need.
    fn tank_left_margin(&self, canvas: &mut dyn Canvas) -> f32 {
        text_room(canvas, "s0", self.style.font) + self.gap
    }

    /// Room the colour bar and its two labels need on the right of a tank.
    fn tank_right_margin(&self, canvas: &mut dyn Canvas, tank: &Tank) -> f32 {
        let widest = [tank.hi_c, tank.lo_c]
            .iter()
            .map(|c| text_room(canvas, &tank_scale_label(*c), self.style.font))
            .fold(0.0_f32, f32::max);
        self.gap + self.style.font * 1.1 + self.gap * 0.5 + widest
    }

    /// Room a value axis needs: its tick labels, the marks, and the rotated
    /// unit label outside them.
    fn axis_margin(&self, canvas: &mut dyn Canvas, ticks: &Ticks, label: &str) -> f32 {
        let font = self.style.font;
        let widest = ticks
            .labels
            .iter()
            .map(|(_, text)| text_room(canvas, text, font))
            .fold(0.0_f32, f32::max);
        let label_block = if label.is_empty() { 0.0 } else { self.line_h + self.gap * 0.4 };
        self.tick_len + self.gap * 0.4 + widest + self.gap * 0.4 + label_block
    }
}

struct ValueTicks {
    left: Ticks,
    right: Option<Ticks>,
}

/// One axis' worth of ticks: where they go, and what is written at them.
pub struct Ticks {
    pub step: f64,
    pub labels: Vec<(f64, String)>,
}

fn draw_graph(
    canvas: &mut dyn Canvas,
    figure: &Figure,
    graph: &Graph,
    ticks: &ValueTicks,
    time: &Ticks,
    plot: Rect,
    m: &Metrics<'_>,
) {
    let style = m.style;
    let p = &style.palette;
    canvas.fill_rect(plot, p.plot_bg);

    let (t0, t1) = figure.range;
    let x_of = |t: f64| plot.left() + ((t - t0) / (t1 - t0)) as f32 * plot.width();
    let y_of = |v: f64, (lo, hi): (f64, f64)| plot.bottom() - ((v - lo) / (hi - lo)) as f32 * plot.height();

    // --- grid ---
    if style.grid {
        let w = (style.line_width * 0.5).max(0.5);
        for (t, _) in &time.labels {
            let x = x_of(*t);
            canvas.polyline(&[pos2(x, plot.top()), pos2(x, plot.bottom())], p.grid, w);
        }
        for (v, _) in &ticks.left.labels {
            let y = y_of(*v, graph.left);
            canvas.polyline(&[pos2(plot.left(), y), pos2(plot.right(), y)], p.grid, w);
        }
    }

    // --- the lines themselves ---
    canvas.clip(Some(plot));
    for series in &graph.series {
        let range = if series.right_axis {
            graph.right.unwrap_or(graph.left)
        } else {
            graph.left
        };
        // A gap in the data is a gap in the line: a run ends wherever a
        // sample is missing rather than being bridged across it.
        let mut run: Vec<Pos2> = Vec::new();
        for point in &series.points {
            if point[0].is_finite() && point[1].is_finite() {
                run.push(pos2(x_of(point[0]), y_of(point[1], range)));
            } else if !run.is_empty() {
                canvas.polyline(&run, series.color, style.line_width);
                run.clear();
            }
        }
        canvas.polyline(&run, series.color, style.line_width);
    }
    if style.cursor
        && let Some(cursor) = figure.cursor
        && (t0..=t1).contains(&cursor)
    {
        let x = x_of(cursor);
        canvas.polyline(
            &[pos2(x, plot.top()), pos2(x, plot.bottom())],
            p.cursor,
            style.line_width,
        );
    }
    canvas.clip(None);

    canvas.stroke_rect(plot, p.frame, (style.line_width * 0.6).max(0.5));

    // --- value axes ---
    draw_value_axis(canvas, &ticks.left, graph.left, &graph.left_label, plot, m, false);
    if let (Some(right_ticks), Some(range)) = (&ticks.right, graph.right)
        && graph.has_right()
    {
        draw_value_axis(canvas, right_ticks, range, &graph.right_label, plot, m, true);
    }
}

/// The tank wall, drawn against the panel's plot rect so a tank stacked under
/// a graph shares its time axis exactly.
///
/// This is the pane's picture ([`crate::tank`]) at the figure's own size: the
/// same field, the same colour ramp, the same division of the vessel into
/// heights. The vessel's dished ends are drawn *inside* the rect rather than
/// outside it, so the strip lines up with the graphs above it rather than the
/// vessel's outline doing.
fn draw_tank(canvas: &mut dyn Canvas, figure: &Figure, tank: &Tank, plot: Rect, m: &Metrics<'_>) {
    use crate::tank::TANK_SENSORS;

    let style = m.style;
    let p = &style.palette;
    let stroke = (style.line_width * 0.6).max(0.5);
    let cap_h = (plot.width() * 0.06)
        .clamp(m.line_h * 0.4, m.line_h * 1.4)
        .min(plot.height() * 0.2);
    let body = Rect::from_min_max(
        pos2(plot.left(), plot.top() + cap_h),
        pos2(plot.right(), plot.bottom() - cap_h),
    );
    if body.height() < MIN_PLOT_SIDE * 0.5 {
        return;
    }

    // Everything with no reading keeps this, so a dropout is a hole in the
    // picture rather than a temperature nobody measured.
    canvas.fill_rect(body, p.hole);
    canvas.clip(Some(body));
    if tank.blocks {
        draw_tank_blocks(canvas, tank, body);
    } else {
        draw_tank_shading(canvas, tank, body);
    }
    let (t0, t1) = figure.range;
    if style.cursor
        && let Some(cursor) = figure.cursor
        && (t0..=t1).contains(&cursor)
    {
        let x = plot.left() + ((cursor - t0) / (t1 - t0)) as f32 * plot.width();
        canvas.polyline(&[pos2(x, body.top()), pos2(x, body.bottom())], p.cursor, style.line_width);
    }
    canvas.clip(None);

    // The dished ends, which are what make the strip read as a vessel seen
    // from the side rather than as a bare heat map.
    for (edge, dir) in [(body.top(), -1.0_f32), (body.bottom(), 1.0_f32)] {
        const STEPS: usize = 24;
        let (cx, rx) = (body.center().x, body.width() / 2.0);
        let arc: Vec<Pos2> = (0..=STEPS)
            .map(|i| {
                let theta = std::f32::consts::PI * i as f32 / STEPS as f32;
                pos2(cx - rx * theta.cos(), edge + dir * cap_h * theta.sin())
            })
            .collect();
        canvas.polyline(&arc, p.frame, stroke);
    }

    // The pane's own setting, and the figure's: a figure with its grid turned
    // off should not keep one because the pane it came from had one.
    if tank.grid && style.grid {
        // In blocks mode the line belongs on the seam between two sensors'
        // regions, not through the middle of one -- the point of that mode is
        // that a region is all one measurement.
        let lines: Vec<f32> = if tank.blocks {
            (1..TANK_SENSORS)
                .map(|sensor| crate::tank::sensor_band(sensor, body.top(), body.bottom()).1)
                .collect()
        } else {
            (0..TANK_SENSORS).map(|s| crate::tank::sensor_y(s, body)).collect()
        };
        for y in lines {
            canvas.polyline(&[pos2(body.left(), y), pos2(body.right(), y)], p.grid, stroke * 0.8);
        }
    }
    canvas.stroke_rect(body, p.frame, stroke);

    // s9 at the top down to s0 at the bottom, against the heights they stand
    // for -- the tank's value axis, and the reason it needs no ticks.
    for sensor in 0..TANK_SENSORS {
        canvas.text(
            pos2(body.left() - m.gap * 0.4, crate::tank::sensor_y(sensor, body)),
            Align2::RIGHT_CENTER,
            &format!("s{sensor}"),
            style.font,
            p.weak_text,
        );
    }

    draw_tank_scale(canvas, tank, body, plot.right() + m.gap, m);
}

/// The colour bar: the only thing that makes the picture a measurement rather
/// than a pattern, so it is not optional the way a legend is.
fn draw_tank_scale(canvas: &mut dyn Canvas, tank: &Tank, body: Rect, left: f32, m: &Metrics<'_>) {
    let style = m.style;
    let p = &style.palette;
    let bar = Rect::from_min_max(pos2(left, body.top()), pos2(left + style.font * 1.1, body.bottom()));
    if bar.height() <= 0.0 {
        return;
    }
    // Nine stops land on all five of the ramp's own knots, so the bar is the
    // ramp exactly rather than an approximation of it.
    const STOPS: usize = 8;
    let stops: Vec<(f32, Color32)> = (0..=STOPS)
        .map(|i| {
            let f = i as f32 / STOPS as f32;
            let c = tank.hi_c + (tank.lo_c - tank.hi_c) * f as f64;
            (f, crate::tank::heat_colour(c, tank.lo_c, tank.hi_c))
        })
        .collect();
    canvas.vertical_gradient(bar, &stops);
    canvas.stroke_rect(bar, p.frame, (style.line_width * 0.5).max(0.5));

    let x = bar.right() + m.gap * 0.5;
    canvas.text(
        pos2(x, bar.top()),
        Align2::LEFT_TOP,
        &tank_scale_label(tank.hi_c),
        style.font,
        p.text,
    );
    canvas.text(
        pos2(x, bar.bottom()),
        Align2::LEFT_BOTTOM,
        &tank_scale_label(tank.lo_c),
        style.font,
        p.text,
    );

    // The critical point: the one temperature on this scale that is a
    // physical boundary rather than a preference.
    if (tank.lo_c..=tank.hi_c).contains(&crate::tank::CRITICAL_C) {
        let f = ((crate::tank::CRITICAL_C - tank.lo_c) / (tank.hi_c - tank.lo_c)) as f32;
        let y = bar.bottom() - bar.height() * f;
        canvas.polyline(
            &[pos2(bar.left() - m.gap * 0.3, y), pos2(bar.right() + m.gap * 0.3, y)],
            p.text,
            (style.line_width * 0.8).max(0.5),
        );
        // ... but not its name when that would land on top of one of the
        // bar's two end labels, which are the numbers it is read against.
        // Half of "Tc" plus a whole end label: any closer and the two would
        // be printed over each other.
        if (y - bar.top()).min(bar.bottom() - y) > m.line_h * 1.5 {
            canvas.text(pos2(x, y), Align2::LEFT_CENTER, "Tc", style.font, p.text);
        }
    }
}

fn tank_scale_label(c: f64) -> String {
    format!("{c:.0} °C")
}

/// The field with no interpolation: one flat rectangle per sensor per moment,
/// with columns that came out the same colour merged into one rectangle --
/// which is most of them, and the difference between a readable SVG and one
/// with ten thousand rectangles in it.
fn draw_tank_blocks(canvas: &mut dyn Canvas, tank: &Tank, body: Rect) {
    use crate::tank::TANK_SENSORS;
    let field = &tank.field;
    for sensor in 0..TANK_SENSORS {
        let (top, bottom) = crate::tank::sensor_band(sensor, body.top(), body.bottom());
        let mut run: Option<(f32, f32, Color32)> = None;
        for col in 0..field.cols {
            let (x0, x1) = crate::tank::column_cell(col, field.cols, body.left(), body.right());
            let c = field.at(col, sensor);
            let colour = (!c.is_nan()).then(|| crate::tank::heat_colour(c, tank.lo_c, tank.hi_c));
            match (run, colour) {
                (Some((start, end, held)), Some(colour)) if held == colour && (end - x0).abs() < 0.01 => {
                    run = Some((start, x1, held));
                }
                (previous, colour) => {
                    if let Some((start, end, held)) = previous {
                        canvas.fill_cell(Rect::from_min_max(pos2(start, top), pos2(end, bottom)), held);
                    }
                    run = colour.map(|colour| (x0, x1, colour));
                }
            }
        }
        if let Some((start, end, held)) = run {
            canvas.fill_cell(Rect::from_min_max(pos2(start, top), pos2(end, bottom)), held);
        }
    }
}

/// The field shaded between neighbours, as one gradient per column.
///
/// The pane does this with a mesh the GPU interpolates; here each column is a
/// rectangle with a colour stop at every sensor, which is the same
/// interpolation and stays one in an SVG. A column is broken wherever a sensor
/// said nothing, so a dropout ends the shading at the column it happened in.
fn draw_tank_shading(canvas: &mut dyn Canvas, tank: &Tank, body: Rect) {
    use crate::tank::TANK_SENSORS;
    let field = &tank.field;
    let mut stops: Vec<(f32, Color32)> = Vec::with_capacity(TANK_SENSORS);
    for col in 0..field.cols {
        let (x0, x1) = crate::tank::column_cell(col, field.cols, body.left(), body.right());
        let mut run_start: Option<usize> = None;
        for sensor in 0..=TANK_SENSORS {
            let real = sensor < TANK_SENSORS && !field.at(col, sensor).is_nan();
            match (real, run_start) {
                (true, None) => run_start = Some(sensor),
                (false, Some(start)) => {
                    let end = sensor - 1;
                    // A lone sensor with holes either side shades against
                    // nothing; the pane leaves it blank too.
                    if end > start {
                        stops.clear();
                        for s in (start..=end).rev() {
                            let f = (end - s) as f32 / (end - start) as f32;
                            stops.push((f, crate::tank::heat_colour(field.at(col, s), tank.lo_c, tank.hi_c)));
                        }
                        let rect = Rect::from_min_max(
                            pos2(x0, crate::tank::sensor_y(end, body)),
                            pos2(x1, crate::tank::sensor_y(start, body)),
                        );
                        canvas.vertical_gradient(rect, &stops);
                    }
                    run_start = None;
                }
                _ => {}
            }
        }
    }
}

fn draw_value_axis(
    canvas: &mut dyn Canvas,
    ticks: &Ticks,
    range: (f64, f64),
    label: &str,
    plot: Rect,
    m: &Metrics<'_>,
    right: bool,
) {
    let style = m.style;
    let p = &style.palette;
    let stroke = (style.line_width * 0.6).max(0.5);
    let edge = if right { plot.right() } else { plot.left() };
    let dir = if right { 1.0 } else { -1.0 };
    let mut widest = 0.0_f32;

    for (value, text) in &ticks.labels {
        let y = plot.bottom() - ((value - range.0) / (range.1 - range.0)) as f32 * plot.height();
        canvas.polyline(&[pos2(edge, y), pos2(edge + dir * m.tick_len, y)], p.frame, stroke);
        let align = if right { Align2::LEFT_CENTER } else { Align2::RIGHT_CENTER };
        canvas.text(
            pos2(edge + dir * (m.tick_len + m.gap * 0.4), y),
            align,
            text,
            style.font,
            p.text,
        );
        widest = widest.max(text_room(canvas, text, style.font));
    }

    if !label.is_empty() {
        let x = edge + dir * (m.tick_len + m.gap * 0.8 + widest + m.line_h * 0.5);
        canvas.text_rotated(pos2(x, plot.center().y), label, style.font, p.weak_text);
    }
}

fn draw_time_axis(canvas: &mut dyn Canvas, figure: &Figure, time: &Ticks, plot: Rect, m: &Metrics<'_>) {
    let style = m.style;
    let p = &style.palette;
    let stroke = (style.line_width * 0.6).max(0.5);
    let (t0, t1) = figure.range;
    for (t, text) in &time.labels {
        let x = plot.left() + ((t - t0) / (t1 - t0)) as f32 * plot.width();
        canvas.polyline(
            &[pos2(x, plot.bottom()), pos2(x, plot.bottom() + m.tick_len)],
            p.frame,
            stroke,
        );
        canvas.text(
            pos2(x, plot.bottom() + m.tick_len + m.gap * 0.4),
            Align2::CENTER_TOP,
            text,
            style.font,
            p.text,
        );
    }
    // The tick labels are clock times; without the date they name an instant
    // that comes round once a day.
    canvas.text(
        pos2(plot.center().x, plot.bottom() + m.tick_len + m.gap * 0.7 + m.line_h),
        Align2::CENTER_TOP,
        &date_label(figure.range),
        style.font,
        p.weak_text,
    );
}

/// `"2026-08-08 (UTC)"`, or both dates when the window crosses midnight.
pub fn date_label((t0, t1): (f64, f64)) -> String {
    let day = |t: f64| {
        chrono::DateTime::from_timestamp(t.floor() as i64, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    };
    let (a, b) = (day(t0), day(t1));
    if a.is_empty() {
        String::new()
    } else if a == b {
        format!("{a} (UTC)")
    } else {
        format!("{a} to {b} (UTC)")
    }
}

/// One entry's width in a legend: colour sample, gap, label.
fn legend_entry_width(canvas: &mut dyn Canvas, label: &str, m: &Metrics<'_>) -> f32 {
    m.style.font * 1.6 + m.gap * 0.5 + text_room(canvas, label, m.style.font)
}

fn legend_below_height(canvas: &mut dyn Canvas, graph: &Graph, m: &Metrics<'_>, width: f32) -> f32 {
    if graph.series.is_empty() {
        return 0.0;
    }
    let rows = legend_rows(canvas, graph, m, width).len();
    m.gap * 0.6 + rows as f32 * m.line_h
}

/// Legend entries wrapped into rows no wider than `width`.
fn legend_rows(canvas: &mut dyn Canvas, graph: &Graph, m: &Metrics<'_>, width: f32) -> Vec<Vec<usize>> {
    let spacing = m.gap * 1.5;
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut row: Vec<usize> = Vec::new();
    let mut x = 0.0;
    for (i, series) in graph.series.iter().enumerate() {
        let w = legend_entry_width(canvas, &series.label, m);
        if !row.is_empty() && x + w > width {
            rows.push(std::mem::take(&mut row));
            x = 0.0;
        }
        x += w + spacing;
        row.push(i);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn draw_legend_below(
    canvas: &mut dyn Canvas,
    graph: &Graph,
    m: &Metrics<'_>,
    plot: Rect,
    height: f32,
    below_axis: f32,
) {
    if height <= 0.0 {
        return;
    }
    let rows = legend_rows(canvas, graph, m, plot.width());
    let mut y = plot.bottom() + below_axis + m.gap * 0.6;
    for row in rows {
        let mut x = plot.left();
        for i in row {
            let series = &graph.series[i];
            draw_legend_entry(canvas, series, pos2(x, y), m);
            x += legend_entry_width(canvas, &series.label, m) + m.gap * 1.5;
        }
        y += m.line_h;
    }
}

fn draw_legend_entry(canvas: &mut dyn Canvas, series: &Series, top_left: Pos2, m: &Metrics<'_>) {
    let style = m.style;
    let mid = top_left.y + m.line_h * 0.5;
    canvas.polyline(
        &[pos2(top_left.x, mid), pos2(top_left.x + style.font * 1.6, mid)],
        series.color,
        style.line_width,
    );
    canvas.text(
        pos2(top_left.x + style.font * 1.6 + m.gap * 0.5, mid),
        Align2::LEFT_CENTER,
        &series.label,
        style.font,
        style.palette.text,
    );
}

/// The legend as a box inside the plot area, which is where it costs no room.
fn draw_legend_inside(canvas: &mut dyn Canvas, graph: &Graph, m: &Metrics<'_>, plot: Rect, right: bool) {
    let style = m.style;
    if graph.series.is_empty() {
        return;
    }
    let widest = graph
        .series
        .iter()
        .map(|s| legend_entry_width(canvas, &s.label, m))
        .fold(0.0_f32, f32::max);
    let inner = m.gap * 0.5;
    let size = vec2(widest + inner * 2.0, graph.series.len() as f32 * m.line_h + inner * 2.0);
    if size.x > plot.width() || size.y > plot.height() {
        // A legend bigger than the graph would hide the data it names.
        return;
    }
    let x = if right {
        plot.right() - m.gap - size.x
    } else {
        plot.left() + m.gap
    };
    let rect = Rect::from_min_size(pos2(x, plot.top() + m.gap), size);
    canvas.fill_rect(rect, style.palette.plot_bg.gamma_multiply(0.85));
    canvas.stroke_rect(rect, style.palette.grid, (style.line_width * 0.5).max(0.5));
    for (i, series) in graph.series.iter().enumerate() {
        draw_legend_entry(
            canvas,
            series,
            pos2(rect.left() + inner, rect.top() + inner + i as f32 * m.line_h),
            m,
        );
    }
}

/// A first guess at the time-tick spacing, before the plot width is known --
/// only used to pick a representative label to measure.
fn span_step((t0, t1): (f64, f64)) -> f64 {
    ((t1 - t0) / 6.0).max(f64::MIN_POSITIVE)
}

/// Ticks for the time axis, on the same clock ladder the app's graphs use, so
/// an exported figure is tick-for-tick the graph it was exported from.
pub fn time_ticks((t0, t1): (f64, f64), width: f32, min_spacing: f32) -> Ticks {
    let empty = Ticks {
        step: 1.0,
        labels: Vec::new(),
    };
    if !super::is_drawable_range((t0, t1)) || width <= 0.0 {
        return empty;
    }
    let wanted = (width / min_spacing.max(1.0)).floor().max(1.0) as f64;
    let base = (t1 - t0) / wanted;
    let step = crate::timeline::TIME_STEPS
        .iter()
        .copied()
        .find(|s| *s >= base)
        .unwrap_or_else(|| (base / 86400.0).ceil() * 86400.0);
    Ticks {
        step,
        labels: multiples_of(step, (t0, t1))
            .map(|t| (t, crate::timeline::format_axis_time(t, step)))
            .collect(),
    }
}

/// Ticks for a value axis: the 1/2/5 ladder, at whatever precision the spacing
/// resolves.
pub fn value_ticks((lo, hi): (f64, f64), height: f32, min_spacing: f32) -> Ticks {
    let empty = Ticks {
        step: 1.0,
        labels: Vec::new(),
    };
    if !super::is_drawable_range((lo, hi)) || height <= 0.0 {
        return empty;
    }
    let wanted = (height / min_spacing.max(1.0)).floor().max(1.0) as f64;
    let step = nice_step((hi - lo) / wanted);
    if !step.is_finite() || step <= 0.0 {
        return empty;
    }
    Ticks {
        step,
        labels: multiples_of(step, (lo, hi))
            .map(|v| {
                // -0 reads as a different number from 0.
                let v = if v == 0.0 { 0.0 } else { v };
                (v, crate::panes::format_axis_value(v, step))
            })
            .collect(),
    }
}

/// Every multiple of `step` inside `range`, capped so a degenerate step can't
/// spin here forever.
fn multiples_of(step: f64, (lo, hi): (f64, f64)) -> impl Iterator<Item = f64> {
    const MAX_TICKS: usize = 500;
    let first = (lo / step).ceil();
    let count = (((hi / step).floor() - first + 1.0).max(0.0) as usize).min(MAX_TICKS);
    (0..count).map(move |i| (first + i as f64) * step)
}

/// The 1/2/5 × 10ⁿ step at or above `raw`.
fn nice_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(raw.log10().floor());
    let normalized = raw / magnitude;
    let factor = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    factor * magnitude
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_ticks_land_on_round_numbers() {
        let ticks = value_ticks((0.0, 10.0), 300.0, 30.0);
        assert_eq!(ticks.step, 1.0);
        assert_eq!(ticks.labels.first().map(|t| t.0), Some(0.0));
        assert_eq!(ticks.labels.last().map(|t| t.0), Some(10.0));
        assert!(ticks.labels.iter().all(|(v, _)| (v.round() - v).abs() < 1e-9));
    }

    #[test]
    fn value_ticks_thin_out_as_the_axis_gets_shorter() {
        let tall = value_ticks((0.0, 100.0), 600.0, 30.0).labels.len();
        let short = value_ticks((0.0, 100.0), 90.0, 30.0).labels.len();
        assert!(tall > short, "{tall} vs {short}");
        assert!(short >= 2, "an axis always needs a couple of numbers, got {short}");
    }

    /// Every one of these reaches a division or a loop bound downstream.
    #[test]
    fn degenerate_ranges_produce_no_ticks_rather_than_a_hang() {
        assert!(value_ticks((5.0, 5.0), 300.0, 30.0).labels.is_empty());
        assert!(value_ticks((f64::NAN, 1.0), 300.0, 30.0).labels.is_empty());
        assert!(value_ticks((0.0, f64::INFINITY), 300.0, 30.0).labels.is_empty());
        assert!(value_ticks((0.0, 1.0), 0.0, 30.0).labels.is_empty());
        assert!(time_ticks((10.0, 0.0), 300.0, 60.0).labels.is_empty());
        assert!(time_ticks((0.0, 1e300), 300.0, 60.0).labels.len() <= 500);
    }

    #[test]
    fn time_ticks_sit_on_the_clock() {
        // A five-minute window: ticks every 30 s, on the half minute.
        let ticks = time_ticks((1_786_000_000.0, 1_786_000_300.0), 600.0, 60.0);
        assert!(ticks.labels.len() >= 4, "{:?}", ticks.labels.len());
        assert!(
            ticks.labels.iter().all(|(t, _)| (t / ticks.step).fract().abs() < 1e-6),
            "every tick is a whole number of steps"
        );
        assert!(ticks.labels.iter().all(|(_, label)| label.contains(':')));
    }

    #[test]
    fn a_nice_step_is_a_one_two_or_five() {
        for raw in [0.0012, 0.03, 0.4, 3.0, 7.0, 12.0, 900.0] {
            let step = nice_step(raw);
            assert!(step >= raw, "{step} < {raw}");
            let normalized = step / 10f64.powf(step.log10().floor());
            assert!(
                (normalized - 1.0).abs() < 1e-9 || (normalized - 2.0).abs() < 1e-9 || (normalized - 5.0).abs() < 1e-9,
                "{step} normalizes to {normalized}"
            );
        }
    }

    #[test]
    fn the_date_says_when_the_window_crossed_midnight() {
        let one_day = 86_400.0;
        assert_eq!(date_label((0.0, 3600.0)), "1970-01-01 (UTC)");
        assert_eq!(date_label((0.0, one_day + 10.0)), "1970-01-01 to 1970-01-02 (UTC)");
    }
}
