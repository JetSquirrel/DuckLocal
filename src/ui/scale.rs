//! Interface size: one setting that scales text, icons and controls together.
//!
//! The component library sizes almost everything in rems — spacing, icons,
//! button heights, text — and `Root` sets the window's rem to the theme's
//! `font_size` on every frame. So the whole interface scales from one number,
//! and this module owns it: a few named steps rather than a free slider,
//! because a 15.3px base only buys blurry text.
//!
//! `Theme::change` rebuilds the theme from stock defaults, which resets the
//! base to the library's 16px; every call site that changes theme follows it
//! with [`apply`]. The choice is persisted like the language — a blocking write
//! to the history store, fine from a click or a key press.

use std::sync::RwLock;

use gpui_kit::component::Theme;
use gpui_kit::*;

/// A length designed in pixels at the default size, scaled to the current one,
/// for the few containers sized in raw pixels — a dialog's width, the status
/// bar's height — that would otherwise squeeze larger text or leave smaller
/// text adrift. Pixels rather than rems because a dialog's width takes pixels;
/// every render reads the current size, so a change reaches it on the next
/// frame like everything else.
pub fn design(pixels: f32) -> Pixels {
    px(pixels * f32::from(current().font_size()) / f32::from(UiSize::Default.font_size()))
}

/// The `settings` key the choice lives under.
const SETTING: &str = "ui_size";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiSize {
    Small,
    Default,
    Large,
    ExtraLarge,
}

impl UiSize {
    pub const ALL: [UiSize; 4] = [
        UiSize::Small,
        UiSize::Default,
        UiSize::Large,
        UiSize::ExtraLarge,
    ];

    fn code(self) -> &'static str {
        match self {
            UiSize::Small => "small",
            UiSize::Default => "default",
            UiSize::Large => "large",
            UiSize::ExtraLarge => "xlarge",
        }
    }

    /// An unknown stored value is the default size rather than an error: the
    /// setting is a preference, not data.
    fn from_code(code: &str) -> UiSize {
        UiSize::ALL
            .into_iter()
            .find(|size| size.code() == code)
            .unwrap_or(UiSize::Default)
    }

    /// The rem base: 14px is the app's long-standing density, a notch under
    /// the library's 16px.
    fn font_size(self) -> Pixels {
        px(match self {
            UiSize::Small => 13.,
            UiSize::Default => 14.,
            UiSize::Large => 16.,
            UiSize::ExtraLarge => 18.,
        })
    }

    /// The editor's monospace size, kept one pixel under the UI text as the
    /// library's own defaults (16 / 13) keep it a little under.
    fn mono_font_size(self) -> Pixels {
        self.font_size() - px(1.)
    }

    pub fn label_key(self) -> &'static str {
        match self {
            UiSize::Small => "ui_size.small",
            UiSize::Default => "ui_size.default",
            UiSize::Large => "ui_size.large",
            UiSize::ExtraLarge => "ui_size.xlarge",
        }
    }

    /// One step larger, or the same when already the largest.
    pub fn larger(self) -> UiSize {
        let ix = UiSize::ALL.iter().position(|size| *size == self).unwrap_or(1);
        UiSize::ALL[(ix + 1).min(UiSize::ALL.len() - 1)]
    }

    /// One step smaller, or the same when already the smallest.
    pub fn smaller(self) -> UiSize {
        let ix = UiSize::ALL.iter().position(|size| *size == self).unwrap_or(1);
        UiSize::ALL[ix.saturating_sub(1)]
    }
}

static CURRENT: RwLock<UiSize> = RwLock::new(UiSize::Default);

pub fn current() -> UiSize {
    *CURRENT.read().unwrap_or_else(|e| e.into_inner())
}

/// The size to start with: the stored choice when the history store is
/// reachable, the default otherwise. Never writes.
pub fn load() {
    if let Ok(Some(code)) = crate::history::get_setting(SETTING) {
        *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = UiSize::from_code(&code);
    }
}

/// Put the current size into the theme. Call after anything that rebuilt the
/// theme (`Theme::change`), and once at startup.
pub fn apply(cx: &mut App) {
    let size = current();
    let theme = Theme::global_mut(cx);
    theme.font_size = size.font_size();
    theme.mono_font_size = size.mono_font_size();
    Theme::sync_base(cx);
}

/// Switch to `size`, remember it, and repaint every window in it.
pub fn set(size: UiSize, cx: &mut App) {
    if size == current() {
        return;
    }
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = size;
    if let Err(e) = crate::history::set_setting(SETTING, size.code()) {
        tracing::warn!("Failed to persist the interface size: {e}");
    }
    apply(cx);
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::UiSize;

    #[test]
    fn steps_stop_at_both_ends() {
        assert_eq!(UiSize::Small.smaller(), UiSize::Small);
        assert_eq!(UiSize::Small.larger(), UiSize::Default);
        assert_eq!(UiSize::Default.larger(), UiSize::Large);
        assert_eq!(UiSize::ExtraLarge.larger(), UiSize::ExtraLarge);
        assert_eq!(UiSize::ExtraLarge.smaller(), UiSize::Large);
    }

    #[test]
    fn a_stored_value_round_trips_and_an_unknown_one_is_the_default() {
        for size in UiSize::ALL {
            assert_eq!(UiSize::from_code(size.code()), size);
        }
        assert_eq!(UiSize::from_code("enormous"), UiSize::Default);
    }

    #[test]
    fn a_design_length_is_unchanged_at_the_default_size() {
        // `current()` is the default unless something set it; no test does.
        assert_eq!(f32::from(super::design(440.)), 440.);
    }

    #[test]
    fn the_default_keeps_the_long_standing_density() {
        assert_eq!(f32::from(UiSize::Default.font_size()), 14.);
        assert!(UiSize::Small.font_size() < UiSize::Default.font_size());
        assert!(UiSize::ExtraLarge.mono_font_size() < UiSize::ExtraLarge.font_size());
    }
}
