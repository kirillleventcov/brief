#!/usr/bin/env python3
"""Count tokens for Markdown, Brief, and LLM-compiled output.

Utility for eyeballing the token cost of a document in different forms.
Not part of the main build.

Tokenizers:

  - tiktoken (default, offline) — for OpenAI/GPT models.
        pip install tiktoken
  - Anthropic count_tokens API (--anthropic) — for Claude models.
        pip install anthropic
        export ANTHROPIC_API_KEY=...

Pick one tokenizer per run. To compare both, run twice.

Examples:
    scripts/count_tokens.py LearnXinYminutes.brf
    scripts/count_tokens.py --compile LearnXinYminutes.brf notes.md
    scripts/count_tokens.py --model gpt-5.5 --compile docs/*.brf
    scripts/count_tokens.py --anthropic --compile LearnXinYminutes.brf
    scripts/count_tokens.py --anthropic --anthropic-model claude-sonnet-4-6 docs/*.brf

The default tiktoken encoding is o200k_base, shared by the gpt-4o / gpt-4.1 /
gpt-5 families. If --model names a release tiktoken doesn't recognize yet
(e.g. gpt-5.5), the script falls back to o200k_base for any gpt-4/5/o-series
name.

The Anthropic count_tokens endpoint is free but rate-limited (100 RPM at
tier 1). With --compile that's up to 3 calls per .md file (raw + brf + llm)
and 2 per .brf, so large batches may hit the limit.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable


@dataclass
class Row:
    path: Path
    kind: str
    bytes_raw: int
    tok_raw: int
    bytes_brf: int | None = None
    tok_brf: int | None = None
    bytes_llm: int | None = None
    tok_llm: int | None = None
    note: str = ""


CountFn = Callable[[str], int]


def build_tiktoken_counter(
    model: str | None, encoding: str | None
) -> tuple[CountFn, str]:
    try:
        import tiktoken
    except ImportError:
        sys.exit("error: tiktoken not installed. run: pip install tiktoken")

    if model:
        try:
            enc = tiktoken.encoding_for_model(model)
            label = f"tiktoken model={model}"
        except KeyError:
            # tiktoken's model map lags new releases. gpt-4.x, gpt-5.x, and the
            # o-series all use o200k_base, so fall back to that for those names.
            m = model.lower()
            if m.startswith(("gpt-4", "gpt-5", "o1", "o3", "o4")):
                enc = tiktoken.get_encoding("o200k_base")
                label = f"tiktoken model={model} -> o200k_base (fallback)"
            else:
                sys.exit(
                    f"error: unknown model {model!r}. "
                    f"pass --encoding o200k_base (or another encoding) explicitly."
                )
    else:
        enc_name = encoding or "o200k_base"
        try:
            enc = tiktoken.get_encoding(enc_name)
        except ValueError:
            sys.exit(f"error: unknown encoding {enc_name!r}")
        label = f"tiktoken encoding={enc_name}"

    # disallowed_special=() so docs that legitimately contain "<|endoftext|>"
    # don't blow up.
    return (lambda text: len(enc.encode(text, disallowed_special=()))), label


def build_anthropic_counter(model: str) -> tuple[CountFn, str]:
    try:
        import anthropic
    except ImportError:
        sys.exit("error: anthropic not installed. run: pip install anthropic")

    if not os.environ.get("ANTHROPIC_API_KEY"):
        sys.exit("error: ANTHROPIC_API_KEY is not set")

    client = anthropic.Anthropic()

    def count_fn(text: str) -> int:
        # The endpoint rejects empty content; short-circuit so we don't waste a call.
        if not text:
            return 0
        try:
            resp = client.messages.count_tokens(
                model=model,
                messages=[{"role": "user", "content": text}],
            )
        except anthropic.APIError as e:
            raise RuntimeError(f"anthropic count_tokens failed: {e}") from e
        return resp.input_tokens

    return count_fn, f"anthropic model={model}"


def kind_for(path: Path) -> str:
    s = path.suffix.lower()
    if s == ".md":
        return "md"
    if s == ".brf":
        return "brf"
    return "other"


def compile_llm(brief_bin: str, path: Path, kind: str) -> str:
    """Run `brief compile --target=llm` and return stdout."""
    args = [brief_bin, "compile", str(path), "--target=llm"]
    if kind == "md":
        args.append("--convert")
    proc = subprocess.run(args, capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"brief exited {proc.returncode}")
    return proc.stdout


def convert_to_brief(brief_bin: str, path: Path) -> str:
    """Run `brief convert FILE --stdout` and return the Brief text."""
    proc = subprocess.run(
        [brief_bin, "convert", str(path), "--stdout"],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"brief exited {proc.returncode}")
    return proc.stdout


def render(rows: list[Row], compile_mode: bool, enc_label: str) -> str:
    headers = ["file", "kind", "raw-tok"]
    if compile_mode:
        headers += ["brf-tok", "llm-tok", "brf/raw", "llm/raw"]
    headers.append("note")

    def fmt_tok(n: int | None) -> str:
        return f"{n:,}" if n is not None else "-"

    def fmt_ratio(num: int | None, denom: int) -> str:
        if num is None or denom == 0:
            return "-"
        return f"{num / denom:.2f}x"

    table: list[list[str]] = [headers]
    for r in rows:
        cols = [str(r.path), r.kind, f"{r.tok_raw:,}"]
        if compile_mode:
            cols += [
                fmt_tok(r.tok_brf),
                fmt_tok(r.tok_llm),
                fmt_ratio(r.tok_brf, r.tok_raw),
                fmt_ratio(r.tok_llm, r.tok_raw),
            ]
        cols.append(r.note)
        table.append(cols)

    widths = [max(len(row[i]) for row in table) for i in range(len(headers))]
    out = []
    for i, row in enumerate(table):
        out.append("  ".join(c.ljust(widths[j]) for j, c in enumerate(row)).rstrip())
        if i == 0:
            out.append("  ".join("-" * widths[j] for j in range(len(headers))))

    if len(rows) > 1:
        total_raw = sum(r.tok_raw for r in rows)
        line = f"\ntotal raw: {total_raw:,}"
        if compile_mode:
            brf_rows = [r for r in rows if r.tok_brf is not None]
            if brf_rows:
                total_brf = sum(r.tok_brf for r in brf_rows)
                base = sum(r.tok_raw for r in brf_rows)
                line += (
                    f"  |  total brf: {total_brf:,} ({total_brf / base:.2f}x)"
                    if base
                    else ""
                )
            llm_rows = [r for r in rows if r.tok_llm is not None]
            if llm_rows:
                total_llm = sum(r.tok_llm for r in llm_rows)
                base = sum(r.tok_raw for r in llm_rows)
                line += (
                    f"  |  total llm: {total_llm:,} ({total_llm / base:.2f}x)"
                    if base
                    else ""
                )
        out.append(line)

    out.append(f"\n[{enc_label}]")
    return "\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Count tokens for Markdown / Brief / LLM-compiled output (tiktoken or Anthropic API).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    ap.add_argument("files", nargs="+", type=Path)
    ap.add_argument(
        "--compile",
        action="store_true",
        help=(
            "show the full pipeline: for .md inputs, run `brief convert --stdout` (brf-tok) "
            "and `brief compile --convert --target=llm` (llm-tok); for .brf inputs, only "
            "the llm-tok column is populated since brf-tok would equal raw-tok"
        ),
    )
    ap.add_argument("--model", help="tiktoken model name, e.g. gpt-4o")
    ap.add_argument("--encoding", help="tiktoken encoding name (default: o200k_base)")
    ap.add_argument(
        "--anthropic",
        action="store_true",
        help="use the Anthropic count_tokens API instead of tiktoken. requires ANTHROPIC_API_KEY.",
    )
    ap.add_argument(
        "--anthropic-model",
        default="claude-opus-4-7",
        help="Claude model id used by --anthropic (default: claude-opus-4-7)",
    )
    ap.add_argument(
        "--brief-bin",
        default=shutil.which("brief") or "brief",
        help="path to the `brief` binary (default: first on PATH)",
    )
    args = ap.parse_args()

    if args.anthropic and (args.model or args.encoding):
        sys.exit("error: --anthropic is mutually exclusive with --model / --encoding")

    if args.anthropic:
        count_fn, enc_label = build_anthropic_counter(args.anthropic_model)
    else:
        count_fn, enc_label = build_tiktoken_counter(args.model, args.encoding)

    if (
        args.compile
        and not shutil.which(args.brief_bin)
        and not Path(args.brief_bin).is_file()
    ):
        sys.exit(
            f"error: --compile requested but brief binary not found at {args.brief_bin!r}"
        )

    rows: list[Row] = []
    had_error = False
    for path in args.files:
        if not path.is_file():
            print(f"warn: skipping {path} (not a file)", file=sys.stderr)
            had_error = True
            continue
        try:
            raw = path.read_text(encoding="utf-8")
        except UnicodeDecodeError as e:
            print(f"warn: skipping {path} ({e})", file=sys.stderr)
            had_error = True
            continue

        kind = kind_for(path)
        row = Row(path=path, kind=kind, bytes_raw=len(raw.encode("utf-8")), tok_raw=0)
        try:
            row.tok_raw = count_fn(raw)
        except RuntimeError as e:
            row.note = str(e)
            had_error = True
            rows.append(row)
            continue

        if args.compile:
            if kind == "md":
                try:
                    converted = convert_to_brief(args.brief_bin, path)
                    row.bytes_brf = len(converted.encode("utf-8"))
                    row.tok_brf = count_fn(converted)
                except RuntimeError as e:
                    row.note = f"convert failed: {e}"
                    had_error = True
            if kind in {"brf", "md"}:
                try:
                    compiled = compile_llm(args.brief_bin, path, kind)
                    row.bytes_llm = len(compiled.encode("utf-8"))
                    row.tok_llm = count_fn(compiled)
                except RuntimeError as e:
                    sep = "; " if row.note else ""
                    row.note = f"{row.note}{sep}compile failed: {e}"
                    had_error = True
            elif not row.note:
                row.note = "compile skipped (not .md/.brf)"

        rows.append(row)

    if not rows:
        return 1

    print(render(rows, args.compile, enc_label))
    return 1 if had_error else 0


if __name__ == "__main__":
    sys.exit(main())
