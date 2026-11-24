"""Version-gated Windows releases. Requires Python 3.11+; no pip dependencies."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib
import urllib.error
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SEMVER = re.compile(
    r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?", re.ASCII
)


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True, encoding="utf-8").strip()


def version(root=ROOT):
    package = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["package"]
    value = package["version"]
    if not SEMVER.fullmatch(value):
        raise ValueError(f"Invalid semantic version: {value!r}")
    locked = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    matches = [p for p in locked["package"] if p["name"] == package["name"] and "source" not in p]
    if len(matches) != 1 or matches[0]["version"] != value:
        raise ValueError("Cargo.lock version differs from Cargo.toml; run cargo check to update it")
    return value


def detect():
    current = version()
    event = os.environ["GITHUB_EVENT_NAME"]
    if os.environ["GITHUB_REF"] != "refs/heads/master":
        raise ValueError("Releases must run on master")
    changed = event == "workflow_dispatch"
    if event == "push":
        before = os.environ["BEFORE"]
        # No trustworthy comparison for a newly created branch: use manual dispatch.
        if before and set(before) != {"0"}:
            previous = tomllib.loads(run("git", "show", f"{before}:Cargo.toml"))["package"]["version"]
            changed = current != previous
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"changed={str(changed).lower()}\nversion={current}\n")
    print(f"Version {current}; release requested: {changed}")


def asset_name(value):
    return f"native-markdown-v{value}-windows-x64.zip"


def package(binary, runtime_dir, output):
    value = version()
    if not binary.is_file():
        raise ValueError(f"Missing executable: {binary}")
    dlls = sorted(runtime_dir.glob("*.dll"))
    if not dlls:
        raise ValueError("No Visual C++ runtime DLLs found")
    output.mkdir(parents=True, exist_ok=True)
    archive = output / asset_name(value)
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, strict_timestamps=False) as bundle:
        bundle.write(binary, "native-markdown.exe")
        for dll in dlls:
            bundle.write(dll, dll.name)
        for name in ("README.md", "README_zh.md", "docs/demo.md"):
            bundle.write(ROOT / name, name)
        for path in sorted((ROOT / "assets/web/vendor").glob("*LICENSE*")):
            bundle.write(path, f"licenses/web/{path.name}")
        # Include notices shipped by the resolved Rust crates, without copying sources.
        metadata = json.loads(run("cargo", "+stable", "metadata", "--locked", "--offline",
                                  "--filter-platform", "x86_64-pc-windows-msvc", "--format-version", "1"))
        resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
        notices = []
        for dependency in metadata["packages"]:
            if dependency["id"] not in resolved or dependency["id"] in metadata["workspace_members"]:
                continue
            notices.append({key: dependency[key] for key in ("name", "version", "license", "repository")})
            folder = Path(dependency["manifest_path"]).parent
            files = {p for p in folder.iterdir() if p.is_file() and
                     p.name.lower().startswith(("license", "copying", "notice"))}
            if dependency.get("license_file"):
                files.add(folder / dependency["license_file"])
            for path in sorted(files):
                bundle.write(path, f"licenses/rust/{dependency['name']}-{dependency['version']}/{path.name}")
        bundle.writestr("licenses/rust-packages.json", json.dumps(notices, indent=2) + "\n")
        bundle.writestr("START-HERE.txt", (
            f"Native Markdown {value} - Windows x64 portable\n\n"
            "Extract the entire ZIP, then run native-markdown.exe. Keep the DLLs beside it.\n"
            "Requires Microsoft Edge WebView2 Runtime (not bundled or silently installed).\n"
            "https://developer.microsoft.com/microsoft-edge/webview2/\n"
            "Portable means no installer; settings/history still use %APPDATA%/NativeMarkdown.\n"
            "See README.md / README_zh.md for usage and storage details.\n"
        ))
    with archive.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    archive.with_suffix(".zip.sha256").write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    print(archive)


def api(path, method="GET", data=None, missing_ok=False):
    request = urllib.request.Request(
        f"{os.environ.get('GITHUB_API_URL', 'https://api.github.com')}/repos/{os.environ['GITHUB_REPOSITORY']}/{path}",
        method=method,
        data=None if data is None else json.dumps(data).encode("utf-8"),
        headers={"Authorization": f"Bearer {os.environ['GH_TOKEN']}",
                 "Accept": "application/vnd.github+json", "Content-Type": "application/json",
                 "X-GitHub-Api-Version": "2022-11-28"},
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        if missing_ok and error.code == 404:
            return None
        raise


def publish(output):
    value = version()
    tag = f"v{value}"
    sha = os.environ["GITHUB_SHA"]
    archive = output / asset_name(value)
    checksum = archive.with_suffix(".zip.sha256")
    with archive.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    if checksum.read_text(encoding="utf-8") != f"{digest}  {archive.name}\n":
        raise ValueError("Release archive checksum mismatch")
    ref = api(f"git/ref/tags/{tag}", missing_ok=True)
    if ref:
        obj = ref["object"]
        while obj["type"] == "tag":
            obj = api(f"git/tags/{obj['sha']}")["object"]
        if obj["type"] != "commit" or obj["sha"] != sha:
            raise ValueError(f"Tag {tag} already points to another commit; bump the version")
    else:
        api("git/refs", "POST", {"ref": f"refs/tags/{tag}", "sha": sha})
    release = api(f"releases/tags/{tag}", missing_ok=True)
    if release and not release["draft"]:
        print(f"{tag} is already published; leaving its assets unchanged")
        return
    prerelease = "-" in value.split("+", 1)[0]
    if not release:
        release = api("releases", "POST", {
            "tag_name": tag, "target_commitish": sha, "name": tag,
            "draft": True, "prerelease": prerelease, "generate_release_notes": True,
        })
    # A failed upload leaves a draft; retry can replace only draft assets.
    subprocess.run(["gh", "release", "upload", tag, str(archive), str(checksum),
                    "--clobber", "--repo", os.environ["GITHUB_REPOSITORY"]], check=True)
    assets = api(f"releases/{release['id']}/assets?per_page=100")
    expected = {archive.name: archive.stat().st_size, checksum.name: checksum.stat().st_size}
    for name, size in expected.items():
        if not any(a["name"] == name and a["size"] == size and a["state"] == "uploaded" for a in assets):
            raise ValueError(f"Incomplete release asset: {name}")
    api(f"releases/{release['id']}", "PATCH", {
        "draft": False, "prerelease": prerelease, "make_latest": "false" if prerelease else "true",
    })
    print(f"Published {tag}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("detect")
    pack = commands.add_parser("package")
    pack.add_argument("--binary", type=Path, required=True)
    pack.add_argument("--runtime-dir", type=Path, required=True)
    pack.add_argument("--output", type=Path, required=True)
    pub = commands.add_parser("publish")
    pub.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "detect":
        detect()
    elif args.command == "package":
        package(args.binary, args.runtime_dir, args.output)
    else:
        publish(args.output)
