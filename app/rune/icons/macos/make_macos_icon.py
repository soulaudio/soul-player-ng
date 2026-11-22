import sys
from PIL import Image, ImageDraw, ImageFilter
src, out = sys.argv[1], sys.argv[2]
N = 1024
PLATE, OFF, R = 824, 100, 185          # Apple macOS icon grid (1024 canvas)
mark = Image.open(src).convert("RGBA")
# Ink-only: alpha from darkness (the source has an opaque white backdrop), colour black.
from PIL import ImageChops, ImageOps
lum = mark.convert("L")
a = ImageChops.multiply(ImageOps.invert(lum), mark.getchannel("A"))
mark = Image.new("RGBA", mark.size, (12, 12, 14, 255)); mark.putalpha(a)
canvas = Image.new("RGBA", (N, N), (0, 0, 0, 0))
# soft drop shadow under the plate
sh = Image.new("RGBA", (N, N), (0, 0, 0, 0))
ImageDraw.Draw(sh).rounded_rectangle([OFF, OFF + 12, OFF + PLATE, OFF + PLATE + 12], R, fill=(0, 0, 0, 90))
canvas.alpha_composite(sh.filter(ImageFilter.GaussianBlur(14)))
# plate: off-white with a gentle top-to-bottom gradient
grad = Image.new("RGBA", (PLATE, PLATE))
for y in range(PLATE):
    v = int(252 - 14 * y / PLATE)
    ImageDraw.Draw(grad).line([(0, y), (PLATE, y)], fill=(v, v, v + 1, 255))
mask = Image.new("L", (PLATE, PLATE), 0)
ImageDraw.Draw(mask).rounded_rectangle([0, 0, PLATE - 1, PLATE - 1], R, fill=255)
canvas.paste(grad, (OFF, OFF), mask)
# the mark, centered on the plate at ~78% of its width
m = int(PLATE * 0.74)
mk = mark.resize((m, m), Image.LANCZOS)
canvas.alpha_composite(mk, (OFF + (PLATE - m) // 2, OFF + (PLATE - m) // 2))
canvas.save(out)
print("wrote", out)
