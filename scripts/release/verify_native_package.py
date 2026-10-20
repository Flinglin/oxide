#!/usr/bin/env python3
"""Verify native release artifacts before they are uploaded."""

from __future__ import annotations

import argparse
import json
import plistlib
import re
import shutil
import subprocess
import tarfile
import tempfile
import zipfile
from pathlib import Path

from conpty_runtime import runtime_files as conpty_runtime_files, verify_digest


REQUIRED_DOCUMENTS = {
    "LICENSE",
}
LINUX_DEB_GRAPHICS_RECOMMENDS = {"libegl1", "libvulkan1"}
LINUX_RPM_GRAPHICS_RECOMMENDS = {"libglvnd-egl", "vulkan-loader"}
LINUX_GLIBC_MAX_VERSION = (2, 35)
PACKAGE_VERSION_FILENAME = "VERSION"
PORTABLE_PLUGINS_DIR = "data/plugins"
PORTABLE_UPDATE_MANIFEST_FILENAME = "portable-update.json"
PORTABLE_UPDATE_MANIFEST_FORMAT = 1


def normalized_version(raw: str) -> str:
    """Match the package script's tag-to-artifact version normalization."""
    for prefix in ("refs/tags/", "native-v", "gpui-v", "v"):
        if raw.startswith(prefix):
            raw = raw[len(prefix) :]
    return raw


def target_label(target: str) -> str:
    labels = {
        "x86_64-pc-windows-msvc": "windows_x64",
    }
    if target not in labels:
        raise ValueError(f"unsupported release target: {target}")
    return labels[target]


def expected_artifact_names(target: str, version: str) -> set[str]:
    label = target_label(target)
    names = {f"oxide_{version}_{label}_portable"}
    if "windows" in target:
        return {
            f"oxide_{version}_{label}-setup.exe",
            f"oxide_{version}_{label}_portable.zip",
        }
    names = {f"oxide_{version}_{label}_portable.tar.gz"}
    return names


def archive_names(path: Path) -> set[str]:
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            return set(archive.namelist())
    if path.name.endswith(".tar.gz"):
        with tarfile.open(path, "r:gz") as archive:
            return set(archive.getnames())
    raise ValueError(f"unsupported archive: {path}")


def require_archive_suffixes(names: set[str], suffixes: set[str], artifact: Path) -> None:
    normalized_names = {name.rstrip("/") for name in names}
    missing = [
        suffix
        for suffix in suffixes
        if not any(name.endswith(suffix) for name in normalized_names)
    ]
    if missing:
        raise RuntimeError(f"{artifact.name} is missing archive entries: {', '.join(missing)}")


def archive_entry_bytes(path: Path, suffix: str) -> bytes:
    """Read one archive entry selected by a stable package-relative suffix."""
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            matches = [name for name in archive.namelist() if name.endswith(suffix)]
            if len(matches) != 1:
                raise RuntimeError(f"expected one {suffix} in {path.name}, found {len(matches)}")
            return archive.read(matches[0])
    with tarfile.open(path, "r:gz") as archive:
        matches = [member for member in archive.getmembers() if member.name.endswith(suffix)]
        if len(matches) != 1:
            raise RuntimeError(f"expected one {suffix} in {path.name}, found {len(matches)}")
        extracted = archive.extractfile(matches[0])
        if extracted is None:
            raise RuntimeError(f"{suffix} in {path.name} is not a regular file")
        return extracted.read()


def verify_embedded_version(path: Path, suffix: str, expected_version: str) -> None:
    actual = archive_entry_bytes(path, suffix).decode("utf-8").strip()
    if actual != expected_version:
        raise RuntimeError(
            f"{path.name} contains version {actual!r}, expected {expected_version!r}"
        )


def verify_portable_archive(path: Path, target: str, expected_version: str) -> None:
    executable = "oxide.exe" if "windows" in target else "oxide"
    update_helper = (
        "tools/oxide-update-helper.exe"
        if "windows" in target
        else "tools/oxide-update-helper"
    )
    require_archive_suffixes(
        archive_names(path),
        REQUIRED_DOCUMENTS
        | {
            PACKAGE_VERSION_FILENAME,
            PORTABLE_PLUGINS_DIR,
            "portable",
            PORTABLE_UPDATE_MANIFEST_FILENAME,
            update_helper,
            executable,
        },
        path,
    )
    verify_embedded_version(path, PACKAGE_VERSION_FILENAME, expected_version)
    manifest = json.loads(
        archive_entry_bytes(path, PORTABLE_UPDATE_MANIFEST_FILENAME).decode("utf-8")
    )
    if manifest.get("formatVersion") != PORTABLE_UPDATE_MANIFEST_FORMAT:
        raise RuntimeError(f"{path.name} has an unsupported portable update manifest")
    if manifest.get("appExecutable") != executable:
        raise RuntimeError(f"{path.name} portable update manifest has the wrong executable")
    if manifest.get("updateHelper") != update_helper:
        raise RuntimeError(f"{path.name} portable update manifest has the wrong helper")
    managed_entries = manifest.get("managedEntries")
    required_managed_entries = {
        executable,
        "resources",
        "tools",
        "portable",
        PACKAGE_VERSION_FILENAME,
        PORTABLE_UPDATE_MANIFEST_FILENAME,
    }
    if not isinstance(managed_entries, list) or not required_managed_entries.issubset(
        managed_entries
    ):
        raise RuntimeError(f"{path.name} portable update manifest is incomplete")
    if {"data", "portable.json"} & set(managed_entries):
        raise RuntimeError(f"{path.name} portable update manifest includes user data")
    if "windows" in target:
        for name, (_, digest) in conpty_runtime_files(target).items():
            entry = f"/resources/conpty/{name}"
            verify_digest(archive_entry_bytes(path, entry), digest, entry)


def run_checked(command: list[str], *, cwd: Path | None = None) -> str:
    result = subprocess.run(
        command,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"command failed ({result.returncode}): {' '.join(command)}\n{result.stdout}")
    return result.stdout


def verify_binary_architecture(path: Path, target: str) -> None:
    file_tool = shutil.which("file")
    if not file_tool:
        raise RuntimeError("file is required for release architecture verification")
    description = run_checked([file_tool, str(path)]).lower()
    expected = ("x86-64", "x86_64") if target.startswith("x86_64") else ("arm64", "aarch64")
    if not any(marker in description for marker in expected):
        raise RuntimeError(f"{path.name} does not match {target}: {description.strip()}")


def extract_portable_binary(path: Path, target: str, destination: Path) -> Path:
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            archive.extractall(destination)
    else:
        with tarfile.open(path, "r:gz") as archive:
            archive.extractall(destination, filter="data")
    executable = "oxide.exe" if "windows" in target else "oxide"
    matches = list(destination.rglob(executable))
    if len(matches) != 1:
        raise RuntimeError(f"expected one {executable} in {path.name}, found {len(matches)}")
    return matches[0]

def require_metadata_values(
    metadata: str,
    expected_values: set[str],
    artifact: Path,
    field_name: str,
) -> None:
    """Require semantic package names in one metadata field."""
    metadata_values = set(re.findall(r"[A-Za-z0-9][A-Za-z0-9+.-]*", metadata))
    missing = sorted(expected_values - metadata_values)
    if missing:
        raise RuntimeError(
            f"{artifact.name} {field_name} is missing: {', '.join(missing)}"
        )


def verify_windows_installer(path: Path, expected_version: str, target: str) -> None:
    seven_zip = next((shutil.which(name) for name in ("7z", "7zz", "7za") if shutil.which(name)), None)
    if not seven_zip:
        raise RuntimeError("7-Zip is required for NSIS content verification")
    listing = run_checked([seven_zip, "l", "-slt", str(path)])
    for name in REQUIRED_DOCUMENTS | {
        PACKAGE_VERSION_FILENAME,
        "oxide.exe",
        "oxide-update-helper.exe",
    }:
        if name not in listing:
            raise RuntimeError(f"{path.name} does not contain {name}")
    with tempfile.TemporaryDirectory() as directory:
        run_checked([seven_zip, "x", "-y", f"-o{directory}", str(path)])
        versions = list(Path(directory).rglob(PACKAGE_VERSION_FILENAME))
        if not versions or all(item.read_text(encoding="utf-8").strip() != expected_version for item in versions):
            raise RuntimeError(f"{path.name} does not contain version {expected_version}")

        # NSIS includes separate normal-install and staged-update payloads.
        for root in (Path(directory), Path(directory) / "install"):
            for name, (_, digest) in conpty_runtime_files(target).items():
                runtime = root / "resources" / "conpty" / name
                relative = runtime.relative_to(directory).as_posix()
                if not runtime.is_file():
                    raise RuntimeError(f"{path.name} must contain {relative}")
                verify_digest(runtime.read_bytes(), digest, relative)


def verify_release(dist: Path, target: str, version: str) -> dict[str, object]:
    version = normalized_version(version)
    expected = expected_artifact_names(target, version)
    missing = sorted(name for name in expected if not (dist / name).is_file())
    if missing:
        raise RuntimeError(f"missing release artifacts: {', '.join(missing)}")
    empty = sorted(name for name in expected if (dist / name).stat().st_size == 0)
    if empty:
        raise RuntimeError(f"empty release artifacts: {', '.join(empty)}")

    label = target_label(target)
    portable_name = (
        f"oxide_{version}_{label}_portable.zip"
        if "windows" in target
        else f"oxide_{version}_{label}_portable.tar.gz"
    )
    portable_path = dist / portable_name
    verify_portable_archive(portable_path, target, version)
    with tempfile.TemporaryDirectory() as directory:
        binary = extract_portable_binary(portable_path, target, Path(directory))
        verify_binary_architecture(binary, target)

    if "windows" in target:
        verify_windows_installer(dist / f"oxide_{version}_{label}-setup.exe", version, target)

    return {"target": target, "version": version, "artifacts": sorted(expected), "status": "ok"}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--dist", type=Path, default=Path("dist"))
    args = parser.parse_args()

    try:
        summary = verify_release(args.dist, args.target, args.version)
    except Exception as error:
        print(json.dumps({"status": "error", "error": str(error)}, ensure_ascii=True))
        return 1
    print(json.dumps(summary, ensure_ascii=True, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
