//! The base map under a map chart: OpenStreetMap raster tiles, fetched on
//! demand, kept on disk, and drawn in the same Web Mercator projection the
//! points use.
//!
//! It is off until someone turns it on (View → Online base map), because it
//! is the one place the app would reach the network on its own: what leaves
//! the machine is which tiles are wanted — roughly the area and zoom being
//! looked at — never a row of data. Off, or offline, the map draws over its
//! graticule. The choice is the `map_tiles` setting, `on` or `off`.
//!
//! The OpenStreetMap Foundation's tile policy is what shapes the fetching: an
//! identifying User-Agent, at most two requests at a time, tiles cached for a
//! week and reused, never a bulk prefetch, and the attribution on the map.
//! `map_tile_url` points at another `{z}/{x}/{y}` server for heavier use.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use gpui_kit::*;
use image::Frame;

pub(crate) const ATTRIBUTION: &str = "© OpenStreetMap contributors";
const DEFAULT_TEMPLATE: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
/// Tiles are 256 px squares at every zoom.
pub(crate) const TILE_SIZE: f64 = 256.;
/// The deepest zoom the default server renders.
pub(crate) const MAX_ZOOM: u8 = 19;
/// The policy's limit on parallel downloads.
const MAX_IN_FLIGHT: usize = 2;
/// Requests waiting beyond this are ones the view has already moved past.
const MAX_QUEUED: usize = 64;
/// Decoded tiles kept for drawing: 256 × 256 × 4 bytes each, so ~50 MB.
const MAX_READY: usize = 200;
/// How long a tile on disk is used without asking again: the policy's floor.
const FRESH_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How long a failed tile waits before it is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(30);
const MAX_TILE_BYTES: u64 = 2 * 1024 * 1024;

/// One tile of the square Web Mercator world: `2^z` tiles a side at zoom `z`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TileKey {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

enum Entry {
    Queued,
    Loading,
    Ready { image: Arc<RenderImage>, used: u64 },
    Failed(Instant),
}

struct Config {
    enabled: bool,
    template: String,
}

impl Config {
    fn load() -> Self {
        let setting = |key| crate::history::get_setting(key).ok().flatten();
        let enabled = setting("map_tiles").is_some_and(|value| value == "on");
        let template = setting("map_tile_url")
            .filter(|url| url.contains("{z}") && url.contains("{x}") && url.contains("{y}"))
            .unwrap_or_else(|| DEFAULT_TEMPLATE.to_string());
        Self { enabled, template }
    }
}

#[derive(Default)]
struct Tiles {
    config: Option<Config>,
    entries: HashMap<TileKey, Entry>,
    queue: VecDeque<TileKey>,
    in_flight: usize,
    /// Bumped once per `visible` call, so eviction knows what was drawn last.
    frame: u64,
}

impl Global for Tiles {}

/// Whether the base map is on: only once someone has turned it on.
pub(crate) fn enabled(cx: &mut App) -> bool {
    let tiles = cx.default_global::<Tiles>();
    tiles.config.get_or_insert_with(Config::load).enabled
}

/// Turn the base map on or off, remember the choice, and redraw every map.
pub(crate) fn toggle(cx: &mut App) {
    let tiles = cx.default_global::<Tiles>();
    let config = tiles.config.get_or_insert_with(Config::load);
    config.enabled = !config.enabled;
    let value = if config.enabled { "on" } else { "off" };
    if let Err(error) = crate::history::set_setting("map_tiles", value) {
        tracing::warn!("map_tiles not saved: {error:#}");
    }
    cx.refresh_windows();
}

/// The tiles among `wanted` that are ready to draw. The rest are asked for;
/// the windows repaint as each arrives.
pub(crate) fn visible(
    wanted: &[TileKey],
    window: &mut Window,
    cx: &mut App,
) -> Vec<(TileKey, Arc<RenderImage>)> {
    let now = Instant::now();
    let tiles = cx.default_global::<Tiles>();
    tiles.config.get_or_insert_with(Config::load);
    tiles.frame += 1;
    let frame = tiles.frame;
    let mut ready = Vec::new();
    for key in wanted {
        match tiles.entries.get_mut(key) {
            Some(Entry::Ready { image, used }) => {
                *used = frame;
                ready.push((*key, image.clone()));
            }
            Some(Entry::Queued | Entry::Loading) => {}
            Some(Entry::Failed(at)) if now.duration_since(*at) < RETRY_AFTER => {}
            _ => {
                tiles.entries.insert(*key, Entry::Queued);
                tiles.queue.push_back(*key);
            }
        }
    }
    // The oldest requests are for views already scrolled or resized away.
    while tiles.queue.len() > MAX_QUEUED {
        if let Some(stale) = tiles.queue.pop_front() {
            tiles.entries.remove(&stale);
        }
    }
    let evicted = evict(tiles, frame);
    for image in evicted {
        cx.drop_image(image, Some(window));
    }
    pump(cx);
    ready
}

/// Drop the least recently drawn tiles past `MAX_READY`, never one drawn in
/// this frame. Returns their images, for the atlas to release.
fn evict(tiles: &mut Tiles, frame: u64) -> Vec<Arc<RenderImage>> {
    let mut ready: Vec<(u64, TileKey)> = tiles
        .entries
        .iter()
        .filter_map(|(key, entry)| match entry {
            Entry::Ready { used, .. } if *used != frame => Some((*used, *key)),
            _ => None,
        })
        .collect();
    let total = ready.len()
        + tiles
            .entries
            .values()
            .filter(|entry| matches!(entry, Entry::Ready { used, .. } if *used == frame))
            .count();
    if total <= MAX_READY {
        return Vec::new();
    }
    ready.sort_unstable_by_key(|(used, _)| *used);
    ready
        .into_iter()
        .take(total - MAX_READY)
        .filter_map(|(_, key)| match tiles.entries.remove(&key) {
            Some(Entry::Ready { image, .. }) => Some(image),
            _ => None,
        })
        .collect()
}

/// Start downloads while there is room: the newest request first, since it
/// is the view on screen now.
fn pump(cx: &mut App) {
    loop {
        let tiles = cx.global_mut::<Tiles>();
        if tiles.in_flight >= MAX_IN_FLIGHT {
            return;
        }
        let Some(key) = tiles.queue.pop_back() else {
            return;
        };
        tiles.entries.insert(key, Entry::Loading);
        tiles.in_flight += 1;
        let template = tiles
            .config
            .as_ref()
            .map_or(DEFAULT_TEMPLATE, |config| config.template.as_str())
            .to_string();
        let task = cx
            .background_executor()
            .spawn(async move { load(&template, key) });
        cx.spawn(async move |cx| {
            let result = task.await;
            cx.update(|cx| {
                let tiles = cx.global_mut::<Tiles>();
                tiles.in_flight -= 1;
                let frame = tiles.frame;
                match result {
                    Ok(image) => {
                        tiles
                            .entries
                            .insert(key, Entry::Ready { image, used: frame });
                    }
                    Err(error) => {
                        tracing::debug!("map tile {key:?}: {error:#}");
                        tiles.entries.insert(key, Entry::Failed(Instant::now()));
                    }
                }
                pump(cx);
                cx.refresh_windows();
            });
        })
        .detach();
    }
}

/// A tile from the disk cache while it is fresh, from the server otherwise,
/// and from a stale copy when the server cannot be reached. Blocking.
fn load(template: &str, key: TileKey) -> anyhow::Result<Arc<RenderImage>> {
    let path = cache_path(template, key);
    let cached = path.as_ref().and_then(|path| {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default();
        Some((std::fs::read(path).ok()?, age < FRESH_FOR))
    });
    let bytes = match cached {
        Some((bytes, true)) => bytes,
        stale => match fetch(template, key) {
            Ok(bytes) => {
                if let Some(path) = &path {
                    store(path, &bytes);
                }
                bytes
            }
            Err(error) => match stale {
                Some((bytes, false)) => bytes,
                _ => return Err(error),
            },
        },
    };
    decode(&bytes)
}

fn fetch(template: &str, key: TileKey) -> anyhow::Result<Vec<u8>> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .user_agent(concat!(
                "DuckLocal/",
                env!("CARGO_PKG_VERSION"),
                " (+https://ducklocal.app)"
            ))
            .build()
            .into()
    });
    let mut response = agent.get(&tile_url(template, key)).call()?;
    Ok(response
        .body_mut()
        .with_config()
        .limit(MAX_TILE_BYTES)
        .read_to_vec()?)
}

pub(crate) fn tile_url(template: &str, key: TileKey) -> String {
    template
        .replace("{z}", &key.z.to_string())
        .replace("{x}", &key.x.to_string())
        .replace("{y}", &key.y.to_string())
}

/// `<data>/tiles/<server>/<z>/<x>/<y>`: one directory per tile server, so a
/// changed `map_tile_url` never draws the old server's tiles.
fn cache_path(template: &str, key: TileKey) -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "DuckLocal")?;
    Some(
        dirs.cache_dir()
            .join("tiles")
            .join(format!("{:016x}", fnv1a(template.as_bytes())))
            .join(key.z.to_string())
            .join(key.x.to_string())
            .join(key.y.to_string()),
    )
}

/// Written whole and renamed into place, so a crash never leaves half a tile
/// to be read back as fresh. A cache that cannot be written only costs a
/// download next time.
fn store(path: &std::path::Path, bytes: &[u8]) {
    let Some(parent) = path.parent() else { return };
    let temporary = path.with_extension(format!("part{}", std::process::id()));
    let written = std::fs::create_dir_all(parent)
        .and_then(|_| std::fs::write(&temporary, bytes))
        .and_then(|_| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// PNG or JPEG bytes to the BGRA frame GPUI draws.
fn decode(bytes: &[u8]) -> anyhow::Result<Arc<RenderImage>> {
    let mut pixels = image::load_from_memory(bytes)?.into_rgba8();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new([Frame::new(pixels)])))
}

/// The zoom whose tiles draw closest to their own size at `scale` pixels per
/// projected degree — never enlarged, which blurs, so at most one level finer
/// than the view needs.
pub(crate) fn zoom_for(scale: f64) -> u8 {
    // The world is 360 projected degrees, and `TILE_SIZE × 2^z` pixels wide.
    let level = (scale * 360. / TILE_SIZE).log2().ceil();
    level.clamp(0., MAX_ZOOM as f64) as u8
}

/// The tiles covering projected `(min_x, max_x, min_y, max_y)` at zoom `z`,
/// clipped to the world.
pub(crate) fn covering(extent: (f64, f64, f64, f64), z: u8) -> Vec<TileKey> {
    let n = 1u32 << z;
    let per_tile = 360. / n as f64;
    let (x0, x1, y0, y1) = extent;
    let column = |x: f64| (((x + 180.) / per_tile).floor().max(0.) as u32).min(n - 1);
    // Tile rows count down from the top, y = +180.
    let row = |y: f64| (((180. - y) / per_tile).floor().max(0.) as u32).min(n - 1);
    let (c0, c1) = (column(x0.max(-180.)), column(x1.min(180.)));
    let (r0, r1) = (row(y1.min(180.)), row(y0.max(-180.)));
    let mut keys = Vec::new();
    for y in r0..=r1 {
        for x in c0..=c1 {
            keys.push(TileKey { z, x, y });
        }
    }
    keys
}

/// A tile's projected extent: `(min_x, max_x, min_y, max_y)`.
pub(crate) fn extent_of(key: TileKey) -> (f64, f64, f64, f64) {
    let per_tile = 360. / (1u32 << key.z) as f64;
    let x0 = key.x as f64 * per_tile - 180.;
    let y1 = 180. - key.y as f64 * per_tile;
    (x0, x0 + per_tile, y1 - per_tile, y1)
}

#[cfg(test)]
mod tests {
    use super::{covering, extent_of, tile_url, zoom_for, TileKey, DEFAULT_TEMPLATE, MAX_ZOOM};

    #[test]
    fn the_world_is_one_tile_at_zoom_zero() {
        assert_eq!(
            covering((-180., 180., -180., 180.), 0),
            vec![TileKey { z: 0, x: 0, y: 0 }]
        );
        assert_eq!(
            extent_of(TileKey { z: 0, x: 0, y: 0 }),
            (-180., 180., -180., 180.)
        );
    }

    #[test]
    fn tiles_count_rows_from_the_top() {
        // Amsterdam, 4.9°E 52.37°N, is tile 131/84 at zoom 8.
        let y = crate::ui::geo::project_lat(52.37);
        let keys = covering((4.9, 4.9, y, y), 8);
        assert_eq!(
            keys,
            vec![TileKey {
                z: 8,
                x: 131,
                y: 84
            }]
        );
        let (x0, x1, y0, y1) = extent_of(keys[0]);
        assert!(x0 <= 4.9 && 4.9 < x1 && y0 <= y && y < y1);
    }

    #[test]
    fn a_view_past_the_edge_of_the_world_is_clipped() {
        let keys = covering((-500., 500., -500., 500.), 1);
        assert_eq!(keys.len(), 4);
    }

    #[test]
    fn zoom_follows_the_scale_and_never_enlarges_a_tile() {
        // One tile across 360 degrees at 256 px is zoom 0 exactly.
        assert_eq!(zoom_for(256. / 360.), 0);
        // A little more detail than zoom 0 gives needs zoom 1.
        assert_eq!(zoom_for(300. / 360.), 1);
        assert_eq!(zoom_for(1e12), MAX_ZOOM);
        assert_eq!(zoom_for(1e-6), 0);
    }

    #[test]
    fn a_tile_url_fills_the_template() {
        assert_eq!(
            tile_url(
                DEFAULT_TEMPLATE,
                TileKey {
                    z: 8,
                    x: 131,
                    y: 84
                }
            ),
            "https://tile.openstreetmap.org/8/131/84.png"
        );
    }
}
