from __future__ import annotations

import importlib.util
import inspect
import json
import sys
from pathlib import Path
from typing import Any

from PIL import Image, ImageDraw, ImageEnhance, ImageFont


ROOT = Path("/Users/Qoo/Desktop/mywork/Lux")
LAYOUT_DIR = Path("/Users/Qoo/Desktop/layouts")
OUT_DIR = ROOT / ".tmp" / "layout-previews"
OUT_DIR.mkdir(parents=True, exist_ok=True)

CANVAS_SIZE = (1920, 1080)
FONT_PATH = "/System/Library/Fonts/Hiragino Sans GB.ttc"

POSTERS = [
    ROOT / "media/movie/2024/破浪男女 (2024) {tmdb-1029244}/poster.jpg",
    ROOT / "media/movie/2023/绿夜 (2023) {tmdb-1075813}/poster.jpg",
    ROOT / "media/movie/2021/姐妹 (2021) {tmdb-908114}/poster.jpg",
    ROOT / "media/movie/2020/人妖阿发 痴人三部曲1／3 (2020) {tmdb-675323}/poster.jpg",
    ROOT / "media/movie/2025/极斗9：龙月亮 (2025) {tmdb-760734}/poster.jpg",
    ROOT / "media/tv/日番/地狱乐 (2023)/poster.jpg",
    ROOT / "media/tv/日番/碧蓝之海 (2018)/poster.jpg",
    ROOT / "media/tv/欧美剧/黑袍纠察队 (2019)/poster.jpg",
    ROOT / "media/tv/国漫/沧元图 (2023)/poster.jpg",
]
BACKDROP = ROOT / "media/movie/2024/破浪男女 (2024) {tmdb-1029244}/fanart.jpg"


def load_module(path: Path):
    module_name = f"layout_{path.stem.encode('punycode').decode()}"
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"无法加载 {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def schema_defaults(module: Any) -> dict[str, Any]:
    config: dict[str, Any] = {}
    for group in module.get_schema():
        for item in group.get("items", []):
            if "default" in item:
                config[item["key"]] = item["default"]
    return config


def rgba_font(size: int):
    return ImageFont.truetype(FONT_PATH, max(12, int(size)))


def build_fonts(config: dict[str, Any]) -> dict[str, Any]:
    title_size = int(config.get("title_size", 140))
    subtitle_size = int(config.get("subtitle_size", config.get("sub_size", 60)))
    main = rgba_font(title_size)
    sub = rgba_font(subtitle_size)
    return {
        "main": main,
        "sub": sub,
        "font_title": main,
        "font_subtitle": sub,
        "font_sub": sub,
        "default.ttf": main,
    }


def normalize_color(color: Any):
    if isinstance(color, tuple):
        return color
    return color or "#FFFFFF"


class RenderContext:
    def __init__(self):
        self._cache: dict[str, Image.Image] = {}

    def download_img(self, url: str | Path | None):
        if not url:
            return None
        path = str(url)
        if path not in self._cache:
            try:
                with Image.open(path) as source:
                    self._cache[path] = source.convert("RGBA").copy()
            except (OSError, ValueError):
                return None
        return self._cache[path].copy()

    def create_smart_mask(self, width: int, height: int, opacity: int, coverage: int, direction: str):
        coverage = max(1, min(100, int(coverage))) / 100.0
        opacity = max(0, min(255, int(opacity)))
        mask = Image.new("L", (width, height), 0)
        pixels = mask.load()
        if direction == "vertical":
            limit = max(1, int(height * coverage))
            for y in range(height):
                alpha = int(opacity * max(0.0, 1.0 - y / limit))
                for x in range(width):
                    pixels[x, y] = alpha
        else:
            limit = max(1, int(width * coverage))
            for x in range(width):
                alpha = int(opacity * max(0.0, 1.0 - x / limit))
                for y in range(height):
                    pixels[x, y] = alpha
        return mask

    def draw_text_wrapper(self, draw, text, x, y, font, max_width, color, align="left"):
        if not text or font is None:
            return y
        if isinstance(font, (str, Path)):
            font = ImageFont.truetype(str(font), 50)
        text = str(text)
        max_width = max(1, int(max_width))
        lines: list[str] = []
        current = ""
        for char in text:
            candidate = current + char
            if current and draw.textlength(candidate, font=font) > max_width:
                lines.append(current)
                current = char
            else:
                current = candidate
        if current:
            lines.append(current)
        if not lines:
            return y
        line_height = max(1, int(font.getbbox("媒体Ag")[3] - font.getbbox("媒体Ag")[1]))
        spacing = max(4, line_height // 6)
        for index, line in enumerate(lines):
            line_width = draw.textlength(line, font=font)
            if align == "center":
                draw_x = int(x - line_width / 2)
            elif align == "right":
                draw_x = int(x - line_width)
            else:
                draw_x = int(x)
            draw.text((draw_x, int(y + index * (line_height + spacing))), line, font=font, fill=normalize_color(color))
        return int(y + len(lines) * (line_height + spacing))


def choose_config(module: Any, filename: str):
    config = schema_defaults(module)
    config.update({"title": "Lux 媒体库", "subtitle": "MOVIES  ·  SERIES  ·  ANIMATION"})
    if filename == "荣耀视界.py":
        config.update({"title": "Lux 媒体库", "subtitle": "LUX MEDIA LIBRARY"})
    if filename == "极简.py":
        config.update({"title": "Lux 媒体库", "subtitle": "精选收藏"})
    if filename == "扇形展开.py":
        config.update({"title": "Lux 媒体库", "subtitle": "COLLECTION"})
    if filename == "海报墙.py":
        config.update({"title": "Lux 媒体库", "subtitle": "ALL TITLES"})
    if filename == "聚焦C佬.py":
        config.update({"title": "Lux 媒体库", "subtitle": "FEATURED"})
    return config


def render_one(path: Path):
    module = load_module(path)
    config = choose_config(module, path.name)
    assets: dict[str, Any] = {
        "posters": [str(p) for p in POSTERS],
        "backdrops": [str(BACKDROP)],
    }

    ctx = RenderContext()
    bg = ctx.download_img(BACKDROP)
    if bg is None:
        bg = Image.new("RGBA", CANVAS_SIZE, (24, 28, 38, 255))
    bg = ImageOps.fit(bg, CANVAS_SIZE, method=Image.Resampling.LANCZOS).convert("RGBA")
    # 荣耀视界会优先读取这些可选模板素材；用本地 Lux 横幅作背景，其他缺失素材保持缺省。
    if path.name == "荣耀视界.py":
        assets["电影封面模板 V1_背景.jpg"] = bg.copy()
    fonts = build_fonts(config)
    render_fn = module.render
    if "step" in inspect.signature(render_fn).parameters:
        result = render_fn(ctx, bg, config, assets, fonts, step=0.35)
    else:
        result = render_fn(ctx, bg, config, assets, fonts)
    if result is None:
        raise RuntimeError("render() 返回 None")
    result = result.convert("RGB")
    output = OUT_DIR / f"{path.stem}.jpg"
    result.save(output, quality=92, optimize=True)
    return output, config


def make_contact_sheet(items: list[tuple[str, Path]]):
    thumb_w, thumb_h = 640, 360
    label_h = 52
    cols = 3
    rows = (len(items) + cols - 1) // cols
    sheet = Image.new("RGB", (cols * thumb_w, rows * (thumb_h + label_h)), (17, 20, 27))
    draw = ImageDraw.Draw(sheet)
    label_font = rgba_font(28)
    for index, (name, path) in enumerate(items):
        x = (index % cols) * thumb_w
        y = (index // cols) * (thumb_h + label_h)
        with Image.open(path) as source:
            thumb = ImageOps.fit(source.convert("RGB"), (thumb_w, thumb_h), method=Image.Resampling.LANCZOS)
        sheet.paste(thumb, (x, y))
        draw.rectangle((x, y + thumb_h, x + thumb_w, y + thumb_h + label_h), fill=(17, 20, 27))
        draw.text((x + 16, y + thumb_h + 9), name, font=label_font, fill=(242, 244, 248))
    contact = OUT_DIR / "全部布局预览.jpg"
    sheet.save(contact, quality=92, optimize=True)
    return contact


def main():
    outputs: list[tuple[str, Path]] = []
    errors: list[dict[str, str]] = []
    names = [
        "极简.py", "海报墙.py", "经典堆叠.py", "经典平铺.py", "经典平铺(动态).py",
        "聚焦C佬.py", "荣耀视界.py", "扇形展开.py", "斜线分割.py", "旋转.py", "旋转堆叠.py",
    ]
    for name in names:
        path = LAYOUT_DIR / name
        try:
            output, _ = render_one(path)
            outputs.append((path.stem, output))
            print(json.dumps({"layout": name, "output": str(output)}, ensure_ascii=False))
        except Exception as exc:
            errors.append({"layout": name, "error": f"{type(exc).__name__}: {exc}"})
            print(json.dumps({"layout": name, "error": errors[-1]["error"]}, ensure_ascii=False))
    if outputs:
        print(json.dumps({"contact": str(make_contact_sheet(outputs))}, ensure_ascii=False))
    if errors:
        print(json.dumps({"errors": errors}, ensure_ascii=False))
        return 1
    return 0


if __name__ == "__main__":
    from PIL import ImageOps

    raise SystemExit(main())
