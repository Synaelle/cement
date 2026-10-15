"""Generates the Syna 3DS Tools artwork (red tile with a cute white S).

usage: python tools/make_logo.py   (from the repo root)
writes server/meta/icon.png, server/meta/banner.png, client/src/icon.ico,
tools/logo.png
"""
import os

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FONT = "C:/Windows/Fonts/seguibl.ttf"  # Segoe UI Black
RED_TOP, RED_BOTTOM = (255, 82, 82), (211, 32, 47)


def gradient(size, top, bottom):
    w, h = size
    g = Image.new("RGB", (1, h))
    for y in range(h):
        t = y / max(h - 1, 1)
        g.putpixel((0, y), tuple(int(a + (b - a) * t) for a, b in zip(top, bottom)))
    return g.resize((w, h))


def rounded_mask(size, radius):
    m = Image.new("L", size, 0)
    ImageDraw.Draw(m).rounded_rectangle((0, 0, size[0] - 1, size[1] - 1), radius, fill=255)
    return m


def cute_s(size):
    """White bubbly S with a little face, as an RGBA layer of size x size."""
    big = size * 2
    glyph = Image.new("L", (big, big), 0)
    d = ImageDraw.Draw(glyph)
    font = ImageFont.truetype(FONT, int(big * 0.78))
    l, t, r, b = d.textbbox((0, 0), "S", font=font)
    d.text(((big - (r - l)) / 2 - l, (big - (b - t)) / 2 - t), "S", font=font, fill=255)
    # puff it up and round every corner: dilate, blur, threshold
    glyph = glyph.filter(ImageFilter.MaxFilter(int(big * 0.02) | 1))
    glyph = glyph.filter(ImageFilter.GaussianBlur(big * 0.03))
    glyph = glyph.point(lambda v: 255 if v > 110 else 0)
    glyph = glyph.filter(ImageFilter.GaussianBlur(big * 0.004))

    layer = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    shadow = Image.new("RGBA", (big, big), (120, 0, 20, 110))
    shadow_mask = glyph.filter(ImageFilter.GaussianBlur(big * 0.02))
    layer.paste(shadow, (0, int(big * 0.025)), shadow_mask)
    layer.paste(Image.new("RGBA", (big, big), (255, 255, 255, 255)), (0, 0), glyph)

    # face on the middle stroke of the S
    d = ImageDraw.Draw(layer)
    cx, cy = big / 2, big * 0.5
    face = 0.25  # face size relative to the first version
    eye_r, eye_dx = big * 0.03 * face, big * 0.09 * face
    for sx in (-1, 1):
        ex = cx + sx * eye_dx
        d.ellipse((ex - eye_r, cy - eye_r * 1.25, ex + eye_r, cy + eye_r * 1.25), fill=(60, 20, 30, 255))
        d.ellipse((ex - eye_r * 0.35, cy - eye_r * 0.9, ex + eye_r * 0.15, cy - eye_r * 0.3), fill=(255, 255, 255, 255))
        bx = cx + sx * eye_dx * 1.75
        d.ellipse((bx - eye_r * 1.1, cy + eye_r * 1.0, bx + eye_r * 1.1, cy + eye_r * 1.8), fill=(255, 140, 160, 200))
    d.arc((cx - eye_r * 1.3, cy - eye_r * 0.2, cx + eye_r * 1.3, cy + eye_r * 1.9), 20, 160, fill=(60, 20, 30, 255), width=max(2, int(big * 0.008 * face)))
    return layer.resize((size, size), Image.LANCZOS)


def tile(size):
    s = size * 4
    base = gradient((s, s), RED_TOP, RED_BOTTOM).convert("RGBA")
    # soft top highlight
    hl = Image.new("L", (s, s), 0)
    ImageDraw.Draw(hl).ellipse((-s * 0.3, -s * 0.75, s * 1.3, s * 0.42), fill=55)
    base = Image.composite(Image.new("RGBA", (s, s), (255, 255, 255, 255)), base, hl)
    base.alpha_composite(cute_s(s))
    out = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    out.paste(base, (0, 0), rounded_mask((s, s), int(s * 0.22)))
    return out.resize((size, size), Image.LANCZOS)


def banner():
    w, h = 256, 128
    img = gradient((w, h), (40, 40, 44), (18, 18, 20)).convert("RGBA")
    logo = tile(84)
    img.alpha_composite(logo, (14, (h - 84) // 2))
    d = ImageDraw.Draw(img)
    big = ImageFont.truetype(FONT, 30)
    small = ImageFont.truetype("C:/Windows/Fonts/segoeuib.ttf", 17)
    d.text((108, 30), "Syna", font=big, fill=(255, 255, 255))
    d.text((108, 66), "3DS Tools", font=small, fill=(255, 110, 110))
    return img.convert("RGB")


def main():
    tile(48).save(os.path.join(ROOT, "server", "meta", "icon.png"))
    banner().save(os.path.join(ROOT, "server", "meta", "banner.png"))
    tile(256).save(os.path.join(ROOT, "client", "src", "icon.ico"),
                   sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    tile(512).save(os.path.join(ROOT, "tools", "logo.png"))
    print("artwork written")


if __name__ == "__main__":
    main()
