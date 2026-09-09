"""Generate AAG-branded icon set in all Tauri-required formats.

Output files (matching the bundle.icon array in tauri.conf.json):
  - 32x32.png         : 32x32 PNG
  - 128x128.png       : 128x128 PNG
  - 128x128@2x.png    : 256x256 PNG (the @2x variant)
  - icon.png          : 512x512 PNG (the canonical source)
  - icon.ico          : multi-resolution Windows .ico
  - icon.icns         : macOS .icns (built via png2icns if available)

Design: dark navy background + cyan rounded "A" mark. Matches the app's
existing dark theme (#0f1115 bg, #4a90e2 / #6fa8ff cyan accents).
"""
import os
import subprocess
import sys
from PIL import Image, ImageDraw, ImageFont

OUT_DIR = os.path.join(os.path.dirname(__file__), "icons")
BG = (15, 17, 21, 255)            # matches --bg: #0f1115
ACCENT = (74, 144, 226, 255)      # matches #4a90e2 (focus ring)
ACCENT_BRIGHT = (111, 168, 255, 255)  # #6fa8ff (chip text)
TEXT_FG = (230, 230, 230, 255)    # #e6e6e6


def find_font(size: int) -> ImageFont.FreeTypeFont:
    """Find a bold system font, fall back to PIL default."""
    candidates = [
        "C:/Windows/Fonts/segoeuib.ttf",
        "C:/Windows/Fonts/arialbd.ttf",
        "C:/Windows/Fonts/arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    ]
    for path in candidates:
        if os.path.exists(path):
            try:
                return ImageFont.truetype(path, size)
            except Exception:
                continue
    return ImageFont.load_default()


def draw_icon(size: int, with_text: bool = True) -> Image.Image:
    img = Image.new("RGBA", (size, size), BG)
    draw = ImageDraw.Draw(img)

    # Rounded square background accent (subtle border).
    pad = max(1, size // 32)
    radius = max(2, size // 6)
    border_color = ACCENT
    draw.rounded_rectangle(
        [(pad, pad), (size - 1 - pad, size - 1 - pad)],
        radius=radius,
        outline=border_color,
        width=max(1, size // 16),
    )

    if with_text:
        # Big "A" centered. Use a font size proportional to icon size.
        font_size = int(size * 0.6)
        font = find_font(font_size)
        text = "A"
        # Measure text to center precisely.
        bbox = draw.textbbox((0, 0), text, font=font)
        text_w = bbox[2] - bbox[0]
        text_h = bbox[3] - bbox[1]
        x = (size - text_w) // 2 - bbox[0]
        y = (size - text_h) // 2 - bbox[1]
        # Shadow for contrast.
        draw.text((x + max(1, size // 64), y + max(1, size // 64)),
                  text, font=font, fill=(0, 0, 0, 200))
        draw.text((x, y), text, font=font, fill=TEXT_FG)

    return img


def main() -> int:
    os.makedirs(OUT_DIR, exist_ok=True)

    # 1) PNG variants.
    targets = [
        ("32x32.png", 32, False),
        ("128x128.png", 128, False),
        ("128x128@2x.png", 256, False),
        ("icon.png", 512, False),
    ]
    for name, size, _ in targets:
        path = os.path.join(OUT_DIR, name)
        img = draw_icon(size)
        img.save(path, "PNG")
        print(f"wrote {path} ({size}x{size})")

    # 2) icon.ico — multi-resolution Windows icon.
    ico_path = os.path.join(OUT_DIR, "icon.ico")
    sizes = [16, 32, 48, 64, 128, 256]
    imgs = [draw_icon(s) for s in sizes]
    imgs[0].save(ico_path, format="ICO",
                 sizes=[(s, s) for s in sizes],
                 append_images=imgs[1:])
    print(f"wrote {ico_path} (ICO {sizes})")

    # 3) icon.icns — macOS icon set. Tauri-build actually requires this even
    # on Windows (it's referenced in tauri.conf.json). PIL doesn't natively
    # write .icns, so try png2icns / iconutil / ImageMagick. Fall back to
    # embedding a 512x512 PNG under .icns name (Tauri's bundler may not
    # actually use the file on non-mac builds).
    icns_path = os.path.join(OUT_DIR, "icon.icns")
    written = False
    for tool in ("png2icns", "iconutil"):
        try:
            if tool == "iconutil":
                # macOS-only; would need icns source directory. Skip on Windows.
                continue
            cmd = [tool, icns_path,
                   os.path.join(OUT_DIR, "icon.png")]
            subprocess.run(cmd, check=True, capture_output=True)
            written = True
            print(f"wrote {icns_path} via {tool}")
            break
        except (FileNotFoundError, subprocess.CalledProcessError):
            continue

    if not written:
        # Fallback: copy the largest PNG with .icns extension. Tauri's bundler
        # only reads .icns on macOS, so on Windows this is benign.
        import shutil
        shutil.copyfile(os.path.join(OUT_DIR, "icon.png"), icns_path)
        print(f"wrote {icns_path} as PNG fallback (no icns tool available)")

    return 0


if __name__ == "__main__":
    sys.exit(main())