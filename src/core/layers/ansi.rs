//! `ansi` layer: strips ANSI escape codes. Wraps `utils::strip_ansi`.

use super::{Layer, LayerCtx, LayerOutcome};
use crate::core::utils::strip_ansi;

pub struct AnsiLayer;

impl Layer for AnsiLayer {
    fn name(&self) -> &'static str {
        "ansi"
    }

    fn apply(&self, input: &str, _ctx: &LayerCtx) -> LayerOutcome {
        if !input.contains('\x1b') {
            return LayerOutcome::Continue(input.to_string());
        }
        LayerOutcome::Continue(strip_ansi(input))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ContentFormat, LayerCtx};
    use super::*;
    use crate::core::filter::Language;

    fn ctx() -> LayerCtx<'static> {
        LayerCtx {
            format: ContentFormat::Web,
            lang: Language::Unknown,
            source: "test",
        }
    }

    #[test]
    fn test_strips_ansi_codes() {
        let LayerOutcome::Continue(out) = AnsiLayer.apply("\x1b[32mok\x1b[0m done", &ctx()) else {
            panic!("ansi layer must continue");
        };
        assert_eq!(out, "ok done");
    }

    #[test]
    fn test_plain_text_unchanged() {
        let LayerOutcome::Continue(out) = AnsiLayer.apply("plain text", &ctx()) else {
            panic!("ansi layer must continue");
        };
        assert_eq!(out, "plain text");
    }
}
