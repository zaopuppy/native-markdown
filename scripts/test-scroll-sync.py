"""Exercise Split scrolling in the real WebView2, with isolated user data.

Run: uv run --with playwright python scripts/test-scroll-sync.py --binary <exe>
Native editor geometry, selection and wheel ownership are covered by Rust tests.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request

from playwright.sync_api import sync_playwright


parser = argparse.ArgumentParser()
parser.add_argument('--binary', default='target/release/native-markdown.exe')
args = parser.parse_args()
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]

with tempfile.TemporaryDirectory(prefix='native-markdown-scroll-', ignore_cleanup_errors=True) as directory:
    directory = Path(directory)
    fixture = directory / 'scroll-sync.md'
    source = ''.join(f'## Section {i} 中文\n\n' + ('中文 wrapped paragraph. ' * 30) + '\n\n'
                     + '| A | B |\n|---|---|\n| 1 | 2 |\n\n' for i in range(60))
    fixture.write_bytes(source.encode('utf-8'))
    env = dict(os.environ, APPDATA=str(directory / 'appdata'),
               NATIVE_MARKDOWN_WEB_PROFILE=str(directory / 'profile'),
               NATIVE_MARKDOWN_WEB_TEST_ARGS=f'--remote-debugging-port={port}')
    with (directory / 'app.log').open('w', encoding='utf-8') as log:
        process = subprocess.Popen([str(Path(args.binary).resolve()), str(fixture)], env=env,
                                   stdout=log, stderr=log)
        try:
            for _ in range(150):
                try:
                    with urllib.request.urlopen(f'http://127.0.0.1:{port}/json', timeout=.5) as response:
                        if json.load(response):
                            break
                except OSError:
                    pass
                if process.poll() is not None:
                    raise RuntimeError('App exited before WebView started')
                time.sleep(.2)
            with sync_playwright() as playwright:
                browser = playwright.chromium.connect_over_cdp(f'http://127.0.0.1:{port}')
                page = browser.contexts[0].pages[0]
                page.wait_for_selector('#document h2')
                page.keyboard.press('Control+2')
                page.wait_for_function('()=>state.sync?.enabled && !loading')
                page.wait_for_timeout(300)
                def settings_snapshot():
                    return {str(path.relative_to(directory / 'appdata')): (path.stat().st_mtime_ns, path.read_bytes())
                            for path in (directory / 'appdata').rglob('*') if path.is_file()}

                saved_settings = settings_snapshot()
                # Record existing IPC without bypassing the real native receiver.
                page.evaluate('''() => {
                    window.scrollMessages=[];
                    const send=window.chrome.webview.postMessage.bind(window.chrome.webview);
                    window.chrome.webview.postMessage=value=>{const message=JSON.parse(value);
                        if(message.kind==='scroll-position')scrollMessages.push(message);send(value);};
                }''')
                blocks = page.locator('.reading-section > .source-block').evaluate_all(
                    '(nodes)=>nodes.map(e=>({start:Number(e.dataset.sourceStart),end:Number(e.dataset.sourceEnd)}))')
                assert len(blocks) == 180, len(blocks)
                data = source.encode('utf-8')
                for block in blocks:
                    assert data[block['start']:block['end']].decode('utf-8').strip()
                # Native source -> preview: fractional position in a wrapped paragraph.
                target = blocks[76]['start']
                page.evaluate('start=>readerScrollTo({start,fraction:.35,edge:0,revision:state.sync.revision})', target)
                page.wait_for_function('()=>scrollY > 1000')
                page.wait_for_timeout(300)
                error = page.evaluate('''start=>{
                    const nodes=[...root.querySelectorAll('.reading-section > .source-block')];
                    const i=nodes.findIndex(e=>Number(e.dataset.sourceStart)===start);
                    const y=e=>e.getBoundingClientRect().top+scrollY;
                    return Math.abs(scrollY-(y(nodes[i])+(y(nodes[i+1])-y(nodes[i]))*.35));
                }''', target)
                assert error < 2, error
                assert not page.evaluate('scrollMessages.length'), 'Following source must not echo'
                # Relayout while source owns the scroll re-applies its block anchor.
                page.evaluate("root.style.fontSize='22px'")
                page.wait_for_timeout(400)
                assert not page.evaluate('scrollMessages.length'), 'Relayout must not switch ownership'
                # A real wheel in the reading surface transfers ownership to preview.
                page.mouse.move(150, 200)
                page.mouse.wheel(0, 500)
                page.wait_for_timeout(500)
                page.wait_for_function('()=>scrollMessages.length > 0')
                page.wait_for_timeout(400)
                count = page.evaluate('scrollMessages.length')
                page.wait_for_timeout(400)
                assert page.evaluate('scrollMessages.length') == count, 'Idle scroll feedback loop'
                message = page.evaluate('scrollMessages.at(-1)')
                assert message['start'] in [block['start'] for block in blocks]
                assert 0 <= message['fraction'] <= 1
                # Stale source commands cannot move the preview.
                before = page.evaluate('scrollY')
                page.evaluate('readerScrollTo({start:0,fraction:0,edge:-1,revision:state.sync.revision-1})')
                page.wait_for_timeout(100)
                assert page.evaluate('scrollY') == before
                # Top and bottom explicitly synchronize across unequal document heights.
                for edge in [-1, 1]:
                    page.evaluate('edge=>readerScrollTo({start:0,fraction:0,edge,revision:state.sync.revision})', edge)
                    page.wait_for_timeout(250)
                    assert page.evaluate('edge=>Math.abs(scrollY-(edge<0?0:document.documentElement.scrollHeight-innerHeight))', edge) < 2
                assert settings_snapshot() == saved_settings, 'Scrolling must not write app settings or caches'
                # Source-only mode disables transport; the same is true of the Split toggle.
                page.keyboard.press('Control+3')
                page.wait_for_function('()=>!state.sync.enabled')
                before = page.evaluate('scrollMessages.length')
                page.evaluate('scrollSync.previewIntent();scrollTo(0,1000)')
                page.wait_for_timeout(200)
                assert page.evaluate('scrollMessages.length') == before
                browser.close()
            assert fixture.read_bytes() == data, 'Scrolling must not modify the document'
            print('PASS: real WebView2 block mapping, fractional scroll, ownership, relayout, endpoints, stale messages and disabled transport')
        finally:
            process.terminate()
            process.wait(timeout=10)
