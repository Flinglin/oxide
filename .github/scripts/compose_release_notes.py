#!/usr/bin/env python3
"""Compose release notes from channel-specific evergreen text and one changelog entry."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from urllib.parse import quote


CHANGELOG_MARKER = "<!-- RELEASE_CHANGELOG -->"
DOWNLOADS_MARKER = "<!-- RELEASE_DOWNLOADS -->"
REPOSITORY_RELEASE_URL = "https://github.com/Flinglin/oxide/releases/download"


def normalize_leading_summary(section: str) -> str:
    """Join soft-wrapped lines in the opening prose paragraph."""
    lines = section.splitlines()
    if not lines:
        return section

    paragraph_end = next(
        (index for index, line in enumerate(lines) if not line.strip()),
        len(lines),
    )
    paragraph = lines[:paragraph_end]
    if not paragraph:
        return section

    # Preserve Markdown blocks when a channel intentionally starts with one.
    if any(
        re.match(r"^(?:#{1,6}\s|[-*+]\s|>\s?|```|~~~|\d+[.)]\s|\||<)", line.lstrip())
        for line in paragraph
    ):
        return section

    summary = " ".join(line.strip() for line in paragraph)
    remainder = lines[paragraph_end:]
    return "\n".join([summary, *remainder])


def release_asset_url(tag: str, filename: str) -> str:
    """Build a URL for an asset attached to the release being composed."""
    return f"{REPOSITORY_RELEASE_URL}/{quote(tag, safe='')}/{quote(filename, safe='')}"


def stable_download_table(version: str, tag: str) -> str:
    """Render the recommended installer matrix without listing portable variants."""
    
    # 1. 修复：将 link 函数提前，直接传入完整文件名
    def link(label: str, filename: str) -> str:
        return f"[{label}]({release_asset_url(tag, filename)})"

    # 2. 定义二维数据结构，预设好所有可能的系统和架构，确保输出顺序固定
    table_data = {
        "Windows": {"x64": [], "ARM64": []},
        "macOS": {"x64": [], "ARM64": []},
        "Linux": {"x64": [], "ARM64": []}
    }

    # 3. 根据 tag 填充数据
    if "oxide-v" in tag:
        # Windows x64
        table_data["Windows"]["x64"].append(link("Setup (.exe)", f"oxide_{version}_windows_x64-setup.exe"))
        table_data["Windows"]["x64"].append(link("Portable (.zip)", f"oxide_{version}_windows_x64-portable.zip")) # 修复了拼写错误和假定后缀
        # Windows ARM64 (如果有的话，预留)
        table_data["Windows"]["ARM64"].append(link("Setup (.exe)", f"oxide_{version}_windows_arm64-setup.exe"))
        
        # macOS (根据截图添加)
        table_data["macOS"]["x64"].append(link("DMG (Intel)", f"oxide_{version}_macos_x64.dmg"))
        table_data["macOS"]["ARM64"].append(link("DMG (Apple Silicon)", f"oxide_{version}_macos_aarch64.dmg"))

        # Linux (根据截图添加，如果你的主程序有这些文件)
        table_data["Linux"]["x64"].append(link("AppImage", f"oxide_{version}_linux_x64.AppImage"))
        table_data["Linux"]["x64"].append(link("DEB", f"oxide_{version}_linux_amd64.deb"))
        table_data["Linux"]["x64"].append(link("RPM", f"oxide_{version}_linux_x86_64.rpm"))
        
        table_data["Linux"]["ARM64"].append(link("AppImage", f"oxide_{version}_linux_aarch64.AppImage"))
        table_data["Linux"]["ARM64"].append(link("DEB", f"oxide_{version}_linux_arm64.deb"))
        table_data["Linux"]["ARM64"].append(link("RPM", f"oxide_{version}_linux_aarch64.rpm"))

    if "oxide-agent-v" in tag:
        # Agent 端的 Linux 链接
        table_data["Linux"]["x64"].append(link("Setup", f"oxide-agent-x86_64-linux-gnu"))
        table_data["Linux"]["ARM64"].append(link("Setup", f"oxide-agent-aarch64-linux-gnu"))

    # 4. 动态生成表格的行
    table_rows = []
    for os_name, archs in table_data.items():
        # 用 " · " 连接同一个系统同一架构下的多个文件（例如 AppImage · DEB · RPM）
        x64_str = " · ".join(archs["x64"]) if archs["x64"] else "-"
        arm64_str = " · ".join(archs["ARM64"]) if archs["ARM64"] else "-"
        
        # 如果某个系统既没有 x64 也没有 ARM64 的文件，就不生成这一行
        if x64_str == "-" and arm64_str == "-":
            continue
            
        table_rows.append(f"| **{os_name}** | {x64_str} | {arm64_str} |")

    # 5. 返回最终的 Markdown 字符串
    return "\n".join(
        [
            "## 📥 Download for your system",
            "",
            "| Operating system | x64 | ARM64 |",
            "| :--- | :--- | :--- |",
            *table_rows, # 使用 * 解包，把生成的行展开插入到列表中
            "",
            "Portable archives, signatures, and `sha256sums.txt` remain available in the release assets below.",
        ]
    )


def extract_version_section(
    changelog: str, version: str, *, include_heading: bool = True
) -> str:
    """Return one version entry, optionally retaining its extraction heading."""
    pattern = re.compile(
        rf"^## {re.escape(version)}\n(?P<section>.*?)(?=^## |\Z)",
        re.MULTILINE | re.DOTALL,
    )
    match = pattern.search(changelog)
    if match is None:
        raise ValueError(f"no changelog section found for {version}")
    section = normalize_leading_summary(match.group("section").strip())
    if include_heading:
        return f"## {version}\n\n{section}"
    return section


def compose_notes(version: str, tag: str, base_path: Path, changelog_path: Path) -> str:
    base = base_path.read_text(encoding="utf-8")
    if CHANGELOG_MARKER not in base:
        raise ValueError(f"{base_path} is missing {CHANGELOG_MARKER}")

    changelog = changelog_path.read_text(encoding="utf-8")
    # Stable releases already show the version in GitHub's release title, so their
    # body omits the version heading while other channels keep it.
    is_stable_release = DOWNLOADS_MARKER in base
    section = extract_version_section(
        changelog, version, include_heading=not is_stable_release
    )
    language_blocks = re.split(r"^### (中文|English)\n", section, flags=re.MULTILINE)
    bilingual = len(language_blocks) == 5 and set(language_blocks[1::2]) == {
        "中文", "English"
    }
    if bilingual:
        blocks = dict(zip(language_blocks[1::2], language_blocks[2::2]))
        section = language_blocks[0] + "\n\n".join(
            f"### {language}\n\n{blocks[language].strip()}"
            for language in ("中文", "English")
        )
    notes = base.replace(CHANGELOG_MARKER, section)
    if DOWNLOADS_MARKER in notes:
        notes = notes.replace(DOWNLOADS_MARKER, stable_download_table(version, tag))
    if bilingual:
        notes = "[中文](#中文) | [English](#english)\n\n" + notes
    return notes.rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--template", required=True, type=Path)
    parser.add_argument("--changelog", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    try:
        notes = compose_notes(args.version, args.tag, args.template, args.changelog)
    except Exception as error:
        print(f"failed to compose release notes: {error}", file=sys.stderr)
        return 1

    args.output.write_text(notes, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
