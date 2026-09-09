"""Bake WeaponGuy's CC0 "Human Head 250 verts" into YinQiDao's static GPUI mesh.

Usage:
    blender --background /path/to/nicehumanmodel.blend \
        --python tools/audio_debug/export_humanoid_cc0.py -- \
        src/ui/audio_debug_humanoid_generated.rs

Source asset:
    Human Head 250 verts
    Author: WeaponGuy
    License: CC0
    Original file: nicehumanmodel.blend
    Layer 1: low-poly head with eyes (not subsurfed)
    https://opengameart.org/content/human-head-250-verts

Only the visible Layer-1 low-poly head is accepted. The exporter deliberately reads the original
mesh data instead of evaluated modifier output so a Subdivision Surface modifier cannot silently
turn the ~250-vertex debug asset into a high-poly runtime mesh.

The generated Rust file is static geometry. YinQiDao never needs Blender, file I/O, glTF/OBJ parsing,
or runtime mesh decoding in the debug renderer. DSP ear positions remain owned exclusively by
ListenerPose::ear_positions(); this asset is visual only.
"""

from __future__ import annotations

import pathlib
import sys

import bpy

TARGET_HEAD_HEIGHT_METERS = 0.215
HEAD_CENTER_FROM_BOTTOM_FRACTION = 0.48
MAX_SOURCE_VERTICES = 1200
MAX_HEAD_ASPECT = 2.20


def output_path() -> pathlib.Path:
    args = sys.argv
    if "--" not in args:
        raise SystemExit("missing '-- <output.rs>' argument")
    rest = args[args.index("--") + 1 :]
    if len(rest) != 1:
        raise SystemExit("expected exactly one output Rust path")
    return pathlib.Path(rest[0]).resolve()


def is_layer_one_visible_mesh(obj: bpy.types.Object) -> bool:
    if obj.type != "MESH" or obj.hide_render:
        return False

    # Blender <=2.79 stored the source file's 20 scene layers directly on each object. When that
    # API is present, select only Layer 1 exactly as documented by the OpenGameArt asset.
    legacy_layers = getattr(obj, "layers", None)
    if legacy_layers is not None and len(legacy_layers) > 0 and not legacy_layers[0]:
        return False

    # Blender 2.80+ migrates legacy layers to collection/view-layer visibility. Respect that state
    # rather than exporting hidden Layer-2 body / Layer-3 high-poly reference geometry.
    try:
        if not obj.visible_get():
            return False
    except (AttributeError, RuntimeError):
        pass
    return True


def collect_mesh() -> tuple[list[tuple[float, float, float]], list[int]]:
    vertices: list[tuple[float, float, float]] = []
    indices: list[int] = []

    for obj in bpy.context.scene.objects:
        if not is_layer_one_visible_mesh(obj):
            continue

        # Use base mesh data, not depsgraph/evaluated mesh. The source explicitly identifies the
        # desired head as "not subsurfed" and the runtime debug asset should stay low-poly.
        mesh = obj.data
        mesh.calc_loop_triangles()
        base = len(vertices)
        transform = obj.matrix_world
        for vertex in mesh.vertices:
            co = transform @ vertex.co
            vertices.append((float(co.x), float(co.y), float(co.z)))
        for triangle in mesh.loop_triangles:
            indices.extend(base + int(index) for index in triangle.vertices)

    if not vertices or not indices:
        raise SystemExit("no visible Layer-1 mesh geometry found in the Blender scene")
    if len(vertices) > MAX_SOURCE_VERTICES:
        raise SystemExit(
            f"refusing {len(vertices)} source vertices: Layer 2/3 or a subdivided mesh is likely visible"
        )
    return vertices, indices


def normalize(vertices: list[tuple[float, float, float]]) -> list[tuple[float, float, float]]:
    min_x = min(v[0] for v in vertices)
    max_x = max(v[0] for v in vertices)
    min_y = min(v[1] for v in vertices)
    max_y = max(v[1] for v in vertices)
    min_z = min(v[2] for v in vertices)
    max_z = max(v[2] for v in vertices)

    width = max_x - min_x
    depth = max_y - min_y
    height = max_z - min_z
    horizontal = max(width, depth)
    if height <= 1.0e-6 or horizontal <= 1.0e-6:
        raise SystemExit("head mesh has zero/invalid bounds")
    if height / horizontal > MAX_HEAD_ASPECT:
        raise SystemExit(
            "visible geometry is too tall for a head; hide Layer 2 body and Layer 3 reference mesh"
        )

    scale = TARGET_HEAD_HEIGHT_METERS / height
    center_x = (min_x + max_x) * 0.5
    center_y = (min_y + max_y) * 0.5
    acoustic_center_z = min_z + height * HEAD_CENTER_FROM_BOTTOM_FRACTION

    result: list[tuple[float, float, float]] = []
    for x, y, z in vertices:
        # Blender: X right, Y depth, Z up. YinQiDao: X right, Y up, Z forward. The common Blender
        # character convention faces -Y, hence the sign flip for engine Z.
        engine_x = (x - center_x) * scale
        engine_y = (z - acoustic_center_z) * scale
        engine_z = -(y - center_y) * scale
        result.append((engine_x, engine_y, engine_z))
    return result


def write_rust(path: pathlib.Path, vertices: list[tuple[float, float, float]], indices: list[int]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as out:
        out.write("// @generated by tools/audio_debug/export_humanoid_cc0.py\n")
        out.write("// Source: WeaponGuy, Human Head 250 verts, CC0, nicehumanmodel.blend Layer 1.\n\n")
        out.write("pub(crate) const HUMANOID_ASSET_READY: bool = true;\n")
        out.write("pub(crate) const HUMANOID_VERTICES: &[[f32; 3]] = &[\n")
        for x, y, z in vertices:
            out.write(f"    [{x:.8f}, {y:.8f}, {z:.8f}],\n")
        out.write("];\n")
        out.write("pub(crate) const HUMANOID_INDICES: &[u32] = &[\n")
        for offset in range(0, len(indices), 18):
            chunk = indices[offset : offset + 18]
            out.write("    " + ", ".join(str(index) for index in chunk) + ",\n")
        out.write("];\n")


def main() -> None:
    path = output_path()
    vertices, indices = collect_mesh()
    vertices = normalize(vertices)
    write_rust(path, vertices, indices)
    print(f"wrote {len(vertices)} vertices / {len(indices) // 3} triangles -> {path}")


if __name__ == "__main__":
    main()
