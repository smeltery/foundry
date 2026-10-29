"""Offline readers for the package's selected data formats and local references."""
import json
from pathlib import Path
import re
import tarfile


class InvalidPackage(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise InvalidPackage(message)


def contained(root, reference, *, base=None, parents=False, directory=False):
    require(isinstance(reference, str) and bool(reference), 'empty reference')
    require(not any(ord(char) < 32 for char in reference), 'control in reference')
    require(not any(char in reference for char in ('\\', '%', ':', '?', '#')),
            'invalid local reference')
    require(not reference.startswith('/'), 'absolute reference')
    parts = reference.split('/')
    require(parents or '..' not in parts, 'parent traversal')
    require('' not in parts[:-1], 'empty path component')
    root = Path(root).resolve()
    origin = Path(base) if base is not None else root
    candidate = origin / reference
    resolved = candidate.resolve()
    require(resolved.is_relative_to(root), 'reference escapes package')
    # Shipped packages are ordinary files, not links into a developer checkout.
    cursor = candidate
    while cursor != root and cursor != cursor.parent:
        require(not cursor.is_symlink(), 'symbolic link in package')
        cursor = cursor.parent
    require(resolved.is_dir() if directory else resolved.is_file(),
            'missing directory' if directory else 'missing file')
    return resolved


def read_text(path):
    try:
        raw = Path(path).read_bytes()
        require(len(raw) <= 1024 * 1024, 'file too large')
        text = raw.decode('utf-8')
    except (OSError, UnicodeError) as error:
        raise InvalidPackage('unreadable UTF-8 file') from error
    require(not text.startswith('\ufeff') and '\x00' not in text, 'invalid text prefix')
    return text


def read_json(path):
    def pairs(entries):
        result = {}
        for key, value in entries:
            require(key not in result, 'duplicate JSON key')
            result[key] = value
        return result

    def constant(_):
        raise InvalidPackage('invalid JSON number')

    try:
        document = json.loads(read_text(path), object_pairs_hook=pairs,
                              parse_constant=constant)
    except (ValueError, RecursionError) as error:
        raise InvalidPackage('invalid JSON document') from error
    require(isinstance(document, dict), 'JSON object required')
    return document


def fields(value, required, optional=()):
    require(isinstance(value, dict), 'object required')
    require(set(required) <= value.keys(), 'missing required field')
    require(value.keys() <= set(required) | set(optional), 'unknown field')


def text_value(value):
    require(isinstance(value, str) and bool(value.strip()), 'nonempty string required')
    require(not any(ord(char) < 32 for char in value), 'control in string')
    return value


def scalar(value):
    """The authored metadata uses scalar YAML, not tags, anchors or executable data."""
    if value in ('true', 'false'):
        return value == 'true'
    if value.startswith('"'):
        try:
            result = json.loads(value)
        except ValueError as error:
            raise InvalidPackage('invalid quoted scalar') from error
        return text_value(result)
    require(not value.startswith(("'", '[', '{', '&', '*', '!', '|', '>')),
            'unsupported scalar form')
    require(value not in ('null', '~') and ': ' not in value and ' #' not in value,
            'ambiguous scalar')
    return text_value(value)


def metadata(text):
    """Parse the intentionally small, two-space mapping subset used by this package."""
    root = {}
    stack = [(-2, root)]
    for line in text.splitlines():
        if not line.strip():
            continue
        match = re.fullmatch(r'( *)([A-Za-z_][A-Za-z_0-9-]*):(?: (.*))?', line)
        require(match is not None, 'invalid metadata mapping')
        indent, key, value = len(match[1]), match[2], match[3]
        require(indent % 2 == 0, 'invalid metadata indentation')
        while stack[-1][0] >= indent:
            stack.pop()
        require(indent == stack[-1][0] + 2, 'invalid metadata nesting')
        parent = stack[-1][1]
        require(key not in parent, 'duplicate metadata key')
        if value is None or value == '':
            parent[key] = {}
            stack.append((indent, parent[key]))
        else:
            parent[key] = scalar(value)
    return root


def frontmatter(path):
    text = read_text(path)
    lines = text.splitlines()
    require(lines and lines[0] == '---', 'frontmatter required')
    try:
        end = lines.index('---', 1)
    except ValueError as error:
        raise InvalidPackage('unterminated frontmatter') from error
    body = '\n'.join(lines[end + 1:]).strip()
    require(bool(body), 'empty instruction body')
    return metadata('\n'.join(lines[1:end])), body


def inventory(root):
    root = Path(root).resolve()
    entries = {}
    for path in sorted(root.rglob('*')):
        require(not path.is_symlink(), 'symbolic link in package')
        if path.is_dir():
            continue
        require(path.is_file(), 'nonregular package entry')
        entries[path.relative_to(root).as_posix()] = path.read_bytes()
    return entries


def archive_inventory(path):
    """Read, never extract; reject aliases and links before inspecting payloads."""
    entries = {}
    try:
        with tarfile.open(path) as archive:
            for member in archive.getmembers():
                name = member.name
                parts = name.split('/')
                require(name and not name.startswith('/') and
                        not any(part in ('', '.', '..') for part in parts) and
                        not any(char in name for char in ('\\', ':', '%', '\x00')),
                        'unsafe archive member')
                require(name not in entries, 'duplicate archive member')
                require(member.isfile(), 'archive must contain regular files only')
                require(member.size <= 128 * 1024 * 1024, 'archive member too large')
                require(len(entries) < 256, 'too many archive files')
                stream = archive.extractfile(member)
                require(stream is not None, 'missing archive payload')
                entries[name] = stream.read()
    except (OSError, tarfile.TarError) as error:
        raise InvalidPackage('invalid package archive') from error
    return entries


def compare_archive(path, expected):
    require(archive_inventory(path) == expected, 'archive inventory or bytes differ')
