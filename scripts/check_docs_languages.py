#!/usr/bin/env python3
"""Check multilingual README and getting-started pages.

Run from the repository root:

    python3 scripts/check_docs_languages.py

The check uses only the standard library. It confirms language navigation,
relative links, GitHub-style heading anchors, shared preview/privacy facts,
and that README.en.md stays a pointer instead of a second English homepage.
"""

from __future__ import annotations

import re
import sys
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FULL_READMES = {
    "en": ROOT / "README.md",
    "zh-Hans": ROOT / "README.zh-Hans.md",
    "zh-Hant": ROOT / "README.zh-Hant.md",
    "ja": ROOT / "README.ja.md",
}
GUIDES = {
    "en": ROOT / "docs/GETTING_STARTED.en.md",
    "zh-Hans": ROOT / "docs/GETTING_STARTED.md",
    "zh-Hant": ROOT / "docs/GETTING_STARTED.zh-Hant.md",
    "ja": ROOT / "docs/GETTING_STARTED.ja.md",
}
README_FOR_GUIDE = {
    "en": "../README.md",
    "zh-Hans": "../README.zh-Hans.md",
    "zh-Hant": "../README.zh-Hant.md",
    "ja": "../README.ja.md",
}
VISIBLE_IMAGES = [
    "docs/media/website-hero.en.jpg",
    "docs/media/website-four-pillars.jpg",
    "docs/media/memory-architecture.gif",
    "docs/media/website-memory-model.jpg",
    "docs/media/memory-layers.png",
    "docs/media/memory-network.gif",
    "docs/media/website-sovereignty.jpg",
    "docs/media/memory-control.png",
    "docs/media/website-vision.jpg",
]
COLLAPSED_IMAGES = ["docs/media/website-structure-explorer.jpg"]
ENGLISH_HEADINGS = [
    "## It started with conversations on my commute",
    "## The product",
    "## Inside the workbench",
    "## How memory structure can grow",
    "## Memory under your control",
    "## Connect the AI tools you use",
    "## Get started",
    "## The long-term vision",
    "## Build it with us",
]
SHARED = [
    "v0.1.0",
    "macOS 14",
    "Rust 1.97.1",
    "Python 3.11",
    "开始本地使用",
    "留住一段记忆",
    "Claude Code",
    "Desktop",
    "AGPL-3.0-only",
    "Powered by Atom Awareness",
    "https://nova-memory.atom-evan001.chatgpt.site/",
    "CHANGELOG.md",
    "docs/VERSIONING.md",
    "docs/media/README.md",
    "./script/build_and_run.sh",
    "local-workspace-v03",
    "https://hub.evenrealities.com/docs/get-started/architecture",
    "LICENSE_SCOPE.md",
    *VISIBLE_IMAGES,
    *COLLAPSED_IMAGES,
]
APPLE = {
    "en": "Apple Silicon",
    "zh-Hans": "Apple 芯片",
    "zh-Hant": "Apple 晶片",
    "ja": "Apple Silicon",
}
REVOCATION = {
    "en": "It does not bring back text already copied into another tool.",
    "zh-Hans": "不能把已经复制到其他工具的文本收回来。",
    "zh-Hant": "不能把已經複製到其他工具的文本收回來。",
    "ja": "すでに他のツールへコピーした文章は戻せません。",
}
CONTEXT_PACK = {
    "en": "A context pack is the one-time form of that reuse.",
    "zh-Hans": "上下文包就是这种复用的一次性形式。",
    "zh-Hant": "上下文包就是這種復用的一次性形式。",
    "ja": "文脈パックは、その再利用の一度きりの形です。",
}
GUIDE_BITS = [
    "v0.1.0",
    "macOS 14",
    "Rust 1.97.1",
    "Python 3.11",
    "开始本地使用",
    "留住一段记忆",
    "AI 整理",
    "记忆库",
    "停止",
    "运行设置 → 加密备份",
    "./script/build_and_run.sh",
    "--profile-dir",
    "--port 3060",
    "local-workspace-v03",
    "cargo fetch --locked",
]


def slug(text: str) -> str:
    kept = []
    for char in text.lower():
        if char.isspace():
            kept.append(" ")
            continue
        if char in "-_":
            kept.append(char)
            continue
        category = unicodedata.category(char)
        if category[0] in {"L", "N", "M"}:
            kept.append(char)
    return re.sub(r"\s+", "-", "".join(kept).strip())


def headings(text: str) -> list[str]:
    return [match.group(2).strip() for match in re.finditer(r"^(#{1,6})\s+(.+?)\s*$", text, re.M)]


def slug_set(text: str) -> set[str]:
    counts: dict[str, int] = {}
    found = set()
    for heading in headings(text):
        base = slug(heading)
        count = counts.get(base, 0)
        counts[base] = count + 1
        found.add(base if count == 0 else f"{base}-{count}")
    return found


def links(text: str) -> list[tuple[str, str]]:
    found = []
    start = 0
    while True:
        marker = text.find("](", start)
        if marker < 0:
            break
        depth = 0
        opener = None
        for index in range(marker, -1, -1):
            if text[index] == "]":
                depth += 1
            elif text[index] == "[":
                depth -= 1
                if depth == 0:
                    opener = index
                    break
        end = text.find(")", marker + 2)
        if opener is None or end < 0:
            start = marker + 2
            continue
        target = text[marker + 2 : end].strip().split()[0]
        is_image = opener > 0 and text[opener - 1] == "!"
        found.append(("image" if is_image else "link", target))
        start = end + 1
    for match in re.finditer(r"<img\b[^>]*\bsrc=\"([^\"]+)\"", text):
        found.append(("image", match.group(1)))
    return found


def details_spans(text: str) -> list[tuple[int, int]]:
    return [(match.start(), match.end()) for match in re.finditer(r"<details\b.*?</details>", text, re.S)]


def image_positions(text: str) -> list[tuple[str, int]]:
    positions = []
    for match in re.finditer(r"!\[[^\]]*\]\(([^)\s]+)\)", text):
        positions.append((match.group(1), match.start()))
    for match in re.finditer(r"<img\b[^>]*\bsrc=\"([^\"]+)\"", text):
        positions.append((match.group(1), match.start()))
    return positions


def inside(spans: list[tuple[int, int]], pos: int) -> bool:
    return any(start <= pos < end for start, end in spans)


def require(errors: list[str], ok: bool, message: str) -> None:
    if not ok:
        errors.append(message)


def check_relative(path: Path, text: str, errors: list[str]) -> None:
    for kind, target in links(text):
        bare = target.split("#", 1)[0].split("?", 1)[0]
        if not bare or bare.startswith(("http://", "https://", "mailto:")):
            continue
        if target.startswith("#"):
            anchor = target[1:]
            require(errors, anchor in slug_set(text), f"{path}: unresolved anchor {target}")
            continue
        destination = (path.parent / bare).resolve()
        require(errors, destination.exists(), f"{path}: missing {kind} target {target}")


def main() -> int:
    errors: list[str] = []
    version = (ROOT / "VERSION").read_text(encoding="utf-8").strip()
    version_line = f"<!-- nova-product-version --> **v{version} · Local source preview**"
    readme = (ROOT / "README.md").read_text(encoding="utf-8")
    pointer = (ROOT / "README.en.md").read_text(encoding="utf-8")
    require(errors, version_line in readme, "README.md version line does not match VERSION")
    require(errors, version_line in pointer, "README.en.md version line does not match VERSION")
    require(errors, "## It started with conversations" not in pointer, "README.en.md duplicates the English story")
    require(errors, "## The product" not in pointer, "README.en.md duplicates the English product section")
    require(errors, "](README.md)" in pointer, "README.en.md does not point at README.md")
    for heading in ENGLISH_HEADINGS:
        require(errors, heading in readme, f"README.md lost heading {heading}")

    texts = {name: path.read_text(encoding="utf-8") for name, path in FULL_READMES.items()}
    for name, text in texts.items():
        for item in SHARED:
            require(errors, item in text, f"{name} README missing {item}")
        require(errors, APPLE[name] in text, f"{name} README missing Apple Silicon wording")
        require(errors, REVOCATION[name] in text, f"{name} README missing revocation boundary")
        require(errors, CONTEXT_PACK[name] in text, f"{name} README missing context-pack explanation")
        require(errors, "简体中文" in text and "繁體中文" in text and "日本語" in text, f"{name} README missing native language labels")
        spans = details_spans(text)
        positions = image_positions(text)
        for image in VISIBLE_IMAGES:
            hits = [pos for target, pos in positions if target == image]
            require(errors, bool(hits), f"{name} README missing visible image {image}")
            require(errors, all(not inside(spans, pos) for pos in hits), f"{name} README hides {image} in details")
        for image in COLLAPSED_IMAGES:
            hits = [pos for target, pos in positions if target == image]
            require(errors, bool(hits) and all(inside(spans, pos) for pos in hits), f"{name} README moved collapsed image {image}")
        check_relative(FULL_READMES[name], text, errors)

    nav = {
        "en": ["README.zh-Hans.md", "README.zh-Hant.md", "README.ja.md", "docs/GETTING_STARTED.en.md"],
        "zh-Hans": ["README.md", "README.zh-Hant.md", "README.ja.md", "docs/GETTING_STARTED.md"],
        "zh-Hant": ["README.md", "README.zh-Hans.md", "README.ja.md", "docs/GETTING_STARTED.zh-Hant.md"],
        "ja": ["README.md", "README.zh-Hans.md", "README.zh-Hant.md", "docs/GETTING_STARTED.ja.md"],
    }
    for name, targets in nav.items():
        text = texts[name]
        for target in targets:
            require(errors, f"]({target})" in text, f"{name} README missing link to {target}")
    for target in ["README.md", "README.zh-Hans.md", "README.zh-Hant.md", "README.ja.md", "docs/GETTING_STARTED.en.md"]:
        require(errors, f"]({target})" in pointer, f"README.en.md missing link to {target}")
    check_relative(ROOT / "README.en.md", pointer, errors)

    guides = {name: path.read_text(encoding="utf-8") for name, path in GUIDES.items()}
    for name, text in guides.items():
        for item in GUIDE_BITS:
            require(errors, item in text, f"{name} guide missing {item}")
        require(errors, APPLE[name] in text or "Apple Silicon" in text or "Apple 芯片" in text or "Apple 晶片" in text, f"{name} guide missing Apple wording")
        require(errors, f"]({README_FOR_GUIDE[name]})" in text, f"{name} guide missing its README link")
        for other, path in GUIDES.items():
            if other == name:
                continue
            require(errors, path.name in text, f"{name} guide missing link to {path.name}")
        check_relative(GUIDES[name], text, errors)

    if errors:
        print(f"{len(errors)} documentation check failure(s):", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("docs language check passed")
    print(f"version {version}; full readmes {len(texts)}; guides {len(guides)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
