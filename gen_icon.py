"""Generate Setu icon: polished blue bubble with people + sync motif."""
from PIL import Image, ImageDraw, ImageFilter
import math

def draw_icon(size):
    """Draw the icon at a given size and return the Image."""
    # Work at 4x for antialiasing, then downscale
    ss = 4
    s = size * ss
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    pad = int(s * 0.06)
    radius = int(s * 0.24)

    # ── Drop shadow ──────────────────────────────────────────────
    shadow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    sd = ImageDraw.Draw(shadow)
    sh_off = int(s * 0.015)
    sd.rounded_rectangle(
        [pad + sh_off, pad + sh_off * 2, s - pad + sh_off, s - pad + sh_off * 2],
        radius=radius, fill=(0, 0, 0, 60),
    )
    shadow = shadow.filter(ImageFilter.GaussianBlur(radius=int(s * 0.025)))
    img = Image.alpha_composite(img, shadow)
    draw = ImageDraw.Draw(img)

    # ── Main bubble — gradient via layered rects ─────────────────
    # Bottom layer: deep blue
    draw.rounded_rectangle(
        [pad, pad, s - pad, s - pad],
        radius=radius, fill=(25, 90, 210),
    )
    # Mid layer: slightly lighter toward center
    inset1 = int(s * 0.02)
    draw.rounded_rectangle(
        [pad + inset1, pad + inset1, s - pad - inset1, s - pad],
        radius=radius - inset1, fill=(35, 105, 225),
    )
    # Top highlight: lighter blue on upper portion
    highlight = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    hd = ImageDraw.Draw(highlight)
    hd.rounded_rectangle(
        [pad + inset1, pad + inset1, s - pad - inset1, int(s * 0.5)],
        radius=radius - inset1, fill=(70, 150, 255, 70),
    )
    highlight = highlight.filter(ImageFilter.GaussianBlur(radius=int(s * 0.03)))
    img = Image.alpha_composite(img, highlight)
    draw = ImageDraw.Draw(img)

    # Subtle glossy edge at top
    gloss = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    gd = ImageDraw.Draw(gloss)
    gd.rounded_rectangle(
        [pad + int(s*0.08), pad + int(s*0.03), s - pad - int(s*0.08), pad + int(s*0.15)],
        radius=int(s * 0.06), fill=(255, 255, 255, 35),
    )
    gloss = gloss.filter(ImageFilter.GaussianBlur(radius=int(s * 0.02)))
    img = Image.alpha_composite(img, gloss)
    draw = ImageDraw.Draw(img)

    # ── People silhouettes ───────────────────────────────────────
    cx = s // 2
    people_cy = int(s * 0.42)

    # Left person (white, slightly larger = foreground)
    _draw_person(draw, cx - int(s * 0.15), people_cy, s,
                 head_r=int(s*0.065), body_w=int(s*0.09), body_h=int(s*0.12),
                 color=(255, 255, 255, 240))

    # Right person (lighter blue-white, slightly smaller = background)
    _draw_person(draw, cx + int(s * 0.15), people_cy, s,
                 head_r=int(s*0.058), body_w=int(s*0.08), body_h=int(s*0.11),
                 color=(190, 220, 255, 220))

    # ── Connection dots between people ───────────────────────────
    dot_y = people_cy - int(s * 0.02)
    dot_r = max(2, int(s * 0.012))
    for dx in [-int(s*0.02), 0, int(s*0.02)]:
        draw.ellipse(
            [cx + dx - dot_r, dot_y - dot_r, cx + dx + dot_r, dot_y + dot_r],
            fill=(180, 220, 255, 180),
        )

    # ── Sync arrows ──────────────────────────────────────────────
    arrow_cy = int(s * 0.74)
    arrow_r = int(s * 0.10)
    _draw_sync_arrows(draw, img, cx, arrow_cy, arrow_r, s)

    # ── Downscale with high-quality resampling ───────────────────
    img = img.resize((size, size), Image.LANCZOS)
    return img


def _draw_person(draw, cx, cy, s, head_r, body_w, body_h, color):
    """Draw a person silhouette with smooth shapes."""
    # Head
    head_y = cy - int(s * 0.10)
    draw.ellipse(
        [cx - head_r, head_y - head_r, cx + head_r, head_y + head_r],
        fill=color,
    )
    # Neck
    neck_w = max(2, int(head_r * 0.5))
    neck_top = head_y + head_r - max(1, int(s * 0.005))
    neck_bot = neck_top + max(2, int(s * 0.025))
    draw.rectangle(
        [cx - neck_w, neck_top, cx + neck_w, neck_bot],
        fill=color,
    )
    # Body (rounded shoulders)
    body_top = neck_bot - max(1, int(s * 0.005))
    draw.rounded_rectangle(
        [cx - body_w, body_top, cx + body_w, body_top + body_h],
        radius=int(body_w * 0.5),
        fill=color,
    )


def _draw_sync_arrows(draw, img, cx, cy, r, s):
    """Draw two curved sync arrows with proper arrowheads."""
    thickness = max(2, int(s * 0.022))
    arc_color = (200, 235, 255, 230)
    arrow_color = (220, 245, 255, 250)

    # Draw arcs on a temporary layer for smoothness
    arc_layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
    arc_draw = ImageDraw.Draw(arc_layer)

    arc_box = [cx - r, cy - r, cx + r, cy + r]

    # Top arc (clockwise, left→right)
    arc_draw.arc(arc_box, start=210, end=330, fill=arc_color, width=thickness)
    # Bottom arc (clockwise, right→left)
    arc_draw.arc(arc_box, start=30, end=150, fill=arc_color, width=thickness)

    # Composite arcs
    combined = Image.alpha_composite(img, arc_layer)
    img.paste(combined, (0, 0))

    # Re-acquire draw on main img
    draw = ImageDraw.Draw(img)

    # Arrowheads as triangles
    arrow_sz = int(s * 0.035)

    # Right arrowhead (tip of top arc at ~330°)
    a1_angle = math.radians(-30)
    a1x = cx + int(r * math.cos(a1_angle))
    a1y = cy + int(r * math.sin(a1_angle))
    draw.polygon([
        (a1x + arrow_sz, a1y),
        (a1x - int(arrow_sz * 0.3), a1y - arrow_sz),
        (a1x - int(arrow_sz * 0.3), a1y + int(arrow_sz * 0.4)),
    ], fill=arrow_color)

    # Left arrowhead (tip of bottom arc at ~150°)
    a2_angle = math.radians(150)
    a2x = cx + int(r * math.cos(a2_angle))
    a2y = cy + int(r * math.sin(a2_angle))
    draw.polygon([
        (a2x - arrow_sz, a2y),
        (a2x + int(arrow_sz * 0.3), a2y + arrow_sz),
        (a2x + int(arrow_sz * 0.3), a2y - int(arrow_sz * 0.4)),
    ], fill=arrow_color)


def main():
    sizes = [256, 128, 64, 48, 32, 16]
    images = [draw_icon(sz) for sz in sizes]

    ico_path = "assets/icon.ico"
    images[0].save(
        ico_path,
        format="ICO",
        sizes=[(sz, sz) for sz in sizes],
        append_images=images[1:],
    )
    print(f"Saved {ico_path} with sizes: {sizes}")

    png_path = "assets/setu.png"
    images[0].save(png_path)
    print(f"Saved {png_path} (256x256 PNG)")

    # Save 32x32 raw RGBA for embedding in the settings/tray window icon
    icon_32 = draw_icon(32)
    rgba_path = "assets/icon_32x32.rgba"
    with open(rgba_path, "wb") as f:
        f.write(icon_32.tobytes("raw", "RGBA"))
    print(f"Saved {rgba_path} (32x32 raw RGBA, {32*32*4} bytes)")


if __name__ == "__main__":
    main()
