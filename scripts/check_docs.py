#!/usr/bin/env python3
"""Check the documentation book (mdBook, `book.toml`, sources in `docs/`).

Source checks (no mdBook needed, run them before pushing):

    python3 scripts/check_docs.py

- every chapter listed in `docs/SUMMARY.md` exists;
- every `docs/**/*.md` page is listed in `docs/SUMMARY.md` (or in EXCLUDED);
- every relative link and image in a page resolves to a file inside `docs/`
  (a link that leaves `docs/` would break on the site: use an absolute
  GitHub URL instead), and every `#anchor` names a heading of its page.

Built-site check (CI runs it after `mdbook build`):

    python3 scripts/check_docs.py --html book

- every relative `href` / `src` in the generated HTML resolves to a file of
  the site, and every `#anchor` to an `id` on the target page.

Exits 1 and lists every problem when a check fails. Standard library only.
"""

import argparse
import re
import sys
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit

REPO = Path(__file__).resolve().parent.parent
DOCS = REPO / "docs"
SUMMARY = DOCS / "SUMMARY.md"

# Pages under docs/ that are deliberately not chapters of the book.
EXCLUDED: set[str] = set()

# Generated mdBook pages whose links are not ours to check.
HTML_SKIPPED = {"print.html", "404.html", "toc.html"}

FENCE = re.compile(r"^\s*(```|~~~)")
INLINE_CODE = re.compile(r"(`+)(.+?)\1")
# [text](target) and ![alt](target), with an optional "title".
MD_LINK = re.compile(r"!?\[(?:[^\[\]]|\[[^\]]*\])*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
# [label]: target reference definitions.
MD_REF = re.compile(r"^\s{0,3}\[[^\]]+\]:\s*<?(\S+?)>?(?:\s+.*)?$")
HTML_ATTR = re.compile(r"""<(?:img|a)\b[^>]*?\b(?:src|href)\s*=\s*["']([^"']+)["']""", re.I)
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
EXTERNAL = re.compile(r"^[a-z][a-z0-9+.-]*:|^//", re.I)


def strip_code(lines):
    """Yield (line number, text) outside fenced code, inline code blanked."""
    in_fence = False
    for number, line in enumerate(lines, 1):
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if not in_fence:
            yield number, INLINE_CODE.sub(lambda m: " " * len(m.group(0)), line)


def heading_text(raw):
    """The visible text of a Markdown heading."""
    text = re.sub(r"`([^`]*)`", r"\1", raw)
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"<[^>]+>", "", text)
    return re.sub(r"(\*\*|__|\*|_)(\S.*?)\1", r"\2", text)


def slug(text):
    """mdBook's (and GitHub's) heading id: lowercase, spaces to '-'."""
    out = []
    for ch in text.strip():
        if ch.isalnum() or ch in "-_":
            out.append(ch.lower())
        elif ch.isspace():
            out.append("-")
    return "".join(out)


def heading_ids(path):
    """Every heading id of a page, with -1, -2 suffixes for repeats."""
    ids, seen = set(), {}
    in_fence = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        match = None if in_fence else HEADING.match(line)
        if not match:
            continue
        base = slug(heading_text(match.group(2)))
        count = seen.get(base, 0)
        seen[base] = count + 1
        ids.add(base if count == 0 else f"{base}-{count}")
    return ids


def page_links(path):
    """(line, target) for every link and image of a Markdown page."""
    for number, line in strip_code(path.read_text(encoding="utf-8").splitlines()):
        for pattern in (MD_LINK, HTML_ATTR):
            for match in pattern.finditer(line):
                yield number, match.group(1)
        ref = MD_REF.match(line)
        if ref:
            yield number, ref.group(1)


def check_link(page, number, target, anchors):
    """The problem with one link of a page, or None."""
    if EXTERNAL.match(target):
        return None
    parts = urlsplit(target)
    where = f"{page.relative_to(REPO)}:{number}: {target}"
    dest = page if not parts.path else (page.parent / unquote(parts.path)).resolve()
    if DOCS.resolve() not in dest.parents and dest != DOCS.resolve():
        return f"{where}: leaves docs/ (breaks on the site; use an absolute GitHub URL)"
    if not dest.exists():
        return f"{where}: no such file"
    if parts.fragment and dest.suffix == ".md":
        if dest not in anchors:
            anchors[dest] = heading_ids(dest)
        if parts.fragment not in anchors[dest]:
            return f"{where}: no heading with id '{parts.fragment}'"
    return None


def summary_chapters():
    """The .md files the SUMMARY lists, relative to docs/."""
    chapters = []
    for number, line in strip_code(SUMMARY.read_text(encoding="utf-8").splitlines()):
        for match in MD_LINK.finditer(line):
            target = urlsplit(match.group(1)).path
            if target and not EXTERNAL.match(match.group(1)):
                chapters.append((number, unquote(target)))
    return chapters


def check_sources():
    problems = []
    listed = set()
    for number, chapter in summary_chapters():
        listed.add(chapter)
        if not (DOCS / chapter).is_file():
            problems.append(f"docs/SUMMARY.md:{number}: {chapter}: no such page")
    pages = sorted(DOCS.rglob("*.md"))
    for page in pages:
        name = page.relative_to(DOCS).as_posix()
        if page != SUMMARY and name not in listed and name not in EXCLUDED:
            problems.append(f"docs/{name}: not listed in docs/SUMMARY.md (add it, or to EXCLUDED)")
    anchors = {}
    for page in pages:
        if page == SUMMARY:
            continue  # its chapters are checked above
        for number, target in page_links(page):
            problem = check_link(page, number, target, anchors)
            if problem:
                problems.append(problem)
    return problems, len(pages)


class _Collector(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.ids, self.refs = set(), []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        for key in ("id", "name"):
            if attrs.get(key) and (key == "id" or tag == "a"):
                self.ids.add(attrs[key])
        ref = attrs.get("href") if tag == "a" else attrs.get("src") if tag == "img" else None
        if ref:
            self.refs.append((self.getpos()[0], ref))


def parse_html(path):
    collector = _Collector()
    collector.feed(path.read_text(encoding="utf-8", errors="replace"))
    return collector


def check_html(site):
    site = site.resolve()
    problems, parsed = [], {}

    def ids_of(path):
        if path not in parsed:
            parsed[path] = parse_html(path)
        return parsed[path].ids

    pages = sorted(p for p in site.rglob("*.html") if p.relative_to(site).as_posix() not in HTML_SKIPPED)
    for page in pages:
        ids_of(page)
        for number, ref in parsed[page].refs:
            if EXTERNAL.match(ref):
                continue
            parts = urlsplit(ref)
            where = f"{page.relative_to(site)}:{number}: {ref}"
            dest = page if not parts.path else (page.parent / unquote(parts.path)).resolve()
            if dest.is_dir():
                dest = dest / "index.html"
            if site not in dest.parents:
                problems.append(f"{where}: leaves the site")
            elif not dest.is_file():
                problems.append(f"{where}: no such file")
            elif parts.fragment and dest.suffix == ".html" and unquote(parts.fragment) not in ids_of(dest):
                problems.append(f"{where}: no element with id '{parts.fragment}'")
    return problems, len(pages)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--html", type=Path, metavar="DIR", help="check a built site instead of the sources")
    args = parser.parse_args()
    if args.html:
        problems, count = check_html(args.html)
        what = "HTML pages"
    else:
        problems, count = check_sources()
        what = "pages"
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    if problems:
        print(f"{len(problems)} problem(s) in {count} {what}", file=sys.stderr)
        return 1
    print(f"ok: {count} {what} checked")
    return 0


if __name__ == "__main__":
    sys.exit(main())
