"""Check the actual WebView2 page before CSS/reader JS can apply a theme.

Run: uv run --with playwright python scripts/test-web-preview-first-paint.py --binary <exe>
"""
import argparse
import json
import os
from pathlib import Path
import socket
import shutil
import subprocess
import tempfile
import time
import urllib.request

from playwright.sync_api import sync_playwright

parser = argparse.ArgumentParser()
parser.add_argument('--binary', required=True)
parser.add_argument('--delayed', action='store_true', help='Start empty, then open the document through the benchmark seam')
args = parser.parse_args()
repo = Path(__file__).resolve().parents[1]
(repo/'tmp').mkdir(exist_ok=True)

for theme, expected in [('midnight', 'rgb(32, 35, 41)'), ('nord', 'rgb(46, 52, 64)')]:
    with tempfile.TemporaryDirectory(prefix='markdown-first-paint-') as directory:
        directory = Path(directory)
        settings = directory/'appdata/NativeMarkdown'
        settings.mkdir(parents=True)
        (settings/'layout.conf').write_text(f'theme={theme}\n', encoding='utf-8')
        fixture = directory/'first-paint.md'
        fixture.write_text('# First paint\n\nDark from the first frame.\n', encoding='utf-8')
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        env = dict(os.environ, APPDATA=str(directory/'appdata'),
                   NATIVE_MARKDOWN_WEB_PROFILE=str(directory/'profile'),
                   NATIVE_MARKDOWN_WEB_TEST_ARGS=f'--remote-debugging-port={port}')
        if args.delayed:
            env.update(NATIVE_MARKDOWN_BENCHMARK='image-release',
                       NATIVE_MARKDOWN_BENCHMARK_SECONDARY_DOCUMENT=str(fixture),
                       NATIVE_MARKDOWN_BENCHMARK_WARMUP_MS='500',
                       NATIVE_MARKDOWN_BENCHMARK_SWITCH_STEP='1',
                       NATIVE_MARKDOWN_BENCHMARK_STEP_MS='50',
                       NATIVE_MARKDOWN_BENCHMARK_SECONDS='20')
        mode='delayed' if args.delayed else 'startup'
        log = repo/f'tmp/first-paint-{theme}-{mode}.log'
        with log.open('w', encoding='utf-8') as output:
            process = subprocess.Popen([str(Path(args.binary).resolve())]+([] if args.delayed else [str(fixture)]),
                                       env=env, stdout=output, stderr=output)
            try:
                for _ in range(100):
                    try:
                        with urllib.request.urlopen(f'http://127.0.0.1:{port}/json/version', timeout=.5) as response:
                            json.load(response)
                        break
                    except Exception:
                        if process.poll() is not None:
                            raise RuntimeError(f'App exited before WebView started; see {log}')
                        time.sleep(.1)
                with sync_playwright() as p:
                    browser = p.chromium.connect_over_cdp(f'http://127.0.0.1:{port}')
                    page = browser.contexts[0].pages[0]
                    page.wait_for_function(f'() => document.documentElement.dataset.theme === "{theme}" && typeof readerConfigure === "function"')
                    assert page.evaluate('getComputedStyle(document.documentElement).backgroundColor') == expected
                    session = page.context.new_cdp_session(page)
                    pending = []
                    session.on('Fetch.requestPaused', lambda event: pending.append(event))
                    session.send('Fetch.enable', {'patterns': [
                        {'urlPattern': '*/reader.js', 'requestStage': 'Request'},
                        {'urlPattern': '*/reader.css', 'requestStage': 'Request'},
                    ]})
                    # Reload under the existing theme while deliberately stalling both assets.
                    # No readerConfigure callback or external stylesheet can hide the first frame.
                    session.send('Page.reload', {'ignoreCache': True})
                    page.wait_for_selector('#document h1')
                    for _ in range(50):
                        if len(pending) >= 2:
                            break
                        page.wait_for_timeout(20)
                    assert len(pending) >= 2, 'CSS and reader JS must both be paused'
                    color = page.evaluate('getComputedStyle(document.documentElement).backgroundColor')
                    assert color == expected, f'{theme}: early page background was {color}, expected {expected}'
                    css=next(event for event in pending if event['request']['url'].endswith('/reader.css'))
                    session.send('Fetch.continueRequest', {'requestId':css['requestId']})
                    page.wait_for_function('() => getComputedStyle(document.documentElement).fontFamily.includes("Segoe")')
                    assert page.evaluate('getComputedStyle(document.documentElement).backgroundColor') == expected, 'CSS must preserve the initial theme before reader JS'
                    session.send('Fetch.disable')
                    page.wait_for_function('() => typeof readerConfigure === "function"')
                    assert page.evaluate('getComputedStyle(document.documentElement).backgroundColor') == expected
                    page.screenshot(path=str(repo/f'tmp/first-paint-{theme}-{mode}.png'))
                    browser.close()
                print(f'PASS {theme} {mode}: themed background before CSS and reader JS, then normal startup')
            finally:
                subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'], capture_output=True)
                process.wait(timeout=15)
        # WebView2 may release its profile handles shortly after the process exits.
        for _ in range(50):
            try:
                shutil.rmtree(directory)
                break
            except PermissionError:
                time.sleep(.1)
