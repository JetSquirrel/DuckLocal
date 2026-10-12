//! The series palette every chart, plot, pie and map draws from.
//!
//! The component library's `chart.1`–`chart.5` are five shades of one blue:
//! fine for an ordered ramp, but as series they read as one colour, so a
//! pivoted line, a pie's slices or a map's categories could not be told
//! apart. These are eight distinct hues instead, in a fixed order that keeps
//! every adjacent pair apart under the common colour-vision deficiencies,
//! with steps chosen per mode: the dark ones are lifted to read on a dark
//! surface, not the light ones reused.
//!
//! A series takes its slot by its position in the query's columns or rows,
//! so a colour names the series wherever it shows — line, bar, legend and
//! tooltip alike. Past eight the slots repeat; `chart` caps series at a dozen.

use gpui_kit::component::ActiveTheme;
use gpui_kit::{rgb, App, Hsla};

/// How many distinct colours the palette holds.
pub const SLOTS: usize = 8;

/// Blue, orange, aqua, yellow, magenta, green, violet, red, on a light surface.
const LIGHT: [u32; SLOTS] = [
    0x2a78d6, 0xeb6834, 0x1baf7a, 0xeda100, 0xe87ba4, 0x008300, 0x6250d6, 0xe34948,
];
/// The same hues, stepped for a dark surface.
const DARK: [u32; SLOTS] = [
    0x3987e5, 0xd95926, 0x199e70, 0xc98500, 0xd55181, 0x008300, 0x9085e9, 0xe66767,
];

/// The palette for the current theme's mode, slot 0 first.
pub fn series(cx: &App) -> [Hsla; SLOTS] {
    let hexes = if cx.theme().mode.is_dark() {
        DARK
    } else {
        LIGHT
    };
    hexes.map(|hex| rgb(hex).into())
}

/// The colour of series `slot`.
pub fn color(slot: usize, cx: &App) -> Hsla {
    series(cx)[slot % SLOTS]
}

/// `#rrggbb` of the first five slots in `dark` or light mode, for the theme
/// overrides: the library's own charts default to `chart.1`–`chart.5`, and
/// should agree with ours.
pub(crate) fn theme_hexes(dark: bool) -> [String; 5] {
    let hexes = if dark { DARK } else { LIGHT };
    std::array::from_fn(|i| format!("#{:06x}", hexes[i]))
}

/// A card `tone`'s two colours: the rule across the card's top, and the ink
/// its value is set in. A status colour bright enough to mark a rule is too
/// faint to read as text on a light surface (amber most of all), so the ink
/// takes a darker step of the same hue there. `None` for an unknown tone.
///
/// The status hues are kept apart from the series slots, so a warning card
/// never reads as "series 4"; the card's label says what the colour means.
pub fn tone(name: &str, cx: &App) -> Option<(Hsla, Hsla)> {
    let dark = cx.theme().mode.is_dark();
    let (rule, ink) = match (name, dark) {
        ("accent", false) => (LIGHT[0], LIGHT[0]),
        ("accent", true) => (DARK[0], DARK[0]),
        ("good", false) => (0x0ca30c, 0x006300),
        ("good", true) => (0x0ca30c, 0x0ca30c),
        ("warning", false) => (0xfab219, 0xb45309),
        ("warning", true) => (0xfab219, 0xfab219),
        ("critical", false) => (0xd03b3b, 0xd03b3b),
        ("critical", true) => (0xd03b3b, 0xe66767),
        _ => return None,
    };
    Some((rgb(rule).into(), rgb(ink).into()))
}

/// A dashboard's page: a warm grey the plot cards stand out from. In the
/// dark the page keeps the window's background and the cards are lifted
/// instead, since there is no darker step to sink the page to.
pub fn page(cx: &App) -> Hsla {
    if cx.theme().mode.is_dark() {
        cx.theme().background
    } else {
        rgb(0xf7f7f5).into()
    }
}

/// A dashboard plot card's surface, on [`page`].
pub fn panel(cx: &App) -> Hsla {
    if cx.theme().mode.is_dark() {
        rgb(0x1c1c1b).into()
    } else {
        rgb(0xffffff).into()
    }
}
