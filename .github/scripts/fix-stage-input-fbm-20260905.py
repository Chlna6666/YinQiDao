from pathlib import Path


def replace_once(path: str, old: str, new: str, label: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected exactly one match, got {count}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


# Fix the generated apple_fluid source's Result aliases without introducing a crate-wide import.
replace_once(
    "src/gpu/apple_fluid.rs",
    "use anyhow::Result;\nuse gpui::{Context, IntoElement, Render, Timer, Window, div, prelude::*, rgb};",
    "use gpui::{Context, IntoElement, Render, Timer, Window, div, prelude::*, rgb};",
    "remove anyhow Result alias",
)
replace_once(
    "src/gpu/apple_fluid.rs",
    "pub(crate) fn apple_fluid_program() -> Result<Arc<ShaderEffectProgram>, String> {\n    static PROGRAM: OnceLock<Result<Arc<ShaderEffectProgram>, String>> = OnceLock::new();",
    "pub(crate) fn apple_fluid_program() -> std::result::Result<Arc<ShaderEffectProgram>, String> {\n    static PROGRAM: OnceLock<std::result::Result<Arc<ShaderEffectProgram>, String>> = OnceLock::new();",
    "shader program result type",
)
replace_once(
    "src/gpu/apple_fluid.rs",
    "        cx.spawn(async move |this, cx| -> Result<()> {",
    "        cx.spawn(async move |this, cx| -> anyhow::Result<()> {",
    "fluid timer result type",
)

# Re-export the new unified interactive slider from the components facade.
replace_once(
    "src/ui/components/mod.rs",
    "pub use slider::{SliderStyle, smooth_slider};",
    "pub use slider::{SliderStyle, interactive_slider, smooth_slider};",
    "interactive slider export",
)

# player_stage no longer consumes the palette itself; AppleFluidView owns that state.
replace_once(
    "src/ui/player_stage.rs",
    "    let palette = id.and_then(|id| app.artwork_palettes.get(&id));\n",
    "",
    "remove obsolete stage palette local",
)

# Once online artwork satisfies a queued fallback, consume the pending marker.
replace_once(
    "src/ui/enrichment.rs",
    "                                this.artwork_missing.remove(&track_id);\n                            }",
    "                                this.artwork_missing.remove(&track_id);\n                                this.artwork_online_fallback_requested.remove(&track_id);\n                            }",
    "consume artwork fallback marker",
)

print("post patch corrections applied")
