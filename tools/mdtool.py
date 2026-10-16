#!/usr/bin/env python3
"""mdtool: parse and edit large Markdown files by section.

STATUS
------
This is the **origin prototype** of the section model: the scan / resolve /
splice / edit logic that `kb-core/src/section.rs` (the Verus-verified Rust
port) was transcribed from. It is NOT built, packaged, or shipped as part of
`kb` — it lives here for provenance, and its edge-case decisions remain the
reference the Rust tests are checked against. `kb` runs on pure Rust; this
file is only read by humans and `kb-core`'s tests.

Design
------
* ATX headings (# .. ######) define the section tree; headings inside
  fenced code blocks (``` / ~~~) are ignored.
* The file is read once into memory (bytes). Every edit is a byte-range
  splice on that buffer, so memory ~= file size and edits are O(1)
  copies, even for multi-GB documents.
* Saves are atomic: write to a temp file in the same directory, then
  os.replace(). A crash never leaves a half-written file.
* Sections are addressed by dotted number ("3.2") or title path
  ("Installation > Quick Start").

Limitations
-----------
* ATX headings only (# .. ######). Setext headings (a text line underlined
  with ===== or -----) are NOT recognized as section boundaries — a known,
  documented limitation. Documents that mix styles will have setext headings
  treated as ordinary body text.
* Fence-close detection is simplified: a closing fence must use the same
  character as the opener and be at least as long.
"""
from __future__ import annotations

import argparse
import bisect
import difflib
import json
import os
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import List, Optional, Union

HEADING_RE = re.compile(rb"^ {0,3}(#{1,6})(?:[ \t]+(.*?))?[ \t]*$")
FENCE_RE = re.compile(rb"^ {0,3}(`{3,}|~{3,})")


class MDError(Exception):
    pass


@dataclass
class Section:
    level: int
    title: str
    start: int          # byte offset of heading line
    end: int            # byte offset where section ends (exclusive)
    own_end: int        # end of own body: before the very next heading
    start_line: int     # 0-based
    end_line: int       # 0-based, exclusive
    own_end_line: int = 0
    ref: str = ""       # dotted number, e.g. "1.2"

    @property
    def lines(self) -> int:
        return self.end_line - self.start_line

    @property
    def chars(self) -> int:
        return self.end - self.start

    @property
    def own_lines(self) -> int:
        return self.own_end_line - self.start_line

    @property
    def own_chars(self) -> int:
        return self.own_end - self.start


def _scan(data: bytes):
    """One pass: find headings (skipping code fences). O(n)."""
    sections: List[Section] = []
    line_starts: List[int] = []
    in_fence = False
    fence_char = b""
    fence_len = 0
    pos, n = 0, len(data)
    while pos < n:
        line_starts.append(pos)
        nl = data.find(b"\n", pos)
        if nl == -1:
            nl = n
        line = data[pos:nl]
        m = FENCE_RE.match(line)
        if m:
            marker = m.group(1)
            if not in_fence:
                in_fence, fence_char, fence_len = True, marker[:1], len(marker)
            elif marker[:1] == fence_char and len(marker) >= fence_len:
                in_fence = False
        elif not in_fence:
            hm = HEADING_RE.match(line)
            if hm:
                title = (hm.group(2) or b"").decode("utf-8", "replace").strip()
                sections.append(
                    Section(
                        len(hm.group(1)), title, pos, 0, 0, len(line_starts) - 1, 0
                    )
                )
        pos = nl + 1
    return sections, line_starts, n


def _ensure_newline(text: str) -> bytes:
    b = text.encode("utf-8")
    if not b.endswith(b"\n"):
        b += b"\n"
    return b


class MarkdownFile:
    def __init__(self, path: Union[str, Path]):
        self.path = Path(path)
        self._load()

    # ---------- core ----------
    def _load(self):
        self._apply(self.path.read_bytes())

    def _apply(self, data: bytes):
        self.data = data
        self.total_lines = 0
        self.preamble_end = 0
        self.title: Optional[Section] = None
        self.sections: List[Section] = []
        if not data:
            return
        sections, line_starts, n = _scan(data)
        self.total_lines = len(line_starts)
        self.preamble_end = sections[0].start if sections else 0

        # assign section end: until next heading of level <= own
        for i, s in enumerate(sections):
            end = n
            for j in range(i + 1, len(sections)):
                if sections[j].level <= s.level:
                    end = sections[j].start
                    break
            # own body ends at the very next heading of any level
            s.own_end = sections[i + 1].start if i + 1 < len(sections) else n
            s.end = end
            s.end_line = bisect.bisect_left(line_starts, end)
            s.own_end_line = bisect.bisect_left(line_starts, s.own_end)

        # assign dotted refs in document order
        counters = [0] * 6
        for s in sections:
            counters[s.level - 1] += 1
            for k in range(s.level, 6):
                counters[k] = 0
            parts = [str(c) for c in counters[: s.level]]
            # drop leading zero parts (document doesn't start at level 1)
            while len(parts) > 1 and parts[0] == "0":
                parts.pop(0)
            s.ref = ".".join(parts)

        self.sections = sections
        self.title = next((s for s in sections if s.level == 1), None)

    def _splice(self, lo: int, hi: int, new: bytes):
        self._apply(self.data[:lo] + new + self.data[hi:])

    def save(self, out: Optional[Union[str, Path]] = None):
        target = Path(out) if out else self.path
        fd, tmp = tempfile.mkstemp(dir=str(target.parent), prefix=target.name + ".~", suffix=".tmp")
        try:
            with os.fdopen(fd, "wb") as fh:
                fh.write(self.data)
            os.replace(tmp, target)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise

    # ---------- reading ----------
    @property
    def title_body(self) -> bytes:
        """Everything under the title heading (until the next H1)."""
        if not self.title:
            return self.data[: self.preamble_end]
        first_nl = self.data.find(b"\n", self.title.start)
        start = first_nl + 1 if first_nl != -1 else self.title.end
        return self.data[start : self.title.end]

    def section_text(self, ref: Union[str, Section], full: bool = False) -> str:
        """Section's own body (up to its first subsection); full=True
        includes all subsections."""
        s = ref if isinstance(ref, Section) else self.resolve(ref)
        hi = s.end if full else s.own_end
        return self.data[s.start : hi].decode("utf-8", "replace")

    def resolve(self, ref: Union[str, Section]) -> Section:
        if isinstance(ref, Section):
            return ref
        ref = ref.strip()
        if not ref:
            raise MDError("empty section reference")
        if re.fullmatch(r"(?:\d+\.)*\d+", ref):
            for s in self.sections:
                if s.ref == ref:
                    return s
            raise MDError(f"no section numbered {ref!r}")

        parts = [p.strip() for p in ref.split(" > ")]
        # 1) exact title path starting at a top-level section
        cands = [s for s in self.sections if s.level == 1 and s.title.lower() == parts[0].lower()]
        for i, part in enumerate(parts[1:], start=2):
            cands = [s for s in cands if s.level == i and s.title.lower() == part.lower()]
            if not cands:
                break
        if len(cands) == 1:
            return cands[0]
        # 2) unique exact title match anywhere
        exact = [s for s in self.sections if s.title.lower() == parts[-1].lower()]
        if len(exact) == 1:
            return exact[0]
        # 3) unique substring match
        subs = [s for s in self.sections if parts[-1].lower() in s.title.lower()]
        if len(subs) == 1:
            return subs[0]
        if exact or subs or cands:
            names = sorted({s.ref + " " + s.title for s in (cands or exact or subs)})
            raise MDError(f"ambiguous reference {ref!r}: {', '.join(names)}")
        raise MDError(f"no section matching {ref!r}")

    # ---------- editing ----------
    def replace(
        self,
        find: str,
        replacement: str,
        section: Optional[Union[str, Section]] = None,
        use_regex: bool = False,
        count: int = 0,
    ) -> int:
        """Find/replace. Returns number of replacements. count=0 -> all."""
        if section is None:
            lo, hi = 0, len(self.data)
        else:
            s = self.resolve(section)
            lo, hi = s.start, s.end
        chunk = self.data[lo:hi].decode("utf-8", "replace")
        if use_regex:
            new, n = re.compile(find).subn(replacement, chunk)
        else:
            new, n = chunk.replace(find, replacement, count if count else -1), (
                chunk.count(find) if not count else min(count, chunk.count(find))
            )
        if n == 0:
            scope = "whole file" if section is None else f"section {section!r}"
            raise MDError(f"nothing to replace: {find!r} not found ({scope})")
        self._splice(lo, hi, new.encode("utf-8"))
        return n

    def set_section(
        self,
        ref: Union[str, Section],
        text: str,
        keep_heading: bool = False,
    ) -> bytes:
        """Replace a section's content. By default `text` replaces the
        whole section including its heading; with keep_heading the
        original heading line is retained and only the body is replaced.
        Returns the bytes that replaced the section."""
        s = self.resolve(ref)
        body = _ensure_newline(text)
        if keep_heading:
            first_nl = self.data.find(b"\n", s.start)
            head = self.data[s.start : first_nl if first_nl != -1 else s.end]
            body = head + b"\n\n" + body  # blank line after heading
        self._splice(s.start, s.end, body)
        return body

    def append_section(self, ref: Union[str, Section], text: str) -> None:
        """Append text at the end of a section (before the next heading)."""
        s = self.resolve(ref)
        ins = _ensure_newline(text)
        # keep a blank line between existing content and the appended text
        if s.end > s.start and self.data[s.end - 1 : s.end] == b"\n":
            if s.end >= s.start + 1 and self.data[s.end - 2 : s.end - 1] != b"\n":
                ins = b"\n" + ins
        elif s.end > s.start:
            ins = b"\n\n" + ins
        self._splice(s.end, s.end, ins)

    def insert_section(
        self,
        placement: str,
        ref: Optional[str],
        text: str,
        title: str,
        level: Optional[int] = None,
        do_splice: bool = True,
    ):
        """Insert a new section (heading + body) at a position.

        placement: 'before' | 'after' | 'child' | 'top' | 'end'
          before  -> immediately before the referenced section
          after   -> after the referenced section's full span (after its
                     subsections)
          child   -> at the end of the referenced section's span, i.e. as
                     its last subsection
          top     -> before the first heading of the file (after any preamble)
          end     -> at the end of the file
        `text` is the section body and may itself contain subsections at any
        Markdown level (## through ######); they are kept as written.
        Returns (insertion_byte_offset, exact_bytes_inserted)."""
        if placement in ("before", "after", "child"):
            s = self.resolve(ref)
        else:
            s = None
        if level is None:
            level = (s.level + 1 if placement == "child" else s.level if s else 1)
            level = min(level, 6)
        if not 1 <= level <= 6:
            raise MDError(f"heading level must be 1-6 (got {level})")
        if not title or not title.strip():
            raise MDError("empty section title")
        if placement == "end":
            ins = len(self.data)
        elif placement == "top":
            ins = self.preamble_end
        else:
            ins = s.start if placement == "before" else s.end
        parts = ["#" * level + " " + title.strip()]
        if text.strip():
            parts.append("")
            parts.append(text.strip("\n"))
        block = ("\n".join(parts) + "\n").encode("utf-8")
        # keep exactly one blank line on each side (unless at a file edge)
        before = self.data[:ins]
        after = self.data[ins:]
        if before:
            gap = len(before) - len(before.rstrip(b"\n"))
            prefix = b"" if gap >= 2 else b"\n" * (2 - gap)
        else:
            prefix = b""
        if after:
            if after.startswith(b"\n\n"):
                suffix = b""
            elif after.startswith(b"\n"):
                suffix = b"\n"
            else:
                suffix = b"\n\n"
        else:
            suffix = b""
        chunk = prefix + block + suffix
        if do_splice:
            self._splice(ins, ins, chunk)
        return ins, chunk

    # ---------- reporting ----------
    def info_dict(self) -> dict:
        def node(s: Section) -> dict:
            return {
                "ref": s.ref,
                "title": s.title,
                "level": s.level,
                "lines": s.lines,
                "chars": s.chars,
                "own_lines": s.own_lines,
                "own_chars": s.own_chars,
                "start_line_1based": s.start_line + 1,
                "end_line_1based": s.end_line,
                "own_end_line_1based": s.own_end_line,
                "children": [],
            }

        roots: List[dict] = []
        stack: List[dict] = []
        for s in self.sections:
            nd = node(s)
            while stack and stack[-1]["level"] >= s.level:
                stack.pop()
            (stack[-1]["children"] if stack else roots).append(nd)
            stack.append(nd)

        title = None
        if self.title is not None:
            tb = self.title_body
            first_nl = self.data.find(b"\n", self.title.start)
            heading_len = (first_nl + 1 if first_nl != -1 else self.title.end) - self.title.start
            intro = self.data[self.title.start : self.title.own_end].decode("utf-8", "replace")
            title = {
                "ref": self.title.ref,
                "title": self.title.title,
                "content_lines": len(tb.splitlines()),
                "content_chars": len(tb),
                "heading_line_chars": heading_len,
                "intro": intro,   # title line + text until the first section
            }
        preamble = None
        if self.preamble_end > 0:
            pb = self.data[: self.preamble_end]
            preamble = {"lines": len(pb.splitlines()), "chars": len(pb), "content": pb.decode("utf-8", "replace")}
        return {
            "path": str(self.path),
            "bytes": len(self.data),
            "lines": self.total_lines,
            "title": title,
            "preamble": preamble,
            "sections": roots,
        }


# ---------------- CLI ----------------

def _diff(old: bytes, new: bytes, label: str) -> None:
    a = old.decode("utf-8", "replace").splitlines(keepends=True)
    b = new.decode("utf-8", "replace").splitlines(keepends=True)
    sys.stdout.writelines(
        difflib.unified_diff(a, b, fromfile=f"{label} (before)", tofile=f"{label} (after)")
    )


def _insert_preview(data: bytes, ins: int, chunk: bytes, ctx: int = 5) -> None:
    """Show the insertion as a unified diff with a few lines of context."""
    lines = data.decode("utf-8", "replace").splitlines(keepends=True)
    li = data[:ins].count(b"\n")  # 0-based index of the line the block precedes
    lo, hi = max(0, li - ctx), min(len(lines), li + ctx)
    old = lines[lo:hi]
    chunk_lines = chunk.decode("utf-8", "replace").splitlines(keepends=True)
    new = old[: li - lo] + chunk_lines + old[li - lo:]
    print(f"would insert at line {li + 1}:")
    sys.stdout.writelines(
        difflib.unified_diff(old, new, fromfile=f"lines {lo + 1}-{hi}", tofile="with new section")
    )


def _diff_sections(a: "MarkdownFile", b: "MarkdownFile") -> None:
    """Section-level summary of differences between two parsed documents."""
    ka = [(s.level, s.title) for s in a.sections]
    kb = [(s.level, s.title) for s in b.sections]
    print(f"{a.path.name}: {a.path.stat().st_size if False else len(a.data):,} B, {a.total_lines:,} L, {len(ka)} sections")
    print(f"{b.path.name}: {len(b.data):,} B, {b.total_lines:,} L, {len(kb)} sections")
    if not ka and not kb:
        print("(no sections in either file; use --unified for a line-level diff)")
        return
    sm = difflib.SequenceMatcher(a=ka, b=kb, autojunk=False)
    n_same = n_chg = n_add = n_rm = 0

    def fmt(s: "Section") -> str:
        return f"{s.ref:<10} {s.title[:60]:<60}"

    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            for k in range(i2 - i1):
                sa, sb = a.sections[i1 + k], b.sections[j1 + k]
                if a.data[sa.start:sa.end] == b.data[sb.start:sb.end]:
                    n_same += 1
                    print(f"  = {fmt(sb)}  (unchanged)")
                else:
                    n_chg += 1
                    print(
                        f"  ~ {fmt(sb)}  was {sa.own_lines} L / {sa.own_chars} C"
                        f"  ->  {sb.own_lines} L / {sb.own_chars} C"
                    )
        elif tag == "delete":
            for s in a.sections[i1:i2]:
                n_rm += 1
                print(f"  - {fmt(s)}  (removed; was {s.own_lines} L / {s.own_chars} C)")
        elif tag == "insert":
            for s in b.sections[j1:j2]:
                n_add += 1
                print(f"  + {fmt(s)}  (added; {s.own_lines} L / {s.own_chars} C)")
        else:  # replace
            for s in a.sections[i1:i2]:
                n_rm += 1
                print(f"  - {fmt(s)}  (removed; was {s.own_lines} L / {s.own_chars} C)")
            for s in b.sections[j1:j2]:
                n_add += 1
                print(f"  + {fmt(s)}  (added; {s.own_lines} L / {s.own_chars} C)")
    print(f"summary: {n_same} unchanged, {n_chg} changed, {n_add} added, {n_rm} removed")


def _unified(a: "MarkdownFile", b: "MarkdownFile") -> None:
    la = a.data.decode("utf-8", "replace").splitlines(keepends=True)
    lb = b.data.decode("utf-8", "replace").splitlines(keepends=True)
    d = list(difflib.unified_diff(la, lb, fromfile=str(a.path), tofile=str(b.path)))
    sys.stdout.writelines(d)
    adds = sum(1 for l in d if l.startswith("+") and not l.startswith("+++"))
    rms = sum(1 for l in d if l.startswith("-") and not l.startswith("---"))
    print(f"summary: +{adds} / -{rms} lines")


def _read_text(args) -> str:
    if getattr(args, "text_file", None) is not None:
        if args.text_file == "-":
            return sys.stdin.read()
        return Path(args.text_file).read_text(encoding="utf-8")
    if getattr(args, "text", None) is not None:
        return args.text
    raise MDError("one of --text or --text-file is required")


def _print_info(d: dict, full: bool = False) -> None:
    print(f"file:  {d['path']}  ({d['bytes']:,} bytes, {d['lines']:,} lines)")
    if d["title"]:
        t = d["title"]
        print(f"title: {t['title']}   [ref {t['ref']}]  (body: {t['content_lines']:,} lines, {t['content_chars']:,} chars)")
        if t.get("intro"):
            print("--- title through first section ---")
            sys.stdout.write(t["intro"])
            if not t["intro"].endswith("\n"):
                print()
            print("---")
    else:
        print("title: (no level-1 heading found)")
    if d["preamble"]:
        p = d["preamble"]
        print(f"preamble (before first heading): {p['lines']:,} lines, {p['chars']:,} chars")
        if p.get("content"):
            print("--- preamble ---")
            sys.stdout.write(p["content"])
            if not p["content"].endswith("\n"):
                print()
            print("---")
    print("sections:")

    def walk(nodes, indent="  "):
        for n in nodes:
            pad = " " * 4 if indent == "  " else " " * 4
            lines = n["lines"] if full else n["own_lines"]
            chars = n["chars"] if full else n["own_chars"]
            end_line = n["end_line_1based"] if full else n["own_end_line_1based"]
            print(
                f"  {n['ref']:<10} {n['title'][:60]:<60} "
                f"{lines:>7,} L / {chars:>9,} C   (lines {n['start_line_1based']}-{end_line})"
            )
            walk(n["children"])

    walk(d["sections"])


def main(argv: Optional[List[str]] = None) -> int:
    if argv is None:
        argv = sys.argv[1:]
    p = argparse.ArgumentParser(prog="mdtool", description=__doc__.splitlines()[0])
    if not argv or argv[0] in ("-h", "--help"):
        p.print_help()
        return 0 if not argv else 0
    cmd_names = ["info", "section", "replace", "set-section", "append-section", "insert-section", "diff"]
    # bare filename (or any non-flag, non-command first arg) defaults to `info`
    if argv[0] not in cmd_names and not argv[0].startswith("-"):
        argv = ["info"] + argv
    sub = p.add_subparsers(dest="cmd", required=True)

    sp = sub.add_parser("info", help="show title, structure, per-section line/char counts")
    sp.add_argument("file")
    sp.add_argument("--json", action="store_true")
    sp.add_argument("--full", action="store_true",
                    help="count each section's full span (incl. subsections); "
                         "default counts only its own body")

    sp = sub.add_parser("section", help="print a section's own body (without subsections)")
    sp.add_argument("file")
    sp.add_argument("ref")
    sp.add_argument("--full", action="store_true",
                    help="include subsections (the section's entire span)")

    sp = sub.add_parser("replace", help="find & replace (whole file or one section)")
    sp.add_argument("file")
    sp.add_argument("--find", required=True)
    sp.add_argument("--replace", required=True)
    sp.add_argument("--section", help="limit to this section (ref like '1.2' or 'A > B')")
    sp.add_argument("--regex", action="store_true", help="treat --find as a regex")
    sp.add_argument("--count", type=int, default=0, help="max replacements (0 = all)")
    sp.add_argument("--dry-run", action="store_true")
    sp.add_argument("--out", help="write result to this file instead of in place")

    sp = sub.add_parser("set-section", help="replace a section with new text")
    sp.add_argument("file")
    sp.add_argument("ref")
    sp.add_argument("--text", help="new section text (includes its own heading)")
    sp.add_argument("--text-file", help="read new text from file ('-' for stdin)")
    sp.add_argument("--keep-heading", action="store_true",
                    help="keep the original heading; replace only the body")
    sp.add_argument("--dry-run", action="store_true")
    sp.add_argument("--out")

    sp = sub.add_parser("append-section", help="append text to the end of a section")
    sp.add_argument("file")
    sp.add_argument("ref")
    sp.add_argument("--text")
    sp.add_argument("--text-file")
    sp.add_argument("--dry-run", action="store_true")
    sp.add_argument("--out")

    sp = sub.add_parser("insert-section", help="insert a new section at before/after/child/top/end of a ref")
    sp.add_argument("file")
    sp.add_argument("--placement", required=True, choices=["before", "after", "child", "top", "end"])
    sp.add_argument("--ref", help="target section (required unless placement is top/end)")
    sp.add_argument("--title", required=True, help="title of the new section")
    sp.add_argument("--level", type=int, help="heading level 1-6 (default: inferred from placement)")
    sp.add_argument("--text", help="section body; may contain subsections at any level")
    sp.add_argument("--text-file", help="read body from file ('-' for stdin)")
    sp.add_argument("--dry-run", action="store_true")
    sp.add_argument("--out")

    sp = sub.add_parser("diff", help="compare two files: section-level summary, or --unified for line-level")
    sp.add_argument("file_a")
    sp.add_argument("file_b")
    sp.add_argument("--unified", action="store_true", help="full line-level unified diff")

    args = p.parse_args(argv)
    try:
        if args.cmd == "info":
            d = MarkdownFile(args.file).info_dict()
            if args.json:
                print(json.dumps(d, indent=2, ensure_ascii=False))
            else:
                _print_info(d, full=args.full)

        elif args.cmd == "section":
            print(MarkdownFile(args.file).section_text(args.ref, full=args.full), end="")

        elif args.cmd == "replace":
            doc = MarkdownFile(args.file)
            before = doc.data
            n = doc.replace(args.find, args.replace, args.section, args.regex, args.count)
            if args.dry_run:
                _diff(before, doc.data, "whole file" if not args.section else f"section {args.section}")
                print(f"--- would replace {n} occurrence(s)")
            else:
                doc.save(args.out)
                print(f"replaced {n} occurrence(s) -> {args.out or doc.path}")

        elif args.cmd == "set-section":
            text = _read_text(args)
            doc = MarkdownFile(args.file)
            s = doc.resolve(args.ref)
            before_slice = doc.data[s.start : s.end]
            after_slice = doc.set_section(s, text, args.keep_heading)
            if args.dry_run:
                _diff(before_slice, after_slice, f"section {s.ref}")
                print("--- no changes written")
            else:
                doc.save(args.out)
                print(f"rewrote section {s.ref} -> {args.out or doc.path}")

        elif args.cmd == "append-section":
            text = _read_text(args)
            doc = MarkdownFile(args.file)
            s = doc.resolve(args.ref)
            before_slice = doc.data[s.start : s.end]
            doc.append_section(s, text)
            if args.dry_run:
                new = doc.resolve(s.ref)
                _diff(before_slice, doc.data[new.start : new.end], f"section {s.ref}")
                print("--- no changes written")
            else:
                doc.save(args.out)
                print(f"appended to section {s.ref} -> {args.out or doc.path}")

        elif args.cmd == "insert-section":
            if args.placement in ("before", "after", "child") and not args.ref:
                raise MDError(f"--ref is required with placement '{args.placement}'")
            text = _read_text(args) if (args.text is not None or args.text_file is not None) else ""
            doc = MarkdownFile(args.file)
            ins, chunk = doc.insert_section(args.placement, args.ref, text, args.title, args.level, do_splice=not args.dry_run)
            if args.dry_run:
                _insert_preview(doc.data, ins, chunk)
                print("--- no changes written")
            else:
                line = doc.data[:ins].count(b"\n") + 1
                doc.save(args.out)
                print(f"inserted section at line {line} -> {args.out or doc.path}")

        elif args.cmd == "diff":
            a = MarkdownFile(args.file_a)
            b = MarkdownFile(args.file_b)
            if args.unified:
                _unified(a, b)
            else:
                _diff_sections(a, b)
    except (MDError, OSError) as e:
        print(f"mdtool: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
