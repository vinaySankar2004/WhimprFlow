#!/usr/bin/env python3
"""Render the iOS app icon.

The icon is generated rather than hand-exported so its geometry and colours are
reviewable in a diff, and so it stays tied to the palette in
`ui/src/tokens/values.ts` rather than drifting into a third opinion about the brand.

It is a redraw of `src-tauri/icons/icon.png`, not a copy of it, because that file is
unusable as an iOS app icon in two ways that both fail late:

1. **It has an alpha channel.** App Store Connect rejects app icons with
   transparency at upload validation, after the archive and the wait.
2. **Its rounded corners are baked in**, with transparent margins around them. iOS
   applies its own mask, so a pre-rounded icon renders as a small rounded square
   inset inside the system's rounded square.

So this draws the same concentric-rings mark full-bleed on an opaque ground, and the
system does the rounding.

    python3 scripts/make-ios-icon.py

Writes the 1024×1024 icon into the asset catalog. Xcode derives every smaller size
from it (iOS 17+ single-size app icons), so there is only ever one file to keep.
"""

from pathlib import Path

from PIL import Image, ImageDraw

# From ui/src/tokens/values.ts — slate950 ground, the accent for the ring, and the
# brighter accent400 for the centre.
GROUND = (0x0C, 0x0E, 0x12)
RING = (0x22, 0xC3, 0xB6)
CENTRE = (0x3F, 0xE0, 0xD0)

SIZE = 1024
# Drawn at 4× and downsampled: PIL's ellipse has no anti-aliasing, and at 1024 the
# ring edges are visibly stepped without this.
SCALE = 4

# Proportions of the original mark, rescaled so the artwork fills the canvas instead
# of sitting inside a baked-in rounded tile.
RING_OUTER = 0.385  # of the canvas width
RING_INNER = 0.285
CENTRE_RADIUS = 0.175

OUT = Path(__file__).resolve().parent.parent / (
    "ios/WhimprFlow/Assets.xcassets/AppIcon.appiconset/AppIcon-1024.png"
)


def circle(draw: ImageDraw.ImageDraw, centre: float, radius: float, colour) -> None:
    draw.ellipse(
        [centre - radius, centre - radius, centre + radius, centre + radius],
        fill=colour,
    )


def main() -> None:
    canvas = SIZE * SCALE
    # "RGB", not "RGBA": the absence of an alpha channel is the point.
    image = Image.new("RGB", (canvas, canvas), GROUND)
    draw = ImageDraw.Draw(image)

    centre = canvas / 2
    # The ring is drawn as a filled disc with the ground punched back out of it,
    # which keeps the two edges concentric by construction.
    circle(draw, centre, canvas * RING_OUTER, RING)
    circle(draw, centre, canvas * RING_INNER, GROUND)
    circle(draw, centre, canvas * CENTRE_RADIUS, CENTRE)

    image = image.resize((SIZE, SIZE), Image.LANCZOS)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    image.save(OUT, "PNG")
    print(f"wrote {OUT.relative_to(OUT.parents[4])} ({SIZE}x{SIZE}, no alpha)")


if __name__ == "__main__":
    main()
