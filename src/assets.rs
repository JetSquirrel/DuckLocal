//! The asset source the application registers.
//!
//! The component library's default bundle holds only the icons its own
//! components draw. An icon outside it is still a valid `IconName` — the
//! enum covers the whole catalog — so it compiles, renders blank and logs
//! "could not find asset" on every frame. The icons DuckLocal draws beyond
//! the defaults are embedded here, one by one, rather than the whole catalog
//! (`AllAssets`): the release build is optimized for size, and a thousand
//! unused SVGs are not free.
//!
//! Adding an icon from outside the default bundle means adding it to
//! `ExtraIcons` too; `every_icon_the_app_draws_is_served` checks that.

use std::borrow::Cow;

use gpui_kit::assets::{icon_assets, Assets};
use gpui_kit::{AssetSource, Result, SharedString};

icon_assets!(
    ExtraIcons,
    [Save, AppWindow, WandSparkles, ListTree, Pencil, Download, Code]
);

/// The default component bundle, plus [`ExtraIcons`].
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::AppAssets;
    use gpui_kit::assets::IconName;
    use gpui_kit::AssetSource;

    /// Every icon named in the source, found by scanning for `IconName::` and
    /// its alias `AssetIcon::`.
    /// A scan rather than a list, so a new icon cannot be added without
    /// this test noticing.
    fn icons_in_source() -> Vec<String> {
        fn walk(dir: &std::path::Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, found);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    // `AssetIcon` is how the workspace names the full catalog.
                    let pieces = text
                        .split("IconName::")
                        .skip(1)
                        .chain(text.split("AssetIcon::").skip(1));
                    for piece in pieces {
                        let name: String = piece
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric())
                            .collect();
                        if name.starts_with(|c: char| c.is_ascii_uppercase())
                            && name != "ALL"
                            && !found.contains(&name)
                        {
                            found.push(name);
                        }
                    }
                }
            }
        }
        let mut found = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut found);
        found
    }

    #[test]
    fn every_icon_the_app_draws_is_served() {
        let names = icons_in_source();
        for expected in ["Save", "Pencil", "SquareTerminal"] {
            assert!(names.contains(&expected.to_string()), "{names:?}");
        }
        for name in names {
            let icon = IconName::ALL
                .iter()
                .find(|icon| format!("{icon:?}") == name)
                .unwrap_or_else(|| panic!("IconName::{name} is not in the catalog"));
            let path = icon.path();
            assert!(
                matches!(AppAssets.load(&path), Ok(Some(_))),
                "IconName::{name} ({path}) is not served; add it to ExtraIcons"
            );
        }
    }
}
