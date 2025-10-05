# Grim Formats Toolkit

`grim_formats` contains lightweight readers for the file formats used by
*Grim Fandango*. The focus today is on the bitmap containers that feed the
background renderer, along with the LAB archive structure that stores them.

---

## Asset Containers

Most assets ship inside LucasArts LAB archives (`DATA001.LAB`, `DATA002.LAB`,
and so on). A manifest produced by `grim_engine --asset-manifest` lists, for each
asset, the archive path and the byte range that holds it. A decoder reads that
slice directly; LAB records are not compressed.

---

## BM / ZBM Containers

Background plates and UI backdrops are shipped as `.bm` bitmaps. Matching
`.zbm` files carry per-pixel depth for the same scene. Both formats share a
0x80-byte header:

```
Offset  Size  Description
0x00    4     Magic "BM  " (0x42 0x4d 0x20 0x20)
0x04    4     Secondary magic "F\0\0\0"
0x08    4     Codec identifier (0 = raw, 3 = codec3 compression)
0x0C    4     Palette flag (unused by Grim, always 0)
0x10    4     Image count
0x14    4     Origin X (unused)
0x18    4     Origin Y (unused)
0x1C    4     Transparent color sentinel (RGB565 value 0xF81F is common)
0x20    4     Format (1 = RGB surface, 5 = depth buffer)
0x24    4     Bits per pixel (16 or 32)
0x28    0x58  Colour channel metadata (often garbage, ignored)
0x80    4     Frame width
0x84    4     Frame height
```

Each frame begins with an 8-byte dimension block (width/height) followed by raw
pixel data or a compressed payload.

### Format field

- `format == 1`  RGB surface encoded as RGB565 or little-endian BGRA32.
  Pixels equal to the sentinel `0xF81F` are interpreted as transparent by the
  original engine.
- `format == 5`  Z-buffer encoded as 16-bit unsigned depth. Value `0` maps to the
  near plane. ScummVM’s OpenGL backend uploads the data as luminance-alpha pairs
  so shaders can compare sprite depths in the fragment stage.

Other format values appear in menu assets and later titles; support is limited
to the two cases above.

### Codec 3 (LZ77-style)

Codec 3 is the dominant compression mode. It is a 4 KB LZSS window with variable
copy lengths. The decoder seeds frame *n* with the decoded pixel buffer of frame
*n − 1*. `.zbm` files expect frame 0 to be optionally primed with the pixels from
the paired `.bm` file; omitting that seed produces the "half black" artefact seen
in the Manny office capture. `decode_bm_with_seed` mirrors ScummVM’s
`decompress_codec3` implementation and exposes that seeding hook.

### Base vs. Delta bitmaps

Typical set resources include both the color plate and the matching depth map:

- `mo_0_ddtws.bm`: RGB scene plate (`format == 1`, codec 3, 16 bpp)
- `mo_0_ddtws.zbm`: Depth map (`format == 5`, codec 3, 16 bpp), same dimensions

The engine draws the RGB surface while consulting the depth buffer to decide
whether actors should be occluded by background geometry. Tooling replicates the
behaviour by decoding the `.bm`, reusing it as the codec3 seed for the `.zbm`,
and then either normalising the depth for preview purposes or forwarding the raw
16-bit buffer for rendering tests.

### Utilities

- `decode_bm` / `decode_bm_with_seed` return a `BmFile` plus its frames.
- `BmFrame::as_rgba8888` converts RGB data to RGBA and normalises depth buffers
  for inspection.
- `examples/zbm_stats.rs` prints counts, value ranges, and diffs against a base
  bitmap.

---

## LAB Archives (overview)

The `lab` module exposes a minimal reader for LAB directory headers. Entries are
stored as null-terminated strings paired with 64-bit offsets and 32-bit sizes.
LAB files simply concatenate the payloads, so feeding the byte slice into the
appropriate decoder is sufficient.

---

## Other Known Formats (to map later)

Additional binary formats appear throughout the set data:

- **`.cos` (costume files)** – describe actor skeletons, animation tracks, and
  sprite bindings. Key for rigged props like `mo_tube.cos`.
- **`.set` (set definitions)** – Lua-facing metadata describing object states,
  hooks, and runtime scripts (already partially decoded in `grim_engine`).
- **`.l3d` (mesh data)** – 3D geometry used for collision and the few polygonal
  props. The binary structure is still largely unknown.
- **`.emi` / `.lab` variants** – shared tech with *Escape from Monkey Island*.
- **Audio containers** – MIDI-like cue sheets and compressed voice files.

---

## Recommended References

- **ScummVM engine sources** (`engines/grim/*.cpp`): Authoritative reference for
  codec3, bitmap uploading, and depth handling.
- **Community tooling**:
  - *GrimEdi* (archived) and its forks – GUI viewers that inspired some of this
    work.
  - *GlIntercept* captures from legacy threads – provide ground truth images for
  color vs. depth comparisons.
- **Archive documentation**: LucasArts LAB format notes from Quick & Easy
  Software and other fan sites. Still applicable here.

Much historical documentation predates the clarification that `.zbm` files hold
depth buffers; contradictory guidance still circulates.

---

## Open Questions / Next Steps

- **Depth semantics**: The 16-bit range is scene-specific. Clarify how the
  original renderer projects the values to clip space.
- **Additional codec modes**: Document other format IDs and their usage (menus,
  cursors, Monkey4).
- **Animation support**: Bitmaps can contain multiple frames. Extend tooling to
  step through them.
- **Export tooling**: A CLI that dumps both color and depth buffers together
  would simplify regression tests.
- **Non-bitmap assets**: Costumes, meshes, audio, and dialogue scripts still
  require format notes.
