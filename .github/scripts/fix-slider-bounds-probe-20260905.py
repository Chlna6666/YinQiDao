from pathlib import Path

p = Path("src/ui/components/slider.rs")
s = p.read_text(encoding="utf-8")

old = """use gpui::{\n    App, Bounds, Div, ElementId, Empty, Global, Hsla, MouseButton, Pixels, Stateful, div, hsla,\n    prelude::*, px, relative, rgb,\n};"""
new = """use gpui::{\n    App, Bounds, Div, ElementId, Empty, Global, Hsla, MouseButton, Pixels, Stateful, canvas, div,\n    hsla, prelude::*, px, relative, rgb,\n};"""
if s.count(old) != 1:
    raise RuntimeError("slider import anchor mismatch")
s = s.replace(old, new, 1)

old = '''    slider_visual(id, ratio, style)
        .on_children_prepainted(move |children_bounds, _window, _cx| {
            // The track is the first child and spans the slider's full width. Preserve its X range
            // but use the unioned vertical interaction bounds from the root hitbox.
            if let Some(track_bounds) = children_bounds.first().copied() {
                *bounds_for_prepaint.borrow_mut() = Some(track_bounds);
            }
        })'''
new = '''    slider_visual(id, ratio, style)
        // GPUI 557f9950 does not yet expose `on_children_prepainted`. A zero-paint absolute canvas
        // participates in the same retained layout and receives the slider's exact inner bounds in
        // prepaint, so click mapping remains geometry-driven instead of reverting to window-space
        // constants. Canvas itself creates no hitbox and emits no primitive.
        .child(
            canvas(
                move |bounds, _window, _cx| {
                    *bounds_for_prepaint.borrow_mut() = Some(bounds);
                },
                |_bounds, (), _window, _cx| {},
            )
            .absolute()
            .inset_0(),
        )'''
if s.count(old) != 1:
    raise RuntimeError("on_children_prepainted anchor mismatch")
s = s.replace(old, new, 1)

p.write_text(s, encoding="utf-8")
print("slider bounds probe adapted to GPUI 557f9950 canvas prepaint")
