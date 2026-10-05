//! DuckLocal's look on top of the component library's stock themes.
//!
//! The stock light theme highlights SQL in saturated Xcode-classic blue and
//! green against an otherwise grey interface, and leaves the app without an
//! accent of its own. Rather than copy the stock themes whole, this takes the
//! registered defaults and merges a few overrides into them, so every key
//! not named here keeps tracking the library:
//!
//! - syntax colours after One Light / One Dark, softer than the stock ones;
//! - DuckDB's yellow, used sparingly, for selections and the selected row;
//! - a lighter current-line band in the editor.
//!
//! `Theme::change` reapplies whichever of the two configs is current, so
//! installing them once makes every later light/dark switch use them.

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use gpui_kit::*;
use serde_json::{json, Value};

/// Replace the stock light and dark configs with DuckLocal's, and apply the
/// one for `mode`. Call once at startup, before [`crate::ui::scale::apply`].
pub fn install(mode: ThemeMode, cx: &mut App) {
    let registry = ThemeRegistry::global(cx);
    let light = customized(registry.default_light_theme(), light_overrides());
    let dark = customized(registry.default_dark_theme(), dark_overrides());
    if !cx.has_global::<Theme>() {
        Theme::change(mode, None, cx);
    }
    let theme = Theme::global_mut(cx);
    if let Some(light) = light {
        theme.light_theme = light;
    }
    if let Some(dark) = dark {
        theme.dark_theme = dark;
    }
    Theme::change(mode, None, cx);
}

/// `base` with `overrides` merged in. `None` — keep the stock theme — if the
/// library's schema ever stops round-tripping the merge.
fn customized(base: &Rc<ThemeConfig>, overrides: Value) -> Option<Rc<ThemeConfig>> {
    let mut value = serde_json::to_value(base.as_ref()).ok()?;
    merge(&mut value, overrides);
    match serde_json::from_value::<ThemeConfig>(value) {
        Ok(mut config) => {
            config.name = format!("DuckLocal {}", base.name).into();
            Some(Rc::new(config))
        }
        Err(e) => {
            tracing::warn!("Keeping the stock theme: {e}");
            None
        }
    }
}

/// Deep-merge objects; anything else in `patch` replaces what is in `target`.
fn merge(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                merge(target.entry(key).or_insert(Value::Null), value);
            }
        }
        (target, patch) => *target = patch,
    }
}

fn light_overrides() -> Value {
    json!({
        "colors": {
            "selection.background": "#fde68a",
            "list.active.background": "#fef3c7",
            "list.active.border": "#f59e0b",
            "table.active.background": "#fef3c7",
            "table.active.border": "#f59e0b",
        },
        "highlight": {
            "editor.foreground": "#383a42",
            "editor.active_line.background": "#fafafa",
            "editor.line_number": "#a3a3a3",
            "editor.active_line_number": "#404040",
            "syntax": syntax(SyntaxPalette {
                keyword: "#a626a4",
                string: "#50a14f",
                number: "#986801",
                function: "#4078f2",
                kind: "#c18401",
                comment: "#a0a1a7",
                plain: "#383a42",
                special: "#e45649",
            }),
        },
    })
}

fn dark_overrides() -> Value {
    json!({
        "colors": {
            "selection.background": "#a16207",
            "list.active.background": "#78350f66",
            "list.active.border": "#d97706",
            "table.active.background": "#78350f66",
            "table.active.border": "#d97706",
        },
        "highlight": {
            "editor.foreground": "#d4d4d4",
            "editor.active_line.background": "#141414",
            "editor.line_number": "#6b6b6b",
            "editor.active_line_number": "#d4d4d4",
            "syntax": syntax(SyntaxPalette {
                keyword: "#c678dd",
                string: "#98c379",
                number: "#d19a66",
                function: "#61afef",
                kind: "#e5c07b",
                comment: "#7f848e",
                plain: "#d4d4d4",
                special: "#e06c75",
            }),
        },
    })
}

/// The handful of roles SQL actually shows, spread over the highlighter's
/// capture names.
struct SyntaxPalette {
    keyword: &'static str,
    string: &'static str,
    number: &'static str,
    function: &'static str,
    /// Types and constructors.
    kind: &'static str,
    comment: &'static str,
    /// Identifiers, operators and punctuation.
    plain: &'static str,
    /// Escapes and special variables.
    special: &'static str,
}

fn syntax(p: SyntaxPalette) -> Value {
    let color = |c: &str| json!({ "color": c });
    json!({
        "attribute": color(p.kind),
        "boolean": color(p.number),
        "comment": { "color": p.comment, "font_style": "italic" },
        "comment.doc": { "color": p.comment, "font_style": "italic" },
        "constant": color(p.number),
        "constructor": color(p.kind),
        "embedded": color(p.plain),
        "function": color(p.function),
        "keyword": color(p.keyword),
        "number": color(p.number),
        "operator": color(p.plain),
        "property": color(p.plain),
        "punctuation": color(p.plain),
        "string": color(p.string),
        "string.escape": color(p.special),
        "string.regex": color(p.string),
        "string.special": color(p.special),
        "string.special.symbol": color(p.special),
        "tag": color(p.keyword),
        "title": color(p.keyword),
        "type": color(p.kind),
        "variable": color(p.plain),
        "variable.special": color(p.special),
    })
}

#[cfg(test)]
mod tests {
    use super::merge;
    use serde_json::json;

    #[test]
    fn merging_keeps_untouched_keys_and_replaces_named_ones() {
        let mut target = json!({ "colors": { "a": "1", "b": "2" }, "name": "x" });
        merge(&mut target, json!({ "colors": { "b": "3", "c": "4" } }));
        assert_eq!(
            target,
            json!({ "colors": { "a": "1", "b": "3", "c": "4" }, "name": "x" })
        );
    }
}
