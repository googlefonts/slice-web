#!/usr/bin/env python3
"""Drive the real interface in a browser, slice a font, and check what comes out.

Question this answers
---------------------
    If someone opens the page, fills in the Axis Editor and presses Slice, do they get a
    font file, and is it the font they asked for?

`tools/browser-smoke.sh` proves the application starts and reads a font. `cargo test`
proves the engine is right. Neither covers the path between them: the click handler, the
job the interface builds from the editors, and the Blob handed back as a download. This
does, by driving Chromium over the DevTools protocol.

Rather than intercept an actual download, it wraps `URL.createObjectURL` before pressing
Slice, so the exact bytes the page produced come back here. Those bytes are then written
out and read with `slice-cli`, which is a genuinely independent check: the browser made
the font, and the native engine has to agree it is a font, with the axes that were asked
for.

Usage
-----
    tools/browser-slice-test.py                 # build, then run
    tools/browser-slice-test.py --no-build      # use whatever is in dist/

Needs chromium (or chrome) and a built `slice` binary; it builds both unless told not to.
Exits 0 on success, 1 on failure, and 0 with a message when no browser is available.

The DevTools client here is deliberately about a hundred lines of socket code rather than
a dependency: this repository otherwise needs nothing but a Rust toolchain and a browser,
and that is worth keeping.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent


# --------------------------------------------------------------------- websocket

class WebSocket:
    """The smallest WebSocket client that can carry DevTools traffic.

    Text frames only, no continuation, no compression. DevTools speaks JSON over text
    frames and never negotiates an extension, so that is all that is needed.
    """

    def __init__(self, url: str):
        _, _, rest = url.partition("://")
        hostport, _, path = rest.partition("/")
        host, _, port = hostport.partition(":")
        self.sock = socket.create_connection((host, int(port or 80)), timeout=30)
        self.buffer = b""

        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET /{path} HTTP/1.1\r\n"
            f"Host: {hostport}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            "\r\n"
        )
        self.sock.sendall(request.encode())

        # Read past the end of the handshake response.
        while b"\r\n\r\n" not in self.buffer:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise RuntimeError("the browser closed the connection during the handshake")
            self.buffer += chunk
        head, _, rest_bytes = self.buffer.partition(b"\r\n\r\n")
        if b"101" not in head.split(b"\r\n")[0]:
            raise RuntimeError(f"unexpected handshake response: {head!r}")
        self.buffer = rest_bytes

    def send(self, text: str) -> None:
        payload = text.encode()
        header = bytearray([0x81])  # FIN + text
        mask = os.urandom(4)
        length = len(payload)
        if length < 126:
            header.append(0x80 | length)
        elif length < (1 << 16):
            header.append(0x80 | 126)
            header += struct.pack(">H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", length)
        header += mask
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(bytes(header) + masked)

    def _read(self, count: int) -> bytes:
        while len(self.buffer) < count:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise RuntimeError("the browser closed the connection")
            self.buffer += chunk
        out, self.buffer = self.buffer[:count], self.buffer[count:]
        return out

    def recv(self) -> str:
        while True:
            first, second = self._read(2)
            opcode = first & 0x0F
            length = second & 0x7F
            if length == 126:
                (length,) = struct.unpack(">H", self._read(2))
            elif length == 127:
                (length,) = struct.unpack(">Q", self._read(8))
            payload = self._read(length)
            if opcode == 0x8:  # close
                raise RuntimeError("the browser closed the connection")
            if opcode == 0x9:  # ping; reply and keep reading
                self.sock.sendall(b"\x8a\x80" + os.urandom(4))
                continue
            if opcode in (0x1, 0x2):
                return payload.decode("utf-8", "replace")

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


# ------------------------------------------------------------------------- CDP

class Devtools:
    def __init__(self, url: str):
        self.ws = WebSocket(url)
        self.next_id = 0

    def call(self, method: str, timeout: float = 60.0, **params):
        self.next_id += 1
        message_id = self.next_id
        self.ws.send(json.dumps({"id": message_id, "method": method, "params": params}))
        deadline = time.time() + timeout
        while time.time() < deadline:
            message = json.loads(self.ws.recv())
            if message.get("id") != message_id:
                continue  # an event, or another call's reply
            if "error" in message:
                raise RuntimeError(f"{method} failed: {message['error']}")
            return message.get("result", {})
        raise TimeoutError(f"{method} did not answer within {timeout}s")

    def evaluate(self, expression: str, timeout: float = 60.0):
        """Evaluate JavaScript, awaiting a promise if one comes back."""
        result = self.call(
            "Runtime.evaluate",
            timeout=timeout,
            expression=expression,
            awaitPromise=True,
            returnByValue=True,
        )
        if result.get("exceptionDetails"):
            details = result["exceptionDetails"]
            text = details.get("exception", {}).get("description") or details.get("text")
            raise RuntimeError(f"JavaScript threw: {text}")
        return result.get("result", {}).get("value")

    def close(self) -> None:
        self.ws.close()


def wait_for(predicate, what: str, timeout: float = 30.0, interval: float = 0.25):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return
        time.sleep(interval)
    raise TimeoutError(f"timed out waiting for {what}")


# ------------------------------------------------------------------------ steps

# Setting an input's `value` property directly does not tell a framework anything. React
# and Leptos both listen for the event, so the value goes in through the native setter
# and an `input` event is dispatched by hand.
SET_INPUT = """
(function (selector, value) {
  const field = document.querySelector(selector);
  if (!field) { throw new Error('no element matching ' + selector); }
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype, 'value').set;
  setter.call(field, value);
  field.dispatchEvent(new Event('input', { bubbles: true }));
  return field.value;
})
"""

# Capture whatever the page hands to the browser as a download, without letting the
# download itself happen.
CAPTURE_DOWNLOAD = """
(function () {
  window.__sliceCaptured = null;
  const original = URL.createObjectURL;
  URL.createObjectURL = function (blob) {
    window.__sliceCaptured = blob;
    return original.call(URL, blob);
  };
  // A download in headless Chromium is noise; the bytes are what matter.
  HTMLAnchorElement.prototype.click = function () {};
})()
"""

READ_CAPTURED = """
(async function () {
  const blob = window.__sliceCaptured;
  if (!blob) { return null; }
  const buffer = await blob.arrayBuffer();
  const bytes = new Uint8Array(buffer);
  let binary = '';
  for (let i = 0; i < bytes.length; i++) { binary += String.fromCharCode(bytes[i]); }
  return btoa(binary);
})()
"""


def sample_font() -> Path:
    """The sample the application ships, located from `files.rs` rather than guessed."""
    source = (REPO_ROOT / "crates" / "slice-web" / "src" / "files.rs").read_text()
    match = re.search(r'SAMPLE_NAME: &str = "(.*)";', source)
    if not match:
        raise SystemExit("could not read SAMPLE_NAME from crates/slice-web/src/files.rs")
    return REPO_ROOT / "web" / "fonts" / match.group(1)


def find_browser() -> str | None:
    for candidate in ("chromium", "chromium-browser", "google-chrome", "chrome"):
        found = shutil.which(candidate)
        if found:
            return found
    return None


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-build", action="store_true", help="use the existing dist/")
    parser.add_argument("--keep", action="store_true", help="keep the produced font file")
    args = parser.parse_args()

    browser = find_browser()
    if browser is None:
        print("no chromium/chrome on PATH; skipping the browser slice test")
        return 0

    if not args.no_build:
        subprocess.run(["./build.sh"], cwd=REPO_ROOT, check=True, stdout=subprocess.DEVNULL)
    if not (REPO_ROOT / "dist" / "pkg" / "slice_web_bg.wasm").exists():
        print("dist/ has not been built; run ./build.sh", file=sys.stderr)
        return 1

    subprocess.run(
        ["cargo", "build", "-p", "slice-cli"],
        cwd=REPO_ROOT,
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    slice_cli = REPO_ROOT / "target" / "debug" / "slice"

    http_port = free_port()
    cdp_port = free_port()
    workdir = Path(tempfile.mkdtemp(prefix="slice-browser-test-"))
    server = None
    chrome = None
    devtools = None

    try:
        server = subprocess.Popen(
            [sys.executable, "-m", "http.server", "--directory", "dist", str(http_port)],
            cwd=REPO_ROOT,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        def server_up() -> bool:
            try:
                urllib.request.urlopen(
                    f"http://127.0.0.1:{http_port}/index.html", timeout=1
                )
                return True
            except Exception:
                return False

        wait_for(server_up, "the static server")

        chrome = subprocess.Popen(
            [
                browser,
                "--headless",
                "--disable-gpu",
                "--no-sandbox",
                f"--remote-debugging-port={cdp_port}",
                f"--user-data-dir={workdir / 'profile'}",
                "--no-first-run",
                "--disable-extensions",
                "about:blank",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

        target = {}

        def devtools_up() -> bool:
            try:
                with urllib.request.urlopen(
                    f"http://127.0.0.1:{cdp_port}/json/list", timeout=1
                ) as response:
                    pages = json.load(response)
                for page in pages:
                    if page.get("type") == "page" and page.get("webSocketDebuggerUrl"):
                        target.update(page)
                        return True
            except Exception:
                pass
            return False

        wait_for(devtools_up, "the browser's debugging endpoint")
        devtools = Devtools(target["webSocketDebuggerUrl"])
        devtools.call("Page.enable")
        devtools.call("Runtime.enable")

        url = f"http://127.0.0.1:{http_port}/index.html?sample"
        print(f"opening {url}")
        devtools.call("Page.navigate", url=url)

        # Wait for the engine to start and the sample font to reach the editors.
        def loaded() -> bool:
            try:
                return bool(
                    devtools.evaluate(
                        "!!document.querySelector('.axis-editor tbody tr')", timeout=10
                    )
                )
            except RuntimeError:
                return False

        wait_for(loaded, "the sample font to load", timeout=60)

        axis_count = devtools.evaluate(
            "document.querySelectorAll('.axis-editor tbody tr').length"
        )
        print(f"  the Axis Editor shows {axis_count} axes")
        assert axis_count == 5, f"expected Recursive's 5 axes, found {axis_count}"

        # Fill in every axis: a static instance, which is what overlap removal needs.
        wanted = ["0", "1", "800", "0", "0.5"]  # MONO CASL wght slnt CRSV
        for index, value in enumerate(wanted):
            selector = f".axis-editor tbody tr:nth-child({index + 1}) input"
            echoed = devtools.evaluate(
                f"({SET_INPUT})({json.dumps(selector)}, {json.dumps(value)})"
            )
            assert echoed == value, f"axis {index} did not take the value: {echoed!r}"
        print(f"  filled the Axis Editor with {' '.join(wanted)}")

        # Rename the family, so the output can be checked for something the interface
        # was asked to change rather than merely for being a font.
        devtools.evaluate(
            f"({SET_INPUT})('.name-editor tbody tr:nth-child(1) input',"
            f" {json.dumps('Sliced In A Browser')})"
        )

        # And turn on the feature this whole project exists for.
        devtools.evaluate(
            """
            (function () {
              const boxes = [...document.querySelectorAll('.option input[type=checkbox]')];
              if (!boxes.length) { throw new Error('no overlap removal checkbox'); }
              boxes[0].click();
              return boxes[0].checked;
            })()
            """
        )
        print("  ticked 'Remove overlapping contours'")

        # The File menu advertises Ctrl+O. Check it is bound, rather than decoration:
        # the observable effect is a click on the hidden file input, so that is what is
        # watched for. A headless browser cannot show the picker, and does not need to.
        opened = devtools.evaluate(
            """
            (function () {
              const input = document.querySelector('input[type=file]');
              if (!input) { throw new Error('no file input'); }
              let clicked = false;
              const original = input.click;
              input.click = function () { clicked = true; };
              document.dispatchEvent(new KeyboardEvent('keydown', {
                key: 'o', ctrlKey: true, bubbles: true, cancelable: true }));
              input.click = original;
              return clicked;
            })()
            """
        )
        if not opened:
            print("FAIL: Ctrl+O is shown in the File menu but does not open anything",
                  file=sys.stderr)
            return 1
        print("  ok   Ctrl+O opens the file picker")

        devtools.evaluate(CAPTURE_DOWNLOAD)
        devtools.evaluate(
            """
            (function () {
              const button = document.querySelector('button.slice');
              if (!button) { throw new Error('no Slice button'); }
              if (button.disabled) { throw new Error('the Slice button is disabled'); }
              button.click();
            })()
            """
        )
        print("  pressed Slice")

        def produced() -> bool:
            return bool(devtools.evaluate("!!window.__sliceCaptured", timeout=20))

        # If the engine reported a problem, say what it was rather than timing out.
        def failed() -> str | None:
            return devtools.evaluate(
                "(document.querySelector('.modal.error p') || {}).textContent || null"
            )

        deadline = time.time() + 120
        while time.time() < deadline:
            if produced():
                break
            message = failed()
            if message:
                detail = devtools.evaluate(
                    "(document.querySelector('.modal.error pre') || {}).textContent || ''"
                )
                print(f"FAIL: the interface reported an error: {message}\n{detail}",
                      file=sys.stderr)
                return 1
            time.sleep(0.25)
        else:
            print("FAIL: no font was produced within 120s", file=sys.stderr)
            return 1

        encoded = devtools.evaluate(READ_CAPTURED, timeout=120)
        assert encoded, "the captured blob was empty"
        font_bytes = base64.b64decode(encoded)
        print(f"  the page produced {len(font_bytes)} bytes")

        status = devtools.evaluate("document.querySelector('.statusbar .status').textContent")
        print(f"  status bar: {status}")

        output = workdir / "sliced.ttf"
        output.write_bytes(font_bytes)

        # The independent check: the native engine has to agree this is a font, and the
        # font the interface was asked for.
        report = subprocess.run(
            [str(slice_cli), "info", str(output)],
            capture_output=True,
            text=True,
            check=False,
        )
        if report.returncode != 0:
            print(f"FAIL: slice-cli could not read the result:\n{report.stderr}",
                  file=sys.stderr)
            return 1

        print("\n--- slice info on what the browser produced ---")
        print(report.stdout.rstrip())
        print("--- end ---\n")

        # Read the count out of the sample rather than repeating it here. A slice keeps
        # every glyph, so the property to assert is "the same as the input had" -- and a
        # literal would have to be chased every time the sample font changes, which is
        # exactly what happened when it did.
        source_report = subprocess.run(
            [str(slice_cli), "info", str(sample_font())], capture_output=True, text=True
        )
        source_glyphs = re.search(r"(\d+) glyphs", source_report.stdout)
        if not source_glyphs:
            print("could not read the sample's glyph count", file=sys.stderr)
            return 1

        checks = [
            ("the name the interface set is in the font", "Sliced In A Browser"),
            ("every axis was pinned, so no fvar remains", "Not a variable font"),
            ("the glyph count survived", f"{source_glyphs.group(1)} glyphs"),
        ]
        for description, needle in checks:
            if needle not in report.stdout:
                print(f"FAIL: {description} (looked for {needle!r})", file=sys.stderr)
                return 1
            print(f"  ok   {description}")

        # ------------------------------------------------------------------
        # Round two: a partial slice, which takes a different path through the
        # engine and must leave the font variable.
        # ------------------------------------------------------------------
        print("\nslicing again, this time leaving wght variable")

        devtools.evaluate(
            """
            (function () {
              const boxes = [...document.querySelectorAll('.option input[type=checkbox]')];
              // Overlap removal needs every axis pinned, so it has to come back off.
              if (boxes[0].checked) { boxes[0].click(); }
              return boxes[0].checked;
            })()
            """
        )
        # CASL goes back to its default before the axis is narrowed. Round one pinned it
        # at 1, and pinning an axis *away* from its default while restricting another is
        # the one partial case this build refuses: the GDEF item variation store's
        # residual at the new default location would have to be written back into the
        # GPOS values, which is not implemented. `partial.pinned-off-default-with-a-gdef-
        # store-is-refused` covers that refusal; this test is about the partial path
        # working, so it asks for a job that is supported.
        devtools.evaluate(
            f"({SET_INPUT})('.axis-editor tbody tr:nth-child(2) input', {json.dumps('0')})"
        )
        devtools.evaluate(
            f"({SET_INPUT})('.axis-editor tbody tr:nth-child(3) input',"
            f" {json.dumps('300:700')})"
        )
        devtools.evaluate("window.__sliceCaptured = null")
        devtools.evaluate("document.querySelector('button.slice').click()")

        deadline = time.time() + 120
        while time.time() < deadline:
            if produced():
                break
            message = failed()
            if message:
                print(f"FAIL: partial slice reported an error: {message}", file=sys.stderr)
                return 1
            time.sleep(0.25)
        else:
            print("FAIL: the partial slice produced nothing", file=sys.stderr)
            return 1

        partial_bytes = base64.b64decode(devtools.evaluate(READ_CAPTURED, timeout=120))
        partial_path = workdir / "partial.ttf"
        partial_path.write_bytes(partial_bytes)
        print(f"  the page produced {len(partial_bytes)} bytes")

        report = subprocess.run(
            [str(slice_cli), "info", str(partial_path)],
            capture_output=True,
            text=True,
            check=False,
        )
        if report.returncode != 0:
            print(f"FAIL: slice-cli could not read the partial result:\n{report.stderr}",
                  file=sys.stderr)
            return 1

        print("\n--- slice info on the partial slice ---")
        print(report.stdout.rstrip())
        print("--- end ---\n")

        partial_checks = [
            ("it is still a variable font", "Axis Editor"),
            ("only wght survived, with its new extent", "wght   300.0 : 700.0 [300.0]"),
        ]
        for description, needle in partial_checks:
            if needle not in report.stdout:
                print(f"FAIL: {description} (looked for {needle!r})", file=sys.stderr)
                return 1
            print(f"  ok   {description}")
        if "MONO" in report.stdout:
            print("FAIL: a pinned axis is still in the output", file=sys.stderr)
            return 1
        print("  ok   the pinned axes are gone")

        if args.keep:
            for name, data in (("sliced-by-browser.ttf", font_bytes),
                               ("partial-by-browser.ttf", partial_bytes)):
                kept = REPO_ROOT / name
                kept.write_bytes(data)
                print(f"\nkept {kept}")

        print("\nbrowser slice test passed")
        return 0

    finally:
        if devtools is not None:
            devtools.close()
        for process in (chrome, server):
            if process is not None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
        if not args.keep:
            shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
