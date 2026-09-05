from pathlib import Path

p = Path("src/ui/components/slider.rs")
s = p.read_text(encoding="utf-8")
old = '''        .on_drag_move::<SliderDrag>(move |event, _window, cx| {
            let drag = event.drag(cx);
            if drag.id != id_for_drag {
                return;
            }
            let ratio = ratio_from_position(event.event.position.x, event.bounds, drag.thumb_size);
            (drag.on_change)(ratio, cx);
        })'''
new = '''        .on_drag_move::<SliderDrag>(move |event, _window, cx| {
            let (thumb_size, on_change) = {
                let drag = event.drag(cx);
                if drag.id != id_for_drag {
                    return;
                }
                (drag.thumb_size, drag.on_change.clone())
            };
            let ratio = ratio_from_position(event.event.position.x, event.bounds, thumb_size);
            (on_change)(ratio, cx);
        })'''
if s.count(old) != 1:
    raise RuntimeError("slider drag borrow anchor mismatch")
p.write_text(s.replace(old, new, 1), encoding="utf-8")
print("slider drag borrow fixed")
