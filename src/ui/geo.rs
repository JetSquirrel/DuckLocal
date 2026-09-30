//! Map mode of the chart tab: a result with a latitude and a longitude column
//! plots each row as a point on a Web Mercator projection, fitted to the
//! points' extent, over a graticule labelled in degrees — and OpenStreetMap
//! tiles, once someone turns them on (`tiles`).
//!
//! Without tiles the points draw the shape on their own; an embedded
//! coastline would say nothing at the city scale most station, store or
//! sensor tables live at. When a low
//! cardinality text column is present (a `type`, a `country`), points are
//! colored by it, so the map says something beyond "where"; a dashboard can
//! also size them by a number (`MapStyle`), so it says "how much" too.
//!
//! Like the other charts, detection and projection happen once per result set
//! (`GeoData::detect`); a frame re-derives only the viewport transform, which
//! is a handful of multiplications, and pushes one quad per point.

use std::collections::HashMap;
use std::f64::consts::FRAC_PI_4;
use std::sync::Arc;

use gpui_kit::component::plot::label::{Text, TEXT_SIZE};
use gpui_kit::component::plot::tooltip::{Dot, Tooltip, TooltipState};
use gpui_kit::component::plot::{Grid, Plot, PlotElement, PlotLabel};
use gpui_kit::component::ActiveTheme;
use gpui_kit::*;

use crate::i18n::{tr, trf};
use crate::query::{ColumnKind, QueryResult};
use crate::ui::chart::parse_number;

/// Points are single quads, so the renderer copes with far more of them than
/// with path vertices; past this a map is a solid blot anyway.
pub(crate) const MAX_GEO_POINTS: usize = 20_000;
/// The palette holds five colors: more categories than that fold the rarest
/// into "other".
const MAX_CATEGORIES: usize = 5;
/// A text column with more distinct values than this is an identifier or a
/// name, not a category.
const MAX_CATEGORY_CARDINALITY: usize = 24;
/// Web Mercator is undefined at the poles; this is where the standard cuts it.
const MAX_MERCATOR_LAT: f64 = 85.051_128_78;
/// The narrowest extent a map zooms to, in degrees, so a single point or a
/// single street does not fill the plot at an absurd scale.
const MIN_SPAN: f64 = 0.01;
const DOT_SIZE: f32 = 9.;
/// A sized point's diameter at the largest value, and the least any point
/// gets — below this a dot stops being findable, let alone hoverable.
const MAX_SIZED: f32 = 40.;
const MIN_SIZED: f32 = 5.;
/// Tooltip rows of measures a map shows unasked.
const DEFAULT_MEASURES: usize = 4;
const HOVER_DOT_SIZE: f32 = 12.;
const HOVER_HALO: f32 = 20.;
/// How close, in pixels, the cursor must be to a point to hover it.
const HIT_RADIUS: f32 = 14.;
/// The gutters the degree labels sit in, left of and under the plot area.
const LEFT_GUTTER: f32 = 52.;
const BOTTOM_GUTTER: f32 = 22.;
const EDGE_PAD: f32 = 8.;
/// Roughly how far apart graticule lines are, in pixels: a fixed count would
/// leave a wide plot with three meridians across it.
const LINE_SPACING: f64 = 90.;

#[derive(Debug)]
pub(crate) struct GeoPoint {
    lat: f64,
    lng: f64,
    /// Projected position: `x` is the longitude, `y` the Mercator ordinate,
    /// both in degrees, so the viewport maps them with one uniform scale.
    x: f64,
    y: f64,
    label: SharedString,
    category: Option<usize>,
    /// The point's diameter in pixels.
    diameter: f32,
    /// The cells of `GeoData::measures`, in order, as the query returned
    /// them: a tooltip shows the number, not a rounding of it.
    measures: Vec<SharedString>,
}

/// How a sized map turns a value into a point's area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SizeScale {
    /// Area in proportion to the value: what a reader's eye compares.
    #[default]
    Sqrt,
    /// By order of magnitude, for values spanning several: one heavy row
    /// would otherwise shrink every other point to the minimum.
    Log,
}

impl SizeScale {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "sqrt" => Some(Self::Sqrt),
            "log" => Some(Self::Log),
            _ => None,
        }
    }

    /// The diameter for `value` on a map whose usable values run
    /// `min..=max`, or `None` when the value cannot be placed on the scale.
    fn diameter(self, value: f64, min: f64, max: f64) -> Option<f32> {
        match self {
            Self::Sqrt if value >= 0. && max > 0. => {
                Some((MAX_SIZED * (value / max).sqrt() as f32).max(MIN_SIZED))
            }
            Self::Log if value > 0. => {
                let t = if max > min {
                    (value.ln() - min.ln()) / (max.ln() - min.ln())
                } else {
                    1.
                };
                Some(MIN_SIZED + (MAX_SIZED - MIN_SIZED) * t as f32)
            }
            _ => None,
        }
    }

    fn usable(self, value: f64) -> bool {
        match self {
            Self::Sqrt => value >= 0.,
            Self::Log => value > 0.,
        }
    }
}

/// What a dashboard's `map` plot asks of the points beyond placing them.
#[derive(Clone, Debug, Default)]
pub struct MapStyle {
    /// The column to color by; `None` picks one.
    pub color: Option<usize>,
    /// The numeric column to size by, and how.
    pub size: Option<(usize, SizeScale)>,
    /// The columns the tooltip lists, in order; `None` lists the size column
    /// and the other numbers, then the coordinates.
    pub tooltip: Option<Vec<usize>>,
}

/// The size key's reference values and the scale they sit on.
#[derive(Debug)]
struct SizeKey {
    scale: SizeScale,
    min: f64,
    max: f64,
}

/// Everything the map needs, derived from a result set once.
#[derive(Debug)]
pub struct GeoData {
    pub lat_name: String,
    pub lng_name: String,
    label_name: Option<String>,
    pub category_name: Option<String>,
    /// Legend entries, in palette order. When categories were folded the last
    /// one is "other", drawn in the muted color.
    pub categories: Vec<SharedString>,
    pub folded_other: bool,
    points: Arc<Vec<GeoPoint>>,
    /// Projected extent: `(min_x, max_x, min_y, max_y)`.
    extent: (f64, f64, f64, f64),
    /// Rows whose coordinates were missing or out of range.
    pub dropped: usize,
    /// How many rows had valid coordinates, when more than were plotted.
    pub capped_from: Option<usize>,
    /// The column points are sized by.
    pub size_name: Option<String>,
    size_key: Option<SizeKey>,
    /// Points whose size value was missing, or off its scale (negative, or
    /// not positive on a log scale): drawn at the smallest size.
    pub size_missing: usize,
    /// The tooltip's measure rows: column names, matching each point's
    /// `measures`.
    measures: Vec<String>,
    /// Whether the tooltip ends with the coordinates.
    show_coordinates: bool,
}

impl GeoData {
    /// The map for `result`, or `None` when it has no latitude/longitude
    /// column pair, or when fewer than half its rows hold valid coordinates in
    /// them — then they are something else that happens to share the name.
    pub fn detect(result: &QueryResult) -> Option<Self> {
        let numeric = |ix: &usize| result.columns[*ix].kind == ColumnKind::Numeric;
        let lat_ix = (0..result.columns.len())
            .filter(numeric)
            .find(|&ix| is_lat_name(&result.columns[ix].name))?;
        let lng_ix = (0..result.columns.len())
            .filter(numeric)
            .find(|&ix| ix != lat_ix && is_lng_name(&result.columns[ix].name))?;
        let valid = valid_coordinates(result, lat_ix, lng_ix);
        if valid.is_empty() || valid.len() * 2 < result.rows.len() {
            return None;
        }
        Some(Self::build(result, lat_ix, lng_ix, valid, &MapStyle::default()))
    }

    /// The map a dashboard's `map` plot names outright: its latitude and
    /// longitude columns, and the `style` its other attributes ask for.
    /// Rows without valid coordinates are dropped and counted; nothing is
    /// second-guessed, so an empty map says the columns held no coordinates
    /// rather than falling back to something else.
    pub fn from_columns(
        result: &QueryResult,
        lat_ix: usize,
        lng_ix: usize,
        style: &MapStyle,
    ) -> Self {
        let valid = valid_coordinates(result, lat_ix, lng_ix);
        Self::build(result, lat_ix, lng_ix, valid, style)
    }

    fn build(
        result: &QueryResult,
        lat_ix: usize,
        lng_ix: usize,
        valid: Vec<(usize, f64, f64)>,
        style: &MapStyle,
    ) -> Self {
        let dropped = result.rows.len() - valid.len();
        let valid_count = valid.len();
        let capped_from = (valid_count > MAX_GEO_POINTS).then_some(valid_count);

        let label_ix = label_column(result, &[lat_ix, lng_ix]);
        let category = match style.color {
            Some(column) => category_of(result, &valid, column),
            None => category_column(result, &valid, label_ix),
        };

        // The scale's ends come from the values that can sit on it.
        let size_key = style.size.and_then(|(column, scale)| {
            let (min, max) = valid
                .iter()
                .filter_map(|(ix, _, _)| parse_number(result.rows[*ix].get(column)?))
                .filter(|value| scale.usable(*value))
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(v), hi.max(v))
                });
            (min <= max).then_some((column, SizeKey { scale, min, max }))
        });
        let mut size_missing = 0;

        let measure_ixs: Vec<usize> = match &style.tooltip {
            Some(columns) => columns.clone(),
            None => {
                let size_ix = style.size.map(|(column, _)| column);
                let category_ix = category.as_ref().map(|c| c.column);
                let others = (0..result.columns.len()).filter(|&ix| {
                    result.columns[ix].kind == ColumnKind::Numeric
                        && ![Some(lat_ix), Some(lng_ix), size_ix, category_ix, label_ix]
                            .contains(&Some(ix))
                });
                size_ix.into_iter().chain(others).take(DEFAULT_MEASURES).collect()
            }
        };

        let mut points: Vec<GeoPoint> = valid
            .into_iter()
            .take(MAX_GEO_POINTS)
            .map(|(ix, lat, lng)| {
                let row = &result.rows[ix];
                let label = match label_ix.and_then(|l| row.get(l)) {
                    Some(text) => text.clone().into(),
                    None => trf("chart.map.row", &[&(ix + 1).to_string()]).into(),
                };
                let category = category
                    .as_ref()
                    .and_then(|c| row.get(c.column).map(|v| c.slot(v)));
                let diameter = match (&size_key, style.size) {
                    (Some((column, key)), _) => row
                        .get(*column)
                        .and_then(|cell| parse_number(cell))
                        .and_then(|value| key.scale.diameter(value, key.min, key.max))
                        .unwrap_or_else(|| {
                            size_missing += 1;
                            MIN_SIZED
                        }),
                    // Asked for, but no row had a usable value.
                    (None, Some(_)) => {
                        size_missing += 1;
                        MIN_SIZED
                    }
                    (None, None) => DOT_SIZE,
                };
                let measures = measure_ixs
                    .iter()
                    .map(|ix| row.get(*ix).cloned().unwrap_or_default().into())
                    .collect();
                GeoPoint {
                    lat,
                    lng,
                    x: lng,
                    y: mercator_y(lat),
                    label,
                    category,
                    diameter,
                    measures,
                }
            })
            .collect();
        // Largest first, so a small point is never buried under a big one
        // and stays reachable by the pointer.
        if style.size.is_some() {
            points.sort_by(|a, b| b.diameter.total_cmp(&a.diameter));
        }

        let extent = points.iter().fold(
            (
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ),
            |(x0, x1, y0, y1), p| (x0.min(p.x), x1.max(p.x), y0.min(p.y), y1.max(p.y)),
        );

        let (category_name, categories, folded_other) = match category {
            Some(c) => (
                Some(result.columns[c.column].name.clone()),
                c.legend,
                c.folded,
            ),
            None => (None, Vec::new(), false),
        };

        Self {
            lat_name: result.columns[lat_ix].name.clone(),
            lng_name: result.columns[lng_ix].name.clone(),
            label_name: label_ix.map(|ix| result.columns[ix].name.clone()),
            category_name,
            categories,
            folded_other,
            points: Arc::new(points),
            extent,
            dropped,
            capped_from,
            size_name: style.size.map(|(column, _)| result.columns[column].name.clone()),
            size_key: size_key.map(|(_, key)| key),
            size_missing,
            measures: measure_ixs
                .iter()
                .map(|ix| result.columns[*ix].name.clone())
                .collect(),
            show_coordinates: style.tooltip.is_none(),
        }
    }

    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// The color of legend entry `slot`: palette order, with a folded
    /// "other" in the muted color.
    pub fn category_color(&self, slot: usize, cx: &App) -> Hsla {
        category_color(
            slot,
            self.folded_other && slot + 1 == self.categories.len(),
            cx,
        )
    }
}

/// `(row index, latitude, longitude)` for every row whose two cells parse as
/// coordinates in range.
fn valid_coordinates(result: &QueryResult, lat_ix: usize, lng_ix: usize) -> Vec<(usize, f64, f64)> {
    result
        .rows
        .iter()
        .enumerate()
        .filter_map(|(ix, row)| {
            let lat = parse_number(row.get(lat_ix)?)?;
            let lng = parse_number(row.get(lng_ix)?)?;
            ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng))
                .then_some((ix, lat, lng))
        })
        .collect()
}

/// The latitude and longitude columns names alone suggest, as `detect` finds
/// them but without the values to check: what `ducklocal check` can say about
/// a `map` plot that leaves `lat` or `lng` out.
pub(crate) fn guess_coordinate_columns<'a>(
    names: impl Iterator<Item = &'a str> + Clone,
) -> (Option<usize>, Option<usize>) {
    let lat = names.clone().position(is_lat_name);
    let lng = names
        .enumerate()
        .find(|(ix, name)| Some(*ix) != lat && is_lng_name(name))
        .map(|(ix, _)| ix);
    (lat, lng)
}

fn category_color(slot: usize, is_other: bool, cx: &App) -> Hsla {
    let theme = cx.theme();
    if is_other {
        return theme.muted_foreground;
    }
    [
        // Distinct hues, not the `chart_*` ramp: those are shades of one
        // blue, fine for ordered series, indistinguishable as categories on
        // a map.
        theme.blue,
        theme.red,
        theme.green,
        theme.magenta,
        theme.yellow,
    ][slot % MAX_CATEGORIES]
}

/// The words of a column name, lowercased: split at anything not a letter or
/// digit, and at a lower-to-upper case change, so `geo_lat`, `geo-lat` and
/// `geoLat` all hold the word `lat`.
fn name_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        prev_lower = c.is_lowercase();
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

pub(crate) fn is_lat_name(name: &str) -> bool {
    name_words(name)
        .iter()
        .any(|w| matches!(w.as_str(), "lat" | "latitude" | "纬度"))
}

pub(crate) fn is_lng_name(name: &str) -> bool {
    name_words(name)
        .iter()
        .any(|w| matches!(w.as_str(), "lng" | "lon" | "long" | "longitude" | "经度"))
}

/// The column a point's tooltip is titled with: a name-like text column if
/// there is one, else the first text column.
fn label_column(result: &QueryResult, exclude: &[usize]) -> Option<usize> {
    result
        .columns
        .iter()
        .enumerate()
        .filter(|(ix, c)| c.kind == ColumnKind::Text && !exclude.contains(ix))
        .min_by_key(|(ix, c)| {
            let words = name_words(&c.name);
            let has = |w: &str| words.iter().any(|x| x == w);
            let rank = if words == ["name"] {
                0
            } else if has("name") && (has("long") || has("full")) {
                1
            } else if has("name") || has("名称") {
                2
            } else if has("title") || has("label") {
                3
            } else {
                4
            };
            (rank, *ix)
        })
        .map(|(ix, _)| ix)
}

struct Category {
    column: usize,
    /// Value → legend slot.
    slots: HashMap<String, usize>,
    legend: Vec<SharedString>,
    folded: bool,
}

impl Category {
    fn slot(&self, value: &str) -> usize {
        match self.slots.get(value) {
            Some(&slot) => slot,
            // Only a folded legend leaves values without a slot of their own.
            None => self.legend.len() - 1,
        }
    }
}

/// The text column points are colored by: the one with the fewest distinct
/// values, as long as it has at least two and values repeat. A column where
/// every row differs is an identifier; coloring by it would be noise.
fn category_column(
    result: &QueryResult,
    valid: &[(usize, f64, f64)],
    label_ix: Option<usize>,
) -> Option<Category> {
    let mut best: Option<(usize, Vec<(String, usize)>)> = None;
    for (ix, column) in result.columns.iter().enumerate() {
        if !matches!(column.kind, ColumnKind::Text | ColumnKind::Boolean) || Some(ix) == label_ix {
            continue;
        }
        let mut counts: HashMap<&str, usize> = HashMap::new();
        let mut too_many = false;
        for &(row_ix, _, _) in valid {
            let Some(value) = result.rows[row_ix].get(ix) else {
                continue;
            };
            *counts.entry(value.as_str()).or_default() += 1;
            if counts.len() > MAX_CATEGORY_CARDINALITY {
                too_many = true;
                break;
            }
        }
        if too_many || counts.len() < 2 || counts.len() * 2 > valid.len() {
            continue;
        }
        if best.as_ref().is_some_and(|(_, b)| b.len() <= counts.len()) {
            continue;
        }
        let mut sorted: Vec<(String, usize)> = counts
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        // Most common first; ties by value, so the legend is stable.
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        best = Some((ix, sorted));
    }

    let (column, sorted) = best?;
    Some(fold_category(column, sorted))
}

/// Color by `column`, as a `color` attribute asks: every value it holds, the
/// most common first, the rest folded into "other" past the palette.
fn category_of(result: &QueryResult, valid: &[(usize, f64, f64)], column: usize) -> Option<Category> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for &(row_ix, _, _) in valid {
        if let Some(value) = result.rows[row_ix].get(column) {
            *counts.entry(value.as_str()).or_default() += 1;
        }
    }
    if counts.is_empty() {
        return None;
    }
    let mut sorted: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Some(fold_category(column, sorted))
}

/// Legend slots for `sorted` values (most common first): one each up to the
/// palette's size, or all but the last slot plus "other" past it.
fn fold_category(column: usize, sorted: Vec<(String, usize)>) -> Category {
    let folded = sorted.len() > MAX_CATEGORIES;
    let kept = if folded {
        MAX_CATEGORIES - 1
    } else {
        sorted.len()
    };
    let slots = sorted
        .iter()
        .take(kept)
        .enumerate()
        .map(|(slot, (value, _))| (value.clone(), slot))
        .collect();
    let mut legend: Vec<SharedString> = sorted
        .into_iter()
        .take(kept)
        .map(|(value, _)| value.into())
        .collect();
    if folded {
        legend.push(tr("chart.map.other").into());
    }
    Category {
        column,
        slots,
        legend,
        folded,
    }
}

/// A latitude's projected ordinate, for the tile layer's tests.
#[cfg(test)]
pub(crate) fn project_lat(lat: f64) -> f64 {
    mercator_y(lat)
}

/// Most tiles a map asks for at once; a bigger plot drops a zoom level
/// rather than filling the policy's queue with one view.
const MAX_TILES: usize = 48;

fn mercator_y(lat: f64) -> f64 {
    let phi = lat.clamp(-MAX_MERCATOR_LAT, MAX_MERCATOR_LAT).to_radians();
    (FRAC_PI_4 + phi / 2.).tan().ln().to_degrees()
}

fn inverse_mercator_y(y: f64) -> f64 {
    (2. * y.to_radians().exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees()
}

/// A round graticule step for `span` degrees across `pixels`, near one line
/// per `LINE_SPACING`: 1, 2 or 5 times a power of ten.
fn nice_step(span: f64, pixels: f64) -> f64 {
    let lines = (pixels / LINE_SPACING).max(2.);
    let raw = (span / lines).max(1e-9);
    let magnitude = 10f64.powf(raw.log10().floor());
    let step = [1., 2., 5., 10.]
        .into_iter()
        .map(|m| m * magnitude)
        .find(|step| *step >= raw)
        .unwrap_or(10. * magnitude);
    // A degree grid past 30° reads better at the familiar 30°.
    step.min(30.)
}

/// The multiples of `step` within `[min, max]`.
fn steps_within(min: f64, max: f64, step: f64) -> impl Iterator<Item = f64> {
    let first = (min / step).ceil() as i64;
    let last = (max / step).floor() as i64;
    (first..=last).map(move |k| k as f64 * step)
}

/// A degree label for a graticule line: as many decimals as the step needs,
/// the hemisphere as a letter.
fn format_degrees(value: f64, step: f64, positive: char, negative: char) -> String {
    let decimals = if step >= 1. {
        0
    } else {
        (-step.log10()).ceil() as usize
    };
    if value.abs() < step / 2. {
        return "0°".into();
    }
    let hemisphere = if value > 0. { positive } else { negative };
    format!("{:.*}°{hemisphere}", decimals, value.abs())
}

/// The map's projection onto a plot area: projected degrees to pixels
/// relative to the plot's origin, one scale on both axes so shapes keep their
/// proportions.
#[derive(Clone, Copy)]
struct Viewport {
    area: Bounds<f32>,
    center_x: f64,
    center_y: f64,
    scale: f64,
}

impl Viewport {
    fn fit(extent: (f64, f64, f64, f64), plot: Size<Pixels>) -> Self {
        let area = Bounds {
            origin: point(LEFT_GUTTER, EDGE_PAD),
            size: size(
                (plot.width.as_f32() - LEFT_GUTTER - EDGE_PAD).max(1.),
                (plot.height.as_f32() - BOTTOM_GUTTER - EDGE_PAD).max(1.),
            ),
        };
        let (x0, x1, y0, y1) = extent;
        // A margin of 6% keeps edge points off the frame.
        let span_x = (x1 - x0).max(MIN_SPAN) * 1.12;
        let span_y = (y1 - y0).max(MIN_SPAN) * 1.12;
        let scale = (area.size.width as f64 / span_x).min(area.size.height as f64 / span_y);
        Self {
            area,
            center_x: (x0 + x1) / 2.,
            center_y: (y0 + y1) / 2.,
            scale,
        }
    }

    fn project(&self, x: f64, y: f64) -> (f32, f32) {
        let a = &self.area;
        (
            a.origin.x + a.size.width / 2. + ((x - self.center_x) * self.scale) as f32,
            a.origin.y + a.size.height / 2. - ((y - self.center_y) * self.scale) as f32,
        )
    }

    /// The projected extent the plot area shows, `(min_x, max_x, min_y, max_y)`.
    fn visible(&self) -> (f64, f64, f64, f64) {
        let half_w = self.area.size.width as f64 / 2. / self.scale;
        let half_h = self.area.size.height as f64 / 2. / self.scale;
        (
            self.center_x - half_w,
            self.center_x + half_w,
            self.center_y - half_h,
            self.center_y + half_h,
        )
    }
}

/// The map element. Rebuilt every frame from the cached `GeoData`; holding the
/// points behind an `Arc` keeps that a refcount bump.
pub(crate) struct GeoPlot {
    id: ElementId,
    data: Arc<GeoData>,
}

impl GeoPlot {
    pub(crate) fn new(id: impl Into<ElementId>, data: Arc<GeoData>) -> Self {
        Self {
            id: id.into(),
            data,
        }
    }

    fn color_of(&self, p: &GeoPoint, cx: &App) -> Hsla {
        match p.category {
            Some(slot) => self.data.category_color(slot, cx),
            None => cx.theme().blue,
        }
    }
}

impl IntoElement for GeoPlot {
    type Element = PlotElement<Self>;

    fn into_element(self) -> Self::Element {
        PlotElement::new(self)
    }
}

impl Plot for GeoPlot {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let view = Viewport::fit(self.data.extent, bounds.size);
        let area = Bounds {
            origin: bounds.origin + point(px(view.area.origin.x), px(view.area.origin.y)),
            size: size(px(view.area.size.width), px(view.area.size.height)),
        };
        let theme = cx.theme();
        let (muted, grid, border, background) = (
            theme.muted_foreground,
            theme.chart_grid,
            theme.border,
            theme.background,
        );

        let dark = theme.mode.is_dark();

        // The plot area reads as a map sheet: a faint fill and a frame.
        window.paint_quad(quad(
            area,
            px(4.),
            Background::from(theme.muted.opacity(0.35)),
            px(1.),
            border,
            BorderStyle::default(),
        ));

        // The base map, when tiles are on and have arrived. Whatever has not
        // arrived yet leaves the sheet showing through.
        let base_map = crate::ui::tiles::enabled(cx);
        let mut drew_tiles = false;
        if base_map {
            let mut z = crate::ui::tiles::zoom_for(view.scale);
            let mut wanted = crate::ui::tiles::covering(view.visible(), z);
            while wanted.len() > MAX_TILES && z > 0 {
                z -= 1;
                wanted = crate::ui::tiles::covering(view.visible(), z);
            }
            for (key, image) in crate::ui::tiles::visible(&wanted, window, cx) {
                let (x0, x1, y0, y1) = crate::ui::tiles::extent_of(key);
                let (left, top) = view.project(x0, y1);
                let (right, bottom) = view.project(x1, y0);
                let tile = Bounds::from_corners(
                    bounds.origin + point(px(left), px(top)),
                    bounds.origin + point(px(right), px(bottom)),
                );
                drew_tiles |= window
                    .paint_image(area, tile, Corners::all(px(4.)), image, 0, false)
                    .is_ok();
            }
            // Tiles are drawn for daylight; in the dark theme a veil of the
            // background keeps them from glaring behind the points.
            if drew_tiles && dark {
                window.paint_quad(fill(area, background.opacity(0.35)).corner_radii(px(4.)));
            }
        }
        let theme = cx.theme();

        // Graticule: meridians at round longitudes, parallels at round
        // latitudes (unevenly spaced, as Mercator spaces them). Over a base
        // map the streets are the reference; only the labels stay.
        let (vx0, vx1, vy0, vy1) = view.visible();
        let lng_step = nice_step(vx1 - vx0, view.area.size.width as f64);
        let (lat0, lat1) = (inverse_mercator_y(vy0), inverse_mercator_y(vy1));
        let lat_step = nice_step(lat1 - lat0, view.area.size.height as f64);
        let meridians: Vec<(f32, f64)> = steps_within(vx0.max(-180.), vx1.min(180.), lng_step)
            .map(|lng| (view.project(lng, 0.).0, lng))
            .collect();
        let parallels: Vec<(f32, f64)> = steps_within(lat0.max(-85.), lat1.min(85.), lat_step)
            .map(|lat| (view.project(0., mercator_y(lat)).1, lat))
            .collect();
        if !drew_tiles {
            Grid::new()
                .x(meridians.iter().map(|(x, _)| px(x - view.area.origin.x)))
                .y(parallels.iter().map(|(y, _)| px(y - view.area.origin.y)))
                .stroke(grid)
                .dash_array(&[px(4.), px(2.)])
                .paint(&area, window);
        }

        let mut labels: Vec<Text> = meridians
            .iter()
            .map(|(x, lng)| {
                Text::new(
                    format_degrees(*lng, lng_step, 'E', 'W'),
                    point(px(*x), px(view.area.origin.y + view.area.size.height + 4.)),
                    muted,
                )
                .align(TextAlign::Center)
            })
            .collect();
        labels.extend(parallels.iter().map(|(y, lat)| {
            Text::new(
                format_degrees(*lat, lat_step, 'N', 'S'),
                point(px(LEFT_GUTTER - 6.), px(y - TEXT_SIZE / 2. - 1.)),
                muted,
            )
            .align(TextAlign::Right)
        }));
        // The tile policy asks for the credit on the map itself, whenever its
        // tiles are shown.
        if drew_tiles {
            labels.push(
                Text::new(
                    crate::ui::tiles::ATTRIBUTION,
                    point(
                        px(view.area.origin.x + view.area.size.width - 6.),
                        px(view.area.origin.y + view.area.size.height - TEXT_SIZE - 6.),
                    ),
                    if dark { theme.foreground } else { muted },
                )
                .align(TextAlign::Right),
            );
        }
        PlotLabel::new(labels).paint(&bounds, window, cx);

        // One quad per point, ringed in the background color so overlapping
        // points stay countable.
        // Solid fills: a translucent dot on the pale sheet reads as blurred.
        let ring = background;
        for p in self.data.points.iter() {
            let (x, y) = view.project(p.x, p.y);
            let color = self.color_of(p, cx);
            let d = p.diameter;
            let origin = bounds.origin + point(px(x - d / 2.), px(y - d / 2.));
            window.paint_quad(quad(
                Bounds::new(origin, size(px(d), px(d))),
                px(d / 2.),
                Background::from(color),
                px(1.),
                ring,
                BorderStyle::default(),
            ));
        }

        if let Some(key) = &self.data.size_key {
            paint_size_key(key, &view, bounds, window, cx);
        }
    }

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn tooltip_state(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _cx: &App,
    ) -> Option<TooltipState> {
        let view = Viewport::fit(self.data.extent, bounds.size);
        let (cx_, cy_) = (position.x.as_f32(), position.y.as_f32());
        // A point is under the pointer inside its own circle, or near a small
        // one. Of those, the one painted last — on top, and for a sized map
        // the smallest — is the one seen; closeness breaks the rest.
        let (index, (x, y), _) = self
            .data
            .points
            .iter()
            .enumerate()
            .filter_map(|(ix, p)| {
                let (x, y) = view.project(p.x, p.y);
                let d2 = (x - cx_).powi(2) + (y - cy_).powi(2);
                let reach = (p.diameter / 2.).max(HIT_RADIUS);
                (d2 <= reach * reach).then_some((ix, (x, y), d2))
            })
            .max_by(|a, b| {
                let inside = |(ix, _, d2): &(usize, (f32, f32), f32)| {
                    let r = self.data.points[*ix].diameter / 2.;
                    *d2 <= r * r
                };
                inside(a)
                    .cmp(&inside(b))
                    .then(if inside(a) && inside(b) {
                        a.0.cmp(&b.0)
                    } else {
                        b.2.total_cmp(&a.2).then(a.0.cmp(&b.0))
                    })
            })?;
        let at = point(px(x), px(y));
        Some(TooltipState::new(index, at, vec![at]))
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let p = self.data.points.get(state.index)?;
        let color = self.color_of(p, cx);
        let mut tooltip = Tooltip::new(cursor, bounds.size)
            .gap(px(8.))
            .dots(state.dots.iter().map(|at| {
                Dot::new(*at)
                    .size(px(HOVER_DOT_SIZE))
                    .halo(px(HOVER_HALO))
                    .stroke(cx.theme().background)
                    .fill(color)
            }))
            .title(p.label.clone());
        if let (Some(name), Some(slot)) = (&self.data.category_name, p.category) {
            let value = self.data.categories.get(slot).cloned().unwrap_or_default();
            tooltip = tooltip.row(color, name.clone(), value);
        }
        // The numbers the map is about come first; where it is, after.
        for (name, value) in self.data.measures.iter().zip(&p.measures) {
            tooltip = tooltip.plain_row(name.clone(), value.clone());
        }
        if self.data.show_coordinates {
            tooltip = tooltip
                .plain_row(self.data.lat_name.clone(), format!("{:.5}", p.lat))
                .plain_row(self.data.lng_name.clone(), format!("{:.5}", p.lng));
        }
        if self.data.label_name.is_none() {
            // Without a name column the title is the row number; say so.
            tooltip = tooltip.plain_row(tr("chart.map.no_label"), "");
        }
        Some(tooltip.into_any_element())
    }
}

/// The size key: the largest value's circle and a smaller reference inside
/// it, bottom-aligned in the plot area's lower-left corner, each labelled —
/// how a reader turns a circle back into a number.
fn paint_size_key(
    key: &SizeKey,
    view: &Viewport,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let theme = cx.theme();
    let (muted, background) = (theme.muted_foreground, theme.background);
    let small = match key.scale {
        // A quarter of the largest value draws half its diameter: the pair
        // shows that area, not width, carries the value.
        SizeScale::Sqrt => key.max / 4.,
        SizeScale::Log => key.min,
    };
    let entries: Vec<(f64, f32)> = [key.max, small]
        .into_iter()
        .filter(|v| *v > 0. || key.scale == SizeScale::Sqrt)
        .filter_map(|v| key.scale.diameter(v, key.min, key.max).map(|d| (v, d)))
        .collect();
    let Some(&(_, largest)) = entries.first() else {
        return;
    };
    let pad = 8.;
    let label_width = 64.;
    let panel = Bounds::new(
        bounds.origin
            + point(
                px(view.area.origin.x + 6.),
                px(view.area.origin.y + view.area.size.height - largest - pad * 2. - 6.),
            ),
        size(px(largest + label_width + pad * 2.), px(largest + pad * 2.)),
    );
    window.paint_quad(fill(panel, background.opacity(0.85)).corner_radii(px(4.)));
    let bottom = panel.origin.y + px(pad + largest);
    let center_x = panel.origin.x + px(pad + largest / 2.);
    let mut labels = Vec::new();
    for (value, d) in &entries {
        let origin = point(center_x - px(d / 2.), bottom - px(*d));
        window.paint_quad(quad(
            Bounds::new(origin, size(px(*d), px(*d))),
            px(d / 2.),
            Background::from(gpui_kit::transparent_black()),
            px(1.),
            muted,
            BorderStyle::default(),
        ));
        labels.push(Text::new(
            crate::ui::chart::format_value(*value),
            point(
                center_x + px(largest / 2. + 6.) - bounds.origin.x,
                bottom - px(*d) - bounds.origin.y - px(1.),
            ),
            muted,
        ));
    }
    PlotLabel::new(labels).paint(&bounds, window, cx);
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that pulls in `gpui_kit::*`, whose
    // `test` macro shadows the built-in `#[test]`.
    use super::{
        format_degrees, inverse_mercator_y, is_lat_name, is_lng_name, mercator_y, nice_step,
        GeoData, MapStyle, SizeScale, DOT_SIZE, MAX_CATEGORIES, MAX_SIZED, MIN_SIZED,
    };
    use crate::query::{ColumnKind, ColumnMeta, QueryResult};

    fn column(name: &str, kind: ColumnKind) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            duck_type: format!("{kind:?}"),
            kind,
        }
    }

    fn stations(rows: Vec<[&str; 4]>) -> QueryResult {
        QueryResult {
            columns: Vec::from([
                column("code", ColumnKind::Text),
                column("name_long", ColumnKind::Text),
                column("geo_lat", ColumnKind::Numeric),
                column("geo_lng", ColumnKind::Numeric),
            ]),
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(String::from).collect())
                .collect(),
            elapsed_ms: 0,
            truncated: false,
        }
    }

    #[test]
    fn coordinate_names_are_recognised() {
        for name in ["lat", "geo_lat", "Latitude", "geoLat", "start-lat", "纬度"] {
            assert!(is_lat_name(name), "{name}");
        }
        for name in ["lng", "geo_lng", "lon", "Longitude", "geoLon", "经度"] {
            assert!(is_lng_name(name), "{name}");
        }
        // Words that merely contain the letters are not coordinates.
        for name in ["latency", "flat", "plate"] {
            assert!(!is_lat_name(name), "{name}");
        }
        for name in ["along", "longest", "salon"] {
            assert!(!is_lng_name(name), "{name}");
        }
    }

    #[test]
    fn a_station_table_becomes_a_map() {
        let geo = GeoData::detect(&stations(Vec::from([
            ["HT", "'s-Hertogenbosch", "51.69048", "5.29362"],
            ["AC", "Abcoude", "52.27", "4.97"],
        ])))
        .expect("geo_lat/geo_lng is a map");
        assert_eq!(geo.lat_name, "geo_lat");
        assert_eq!(geo.lng_name, "geo_lng");
        assert_eq!(geo.point_count(), 2);
        // The tooltip is titled by the name column, not the code.
        assert_eq!(geo.points[0].label, "'s-Hertogenbosch");
        assert_eq!(geo.dropped, 0);
    }

    #[test]
    fn missing_or_out_of_range_coordinates_are_dropped() {
        let geo = GeoData::detect(&stations(Vec::from([
            ["A", "a", "52.1", "5.1"],
            ["B", "b", "NULL", "5.1"],
            ["C", "c", "52.3", "5.3"],
        ])))
        .unwrap();
        assert_eq!(geo.point_count(), 2);
        assert_eq!(geo.dropped, 1);
    }

    #[test]
    fn projected_coordinates_are_not_a_map() {
        // Metres in a national grid share the names but not the ranges.
        let geo = GeoData::detect(&stations(Vec::from([
            ["A", "a", "455000", "155000"],
            ["B", "b", "456000", "156000"],
        ])));
        assert!(geo.is_none());
    }

    #[test]
    fn a_text_column_that_repeats_colors_the_points() {
        let mut result = stations(Vec::new());
        result.columns.push(column("type", ColumnKind::Text));
        for ix in 0..40 {
            result.rows.push(Vec::from([
                format!("S{ix}"),
                format!("Station {ix}"),
                format!("52.{ix}"),
                "5.0".into(),
                // Seven types: the rarest fold into "other".
                format!("type-{}", ix % 7),
            ]));
        }
        let geo = GeoData::detect(&result).unwrap();
        assert_eq!(geo.category_name.as_deref(), Some("type"));
        assert_eq!(geo.categories.len(), MAX_CATEGORIES);
        assert!(geo.folded_other);
        // Unique codes are identifiers, never a category.
        assert_ne!(geo.category_name.as_deref(), Some("code"));
    }

    #[test]
    fn mercator_round_trips() {
        for lat in [-60., -1.5, 0., 23.4, 52.37] {
            assert!((inverse_mercator_y(mercator_y(lat)) - lat).abs() < 1e-9);
        }
    }

    #[test]
    fn graticule_steps_are_round() {
        // Five lines' worth of pixels.
        assert_eq!(nice_step(3.0, 450.), 1.0);
        assert_eq!(nice_step(1.2, 450.), 0.5);
        assert_eq!(nice_step(360., 450.), 30.);
        // A wide plot gets a finer grid over the same span.
        assert_eq!(nice_step(10.0, 1800.), 0.5);
        assert_eq!(format_degrees(52.0, 1.0, 'N', 'S'), "52°N");
        assert_eq!(format_degrees(-4.5, 0.5, 'E', 'W'), "4.5°W");
        assert_eq!(format_degrees(0.0, 0.5, 'E', 'W'), "0°");
    }

    fn traffic() -> QueryResult {
        QueryResult {
            columns: Vec::from([
                column("place", ColumnKind::Text),
                column("lat", ColumnKind::Numeric),
                column("lng", ColumnKind::Numeric),
                column("requests", ColumnKind::Numeric),
                column("ips", ColumnKind::Numeric),
            ]),
            rows: [
                ["Hong Kong", "22.3", "114.2", "836", "40"],
                ["Tokyo", "35.7", "139.7", "209", "12"],
                ["Singapore", "1.35", "103.8", "0", "1"],
                ["Nowhere", "10.0", "10.0", "NULL", "0"],
            ]
            .into_iter()
            .map(|r| r.into_iter().map(String::from).collect())
            .collect(),
            elapsed_ms: 0,
            truncated: false,
        }
    }

    fn sized(scale: SizeScale, tooltip: Option<Vec<usize>>) -> GeoData {
        GeoData::from_columns(
            &traffic(),
            1,
            2,
            &MapStyle {
                color: None,
                size: Some((3, scale)),
                tooltip,
            },
        )
    }

    fn diameter_of(geo: &GeoData, label: &str) -> f32 {
        geo.points.iter().find(|p| p.label == label).unwrap().diameter
    }

    #[test]
    fn area_follows_the_value() {
        let geo = sized(SizeScale::Sqrt, None);
        let (hk, tokyo) = (diameter_of(&geo, "Hong Kong"), diameter_of(&geo, "Tokyo"));
        assert_eq!(hk, MAX_SIZED);
        // A quarter of the value is a quarter of the area: half the diameter.
        let ratio = (tokyo / hk) as f64;
        assert!((ratio - (209f64 / 836.).sqrt()).abs() < 1e-3, "{ratio}");
        // Zero is on the scale, just too small to see unclamped.
        assert_eq!(diameter_of(&geo, "Singapore"), MIN_SIZED);
        // A missing value is drawn smallest, and counted.
        assert_eq!(diameter_of(&geo, "Nowhere"), MIN_SIZED);
        assert_eq!(geo.size_missing, 1);
        assert_eq!(geo.size_name.as_deref(), Some("requests"));
    }

    #[test]
    fn a_log_scale_spreads_orders_of_magnitude_and_rejects_zero() {
        let geo = sized(SizeScale::Log, None);
        // Smallest positive value at the minimum, largest at the maximum.
        assert_eq!(diameter_of(&geo, "Tokyo"), MIN_SIZED);
        assert_eq!(diameter_of(&geo, "Hong Kong"), MAX_SIZED);
        // Zero has no logarithm: it and the missing value are counted.
        assert_eq!(geo.size_missing, 2);
    }

    #[test]
    fn big_points_paint_first_so_small_ones_stay_on_top() {
        let geo = sized(SizeScale::Sqrt, None);
        let diameters: Vec<f32> = geo.points.iter().map(|p| p.diameter).collect();
        assert!(diameters.windows(2).all(|w| w[0] >= w[1]), "{diameters:?}");
    }

    #[test]
    fn the_tooltip_leads_with_the_numbers() {
        // Unasked: the size column, then the other numbers, then where.
        let geo = sized(SizeScale::Sqrt, None);
        assert_eq!(geo.measures, ["requests", "ips"]);
        assert!(geo.show_coordinates);
        let hk = geo.points.iter().find(|p| p.label == "Hong Kong").unwrap();
        assert_eq!(hk.measures, ["836", "40"]);

        // Asked: exactly those columns, in that order, and no coordinates.
        let geo = sized(SizeScale::Sqrt, Some(vec![4, 0]));
        assert_eq!(geo.measures, ["ips", "place"]);
        assert!(!geo.show_coordinates);
    }

    #[test]
    fn an_unsized_map_keeps_its_dots_and_still_shows_numbers() {
        let geo = GeoData::detect(&traffic()).unwrap();
        assert!(geo.points.iter().all(|p| p.diameter == DOT_SIZE));
        assert_eq!(geo.size_name, None);
        assert_eq!(geo.measures, ["requests", "ips"]);
    }
}
