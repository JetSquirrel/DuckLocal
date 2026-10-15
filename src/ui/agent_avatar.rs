//! An agent's avatar: its maker's mark on its brand colour — Claude's on
//! terracotta, OpenAI's on black — so a comment or a name in a dashboard's
//! corner says at a glance which AI it was. An agent with no known mark gets
//! its initial on the agent colour. The person gets a plain figure on a
//! neutral ground, which no maker's colour can be mistaken for.

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::{ActiveTheme, Icon};
use gpui_kit::*;

/// A maker whose mark DuckLocal carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Maker {
    Anthropic,
    OpenAi,
    DeepSeek,
    Google,
    Cursor,
}

impl Maker {
    /// The maker an agent's name gives away — `Claude`, `Sonnet`, `Codex`,
    /// `gpt-5`, `DeepSeek-V3` — or none.
    pub fn of(agent: &str) -> Option<Self> {
        let name = agent.to_ascii_lowercase();
        let has = |words: &[&str]| words.iter().any(|word| name.contains(word));
        if has(&["claude", "anthropic", "opus", "sonnet", "haiku", "fable"]) {
            Some(Self::Anthropic)
        } else if has(&["codex", "openai", "gpt", "chatgpt", "oai"]) {
            Some(Self::OpenAi)
        } else if has(&["deepseek"]) {
            Some(Self::DeepSeek)
        } else if has(&["gemini", "google"]) {
            Some(Self::Google)
        } else if has(&["cursor"]) {
            Some(Self::Cursor)
        } else {
            None
        }
    }

    fn mark(self) -> &'static str {
        match self {
            Self::Anthropic => "agents/claude.svg",
            Self::OpenAi => "agents/openai.svg",
            Self::DeepSeek => "agents/deepseek.svg",
            Self::Google => "agents/gemini.svg",
            Self::Cursor => "agents/cursor.svg",
        }
    }

    /// The mark's ground and the mark's own colour.
    fn colors(self, cx: &App) -> (Hsla, Hsla) {
        let white: Hsla = gpui_kit::white();
        match self {
            Self::Anthropic => (rgb(0xd97757).into(), white),
            Self::DeepSeek => (rgb(0x4d6bfe).into(), white),
            Self::Google => (rgb(0x3c7bf2).into(), white),
            // Black marks, inverted on a dark theme so they never vanish
            // into it.
            Self::OpenAi | Self::Cursor => (cx.theme().foreground, cx.theme().background),
        }
    }
}

/// The person's avatar, `size` across.
pub fn person_avatar(size: Pixels, cx: &App) -> AnyElement {
    div()
        .size(size)
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(cx.theme().muted)
        .border_1()
        .border_color(cx.theme().border)
        .child(
            Icon::new(AssetIcon::UserRound)
                .size(size * 0.62)
                .text_color(cx.theme().muted_foreground),
        )
        .into_any_element()
}

/// `agent`'s avatar, `size` across.
pub fn agent_avatar(agent: &str, size: Pixels, cx: &App) -> AnyElement {
    let circle = div()
        .size(size)
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center();
    match Maker::of(agent) {
        Some(maker) => {
            let (ground, ink) = maker.colors(cx);
            circle
                .bg(ground)
                .child(svg().path(maker.mark()).size(size * 0.62).text_color(ink))
                .into_any_element()
        }
        None => circle
            .bg(crate::ui::palette::presence(cx))
            .text_color(gpui_kit::white())
            .text_size(size * 0.56)
            .font_weight(FontWeight::SEMIBOLD)
            .child(
                agent
                    .chars()
                    .next()
                    .map_or_else(|| "A".to_string(), |c| c.to_uppercase().to_string()),
            )
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::Maker;

    #[test]
    fn an_agents_name_gives_its_maker_away() {
        for (name, maker) in [
            ("Claude", Some(Maker::Anthropic)),
            ("Sonnet", Some(Maker::Anthropic)),
            ("haiku", Some(Maker::Anthropic)),
            ("Codex", Some(Maker::OpenAi)),
            ("gpt-5", Some(Maker::OpenAi)),
            ("DeepSeek-V3", Some(Maker::DeepSeek)),
            ("Gemini", Some(Maker::Google)),
            ("Cursor", Some(Maker::Cursor)),
            ("Agent", None),
        ] {
            assert_eq!(Maker::of(name), maker, "{name}");
        }
    }

    #[test]
    fn every_mark_is_served() {
        use gpui_kit::AssetSource;
        for maker in [
            Maker::Anthropic,
            Maker::OpenAi,
            Maker::DeepSeek,
            Maker::Google,
            Maker::Cursor,
        ] {
            assert!(
                matches!(crate::assets::AppAssets.load(maker.mark()), Ok(Some(_))),
                "{maker:?}"
            );
        }
    }
}
