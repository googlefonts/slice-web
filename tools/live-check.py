#!/usr/bin/env python3
"""Slice a font on the deployed site, through a shared link, and check what comes back.

Question this answers
---------------------
    After a push, does the app people actually use -- the one on GitHub Pages, not a local
    build -- produce a correct font from a user's own link?

`browser-slice-test.py` drives a local build with the bundled sample. This drives the
deployed page: it opens a link exactly as a user would paste it (settings in the address
bar), gives the page the font through its file input, presses Slice, and keeps the bytes
the page hands back as a download. The status bar's build stamp is printed, so the run
says which commit it tested. Then the font goes to the two independent checks:

    tools/overlap-check.py      is anything still overlapping (only if the link asks
                                for overlap removal)
    tools/kerning-compare.py    is every character pair placed as the variable font
                                places it (--ours, with the link's axis settings)

Usage
-----
    tools/live-check.py FONT                    # the Google Sans Flex report's link
    tools/live-check.py FONT --url 'https://felipesanches.github.io/slice-web/app/?axes=...'
    tools/live-check.py FONT --keep out.ttf     # keep the font the page produced

Needs chromium and network access. Exits 0 when the page produced a font and both
checks pass, 1 otherwise. The DevTools client is browser-slice-test.py's.
"""

from __future__ import annotations

import argparse
import base64
import importlib.util
import json
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
TOOLS = REPO_ROOT / "tools"

# The link from the October 2026 Google Sans Flex report, verbatim.
REPORT_URL = (
    "https://felipesanches.github.io/slice-web/app/"
    "?axes=opsz=18,wdth=80,wght=900,GRAD=0,ROND=0,slnt=0"
    "&n1=Google%20Sans%20Flex%20Condensed&n2=Black"
    "&n3=4.005%3BGOOG%3BGoogleSansFlex-Condensed-Black"
    "&n4=Google%20Sans%20Flex%20Condensed%20Black"
    "&n6=GoogleSansFlex-Condensed-Black&overlaps=1"
)


def load_browser_test():
    spec = importlib.util.spec_from_file_location(
        "browser_slice_test", TOOLS / "browser-slice-test.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def slice_on_page(bst, url: str, font: Path, out: Path) -> dict:
    """Open `url`, load `font`, press Slice, write what comes back to `out`."""
    port = bst.free_port()
    with tempfile.TemporaryDirectory(prefix="live-check-", ignore_cleanup_errors=True) as profile:
        chrome = subprocess.Popen(
            [bst.find_browser(), "--headless", "--disable-gpu", "--no-sandbox",
             f"--remote-debugging-port={port}", f"--user-data-dir={profile}",
             "--no-first-run", "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            target = {}

            def up() -> bool:
                try:
                    with urllib.request.urlopen(
                            f"http://127.0.0.1:{port}/json/list", timeout=1) as response:
                        for page in json.load(response):
                            if page.get("type") == "page":
                                target.update(page)
                                return True
                except Exception:
                    pass
                return False

            bst.wait_for(up, "the browser's debugging endpoint")
            page = bst.Devtools(target["webSocketDebuggerUrl"])
            for domain in ("Page", "Runtime", "DOM"):
                page.call(f"{domain}.enable")
            page.call("Page.navigate", url=url)
            bst.wait_for(lambda: page.evaluate(
                "!!document.querySelector('input[type=file]')", timeout=10), "the page", 60)

            root = page.call("DOM.getDocument")["root"]["nodeId"]
            node = page.call("DOM.querySelector", nodeId=root,
                             selector="input[type=file]")["nodeId"]
            page.call("DOM.setFileInputFiles", nodeId=node, files=[str(font.resolve())])
            bst.wait_for(lambda: page.evaluate(
                "document.querySelectorAll('.axis-editor tbody tr').length", timeout=10) > 0,
                "the font to load", 120)

            seen = {
                "status": " ".join(page.evaluate(
                    "document.querySelector('.statusbar').innerText").split()),
                "axes": page.evaluate("[...document.querySelectorAll("
                                      "'.axis-editor tbody tr input')].map(i => i.value)"),
                "names": page.evaluate("[...document.querySelectorAll("
                                       "'.name-editor tbody tr input')].map(i => i.value)"),
                "remove_overlaps": page.evaluate("[...document.querySelectorAll("
                                                 "'.option input[type=checkbox]')][0].checked"),
            }

            page.evaluate(bst.CAPTURE_DOWNLOAD)
            page.evaluate("document.querySelector('button.slice').click()")
            bst.wait_for(
                lambda: page.evaluate("!!window.__sliceCaptured", timeout=20)
                or page.evaluate("!!document.querySelector('.modal.error')", timeout=20),
                "the slice", 600, 1.0)
            error = page.evaluate("(document.querySelector('.modal.error') || {})"
                                  ".innerText || null")
            if error:
                raise SystemExit(f"the page reported an error: {error}")
            out.write_bytes(base64.b64decode(page.evaluate(bst.READ_CAPTURED, timeout=300)))
            seen["saved"] = " ".join(page.evaluate(
                "document.querySelector('.statusbar').innerText").split())
            page.close()
            return seen
        finally:
            chrome.terminate()
            chrome.wait(timeout=30)


def axis_arguments(url: str) -> list[str]:
    """The link's `axes=` parameter as `--axis` arguments, which share its syntax."""
    query = urllib.parse.parse_qs(urllib.parse.urlparse(url).query)
    axes = query.get("axes", [""])[0]
    return [f"--axis={item}" for item in axes.split(",") if item]


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("font", type=Path)
    parser.add_argument("--url", default=REPORT_URL)
    parser.add_argument("--keep", type=Path, help="keep the font the page produced here")
    args = parser.parse_args()

    bst = load_browser_test()
    if not bst.find_browser():
        sys.exit("needs chromium")

    with tempfile.TemporaryDirectory(prefix="live-check-font-") as scratch:
        out = args.keep or Path(scratch) / "sliced-by-the-page.ttf"
        seen = slice_on_page(bst, args.url, args.font, out)
        print(f"page:      {seen['status']}")
        print(f"axes:      {' '.join(seen['axes'])}")
        print(f"names:     {' | '.join(n for n in seen['names'] if n)}")
        print(f"overlaps:  {'removed' if seen['remove_overlaps'] else 'kept'}")
        print(f"saved:     {seen['saved']} -- {out.stat().st_size} bytes")

        failed = False
        if seen["remove_overlaps"]:
            print("\n$ tools/overlap-check.py", flush=True)
            failed |= subprocess.run([str(TOOLS / "overlap-check.py"), str(out)]).returncode != 0
        print("\n$ tools/kerning-compare.py --ours", flush=True)
        failed |= subprocess.run(
            [str(TOOLS / "kerning-compare.py"), str(args.font), "--ours", str(out),
             *axis_arguments(args.url)]).returncode != 0
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
