"""Verify the selected local environment without installing or changing PATH."""

import argparse
import base64
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from urllib.parse import parse_qsl

ROOT = Path(__file__).resolve().parents[2]
SYSTEMS = {f"{arch}-{osname}" for arch in ("aarch64", "x86_64")
           for osname in ("darwin", "linux")}
VERSIONS = {"rust": "1.95.0", "bun": "1.3.11", "nodejs": "22.23.1",
            "python": "3.13.13", "pre-commit": "4.5.1", "cargo-deny": "0.19.9",
            "actionlint": "1.7.12", "shellcheck": "0.11.0", "git": "2.54.0",
            "pkg-config": "0.29.2"}
COMMANDS = {
    "rustc": ("rust", "1.95.0"), "cargo": ("rust", "1.95.0"),
    "cargo-clippy": ("rust", "0.1.95"), "clippy-driver": ("rust", "0.1.95"),
    "rustfmt": ("rust", "1.9.0-stable"), "cargo-fmt": ("rust", "1.9.0-stable"),
    "bun": ("bun", "1.3.11"), "node": ("nodejs", "22.23.1"),
    "python3": ("python", "3.13.13"), "pre-commit": ("pre-commit", "4.5.1"),
    "cargo-deny": ("cargo-deny", "0.19.9"), "actionlint": ("actionlint", "1.7.12"),
    "shellcheck": ("shellcheck", "0.11.0"), "git": ("git", "2.54.0"),
    "pkg-config": ("pkg-config", "0.29.2"),
}
PUBLIC_REF = re.compile(r"github:[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/[0-9a-f]{40}")
STORE_PATH = re.compile(r"/nix/store/[0-9a-z]{32}-[^/]+")


def public_reference(value, locked=False):
    """Compare source identity while allowing a validated locked NAR commitment."""
    if not isinstance(value, str) or "#" in value:
        raise ValueError("invalid public flake reference")
    base, separator, query = value.partition("?")
    if not PUBLIC_REF.fullmatch(base) or (separator and not query):
        raise ValueError("invalid public flake reference")
    pairs = parse_qsl(query, keep_blank_values=True, strict_parsing=True)
    parameters = unique_object(pairs)
    allowed = {"dir", "narHash"} if locked else {"dir"}
    if set(parameters) - allowed:
        raise ValueError("unsupported flake reference parameter")
    directory = parameters.get("dir", "")
    if "dir" in parameters and directory != ".flox/toolchain":
        raise ValueError("unselected flake directory")
    if "narHash" in parameters:
        digest = parameters["narHash"]
        if not digest.startswith("sha256-"):
            raise ValueError("invalid NAR hash")
        try:
            raw = base64.b64decode(digest[7:], validate=True)
        except ValueError as error:
            raise ValueError("invalid NAR hash") from error
        if len(raw) != 32 or base64.b64encode(raw).decode() != digest[7:]:
            raise ValueError("invalid NAR hash")
    return base, directory


def native_system():
    arch = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64"}.get(
        platform.machine())
    system = f"{arch}-{platform.system().lower()}"
    if system not in SYSTEMS:
        raise ValueError("unsupported native system")
    return system


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate lock field")
        result[key] = value
    return result


def package_name(name):
    return "rust" if name == "rust-toolchain" else name


def validate_lock(manifest, lock, system):
    """Validate Flox v1 flake records; return selected native output roots."""
    if system not in SYSTEMS:
        raise ValueError("unsupported native system")
    if lock.get("lockfile-version") != 1 or lock.get("manifest") != manifest:
        raise ValueError("lock version or embedded manifest mismatch")
    systems = manifest.get("options", {}).get("systems", [])
    if len(systems) != 4 or set(systems) != SYSTEMS or manifest.get("include"):
        raise ValueError("manifest must select exactly four systems without includes")
    installs = manifest.get("install", {})
    expected = set(VERSIONS) | {"cc", "binutils", "toolchain-env"}
    if len(installs) != 13 or {package_name(n) for n in installs} != expected:
        raise ValueError("manifest must select all thirteen outputs")
    origins = {}
    for name, descriptor in installs.items():
        if set(descriptor) != {"flake"} or not isinstance(descriptor["flake"], str):
            raise ValueError("only public immutable flake descriptors are supported")
        ref, separator, attribute = descriptor["flake"].partition("#")
        if not separator or attribute != name:
            raise ValueError("invalid public flake selection")
        origins[name] = public_reference(ref)
    records = lock.get("packages")
    if not isinstance(records, list) or len(records) != 52:
        raise ValueError("lock must contain all 52 selections")
    selected, seen = {}, set()
    for record in records:
        name, target = record.get("install_id"), record.get("system")
        key = (name, target)
        if name not in installs or target not in SYSTEMS or key in seen:
            raise ValueError("unknown or duplicate package/system")
        seen.add(key)
        if record.get("package-system") != target:
            raise ValueError("actual package-system mismatch")
        if public_reference(record.get("locked-url"), locked=True) != origins[name]:
            raise ValueError("locked source differs from selected origin")
        attribute = record.get("locked-flake-attr-path")
        if attribute not in (f"packages.{target}.{name}", f"legacyPackages.{target}.{name}"):
            raise ValueError("locked attribute differs from selected system/output")
        if not STORE_PATH.fullmatch(record.get("derivation", "")) or not record["derivation"].endswith(".drv"):
            raise ValueError("invalid derivation")
        outputs = record.get("outputs")
        names = record.get("output-names")
        if not isinstance(outputs, dict) or not outputs or not isinstance(names, list):
            raise ValueError("missing locked outputs")
        if len(names) != len(set(names)) or set(names) != set(outputs):
            raise ValueError("inconsistent output names")
        if any(not isinstance(p, str) or not STORE_PATH.fullmatch(p) for p in outputs.values()):
            raise ValueError("invalid output path")
        installed = record.get("requested-outputs-to-install")
        if installed is None or installed == []:
            installed = record.get("outputs-to-install")
        if installed is None:
            installed = ["out"]
        if not isinstance(installed, list) or not installed or any(n not in outputs for n in installed):
            raise ValueError("invalid selected outputs")
        canonical = package_name(name)
        version = VERSIONS.get(canonical)
        if canonical == "cc":
            version = "21.1.8" if target.endswith("darwin") else "15.3.0"
        if version and record.get("version") != version:
            raise ValueError(f"locked version mismatch: {name}")
        if record.get("broken") is True:
            raise ValueError("selected package is broken")
        if target == system:
            selected[canonical] = [Path(outputs[n]) for n in installed]
    return selected


def support_origin(environment, outputs):
    relative = Path("share/foundry/toolchain.sh")
    activated = environment / relative
    if not activated.is_file() or not any(
            (root / relative).is_file() and os.path.samefile(activated, root / relative)
            for root in outputs):
        raise ValueError("toolchain support script is outside selected locked output")
    return str(activated)


def command_origin(command, environment, outputs):
    resolved = shutil.which(command)
    activated = environment / "bin" / command
    if not resolved or not activated.is_file() or not os.path.samefile(resolved, activated):
        raise ValueError(f"command is outside activated environment: {command}")
    if not any((root / "bin" / command).is_file()
               and os.path.samefile(resolved, root / "bin" / command) for root in outputs):
        raise ValueError(f"command is outside selected locked output: {command}")
    return str(activated)


def output(arguments, *, darwin_linker=False):
    try:
        completed = subprocess.run(arguments, check=True, text=True, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, timeout=60)
    except subprocess.CalledProcessError as error:
        diagnostic = error.stdout or ""
        if (darwin_linker and error.returncode == 1
                and re.search(r"^@\(#\)PROGRAM:ld PROJECT:ld64-956\.6$", diagnostic, re.MULTILINE)
                and "ld: no object files specified" in diagnostic):
            return diagnostic.strip()
        raise ValueError(f"command failed: {Path(arguments[0]).name}") from error
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError(f"command failed: {Path(arguments[0]).name}") from error
    return completed.stdout.strip()


def require_version(text, version, command):
    if not re.search(r"(?<![\w.])v?" + re.escape(version) + r"(?![\w.+-])", text):
        raise ValueError(f"executable version mismatch: {command}")


def archive_smoke(commands, compile_code=False):
    """Exercise native archive tools; optionally compile, archive, link and execute C."""
    with tempfile.TemporaryDirectory(prefix="flox-verify-") as temporary:
        directory = Path(temporary)
        archive = directory / "libprobe.a"
        members = []
        if compile_code:
            source = directory / "value.c"
            source.write_text("int value(void) { return 42; }\n")
            obj = directory / "value.o"
            output([commands["cc"], "-c", str(source), "-o", str(obj)])
            members.append(str(obj))
        output([commands["ar"], "rcs", str(archive), *members])
        output([commands["ranlib"], str(archive)])
        output([commands["ar"], "t", str(archive)])
        if compile_code:
            main = directory / "main.c"
            main.write_text("int value(void); int main(void) { return value() != 42; }\n")
            binary = directory / "probe"
            output([commands["cc"], str(main), str(archive), "-o", str(binary)])
            output([str(binary)])


def verify(root=ROOT, smoke=False):
    system = native_system()
    directory = root / ".flox" / "env"
    manifest = tomllib.loads((directory / "manifest.toml").read_text())
    lock = json.loads((directory / "manifest.lock").read_text(), object_pairs_hook=unique_object)
    selected = validate_lock(manifest, lock, system)
    active = os.environ.get("FLOX_ENV")
    if not active or not Path(active).is_absolute():
        raise ValueError("FLOX_ENV must name the activated environment")
    environment = Path(active)
    support = support_origin(environment, selected["toolchain-env"])
    commands, evidence = {}, {}
    specifications = dict(COMMANDS)
    compiler_version = "21.1.8" if system.endswith("darwin") else "15.3.0"
    specifications.update({"cc": ("cc", compiler_version), "c++": ("cc", compiler_version)})
    for command, (package, version) in specifications.items():
        path = command_origin(command, environment, selected[package])
        commands[command] = path
        flag = "-version" if command == "actionlint" else "--version"
        banner = output([path, flag])
        require_version(banner, version, command)
        evidence[command] = {"path": path, "version": banner}
    for command in ("ar", "ranlib", "ld"):
        commands[command] = command_origin(command, environment, selected["binutils"])
        evidence[command] = {"path": commands[command]}
    # Native archivers differ in version flags; exercise their common interface.
    archive_smoke(commands, smoke)
    evidence["ld"]["version"] = output([commands["ld"], "-v"], darwin_linker=system.endswith("darwin"))
    return {"system": system, "commands": evidence, "support_script": support,
            "c_link_smoke": smoke}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--smoke", action="store_true", help="compile, archive, link and execute C")
    arguments = parser.parse_args()
    try:
        print(json.dumps(verify(smoke=arguments.smoke), sort_keys=True))
    except (ValueError, OSError, TypeError, KeyError) as error:
        print(f"Flox verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
