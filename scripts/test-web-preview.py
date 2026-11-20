"""Real WebView2 DOM regression test via an opt-in loopback debugging port.

Run: uv run --with playwright python scripts/test-web-preview.py --binary <exe>
No browser installation, Node, or app production debugging port is required.
"""
import argparse
import json
import os
import shutil
import socket
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.request
from playwright.sync_api import sync_playwright

parser = argparse.ArgumentParser()
parser.add_argument('--binary', required=True)
parser.add_argument('--poc', action='store_true')
args = parser.parse_args()
repo = Path(__file__).resolve().parents[1]
# A fresh port avoids connecting to a previous test's browser while it is still exiting.
with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0))
    port = listener.getsockname()[1]
with tempfile.TemporaryDirectory(prefix='markdown-web-test-') as directory:
    fixture = Path(directory) / 'acceptance.md'
    fixture.write_text('''# Browser reading acceptance

正文可以选择复制，English selectable text.

> A quiet quotation.
>
> > A nested quotation.

- [x] Complete task
- [ ] Pending task

| Name | Value |
| --- | --- |
| Alpha | 42 |

```rust
fn main() { println!("hello"); }
```

$E=mc^2$

![Local diagram](local.svg)

[External](https://example.com) [Unsafe](javascript:alert(1))

<script>window.pwned = true; window.ipc.postMessage('{"kind":"save","path":"secret"}')</script>

<img src="file:///C:/Windows/win.ini" onerror="window.pwned=true">

```mermaid
flowchart LR
  A[开始] --> B{检查}
  B --> C[完成]
```

```mermaid
this is not valid Mermaid
```

Text after broken Mermaid.
''', encoding='utf-8')
    (Path(directory)/'local.svg').write_text('<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60"><rect width="120" height="60" fill="teal"/></svg>', encoding='utf-8')
    original = fixture.read_bytes()
    env = dict(os.environ, NATIVE_MARKDOWN_WEB_PROFILE=str(Path(directory)/'profile'), NATIVE_MARKDOWN_WEB_TEST_ARGS=f'--remote-debugging-port={port}')
    log_path = repo/f'tmp/web-preview-test-{time.time_ns()}.log'
    log = open(log_path, 'w', encoding='utf-8')
    process = subprocess.Popen([str(Path(args.binary).resolve()), str(fixture)], env=env, stdout=log, stderr=log)
    try:
        for _ in range(100):
            try:
                with urllib.request.urlopen(f'http://127.0.0.1:{port}/json/version', timeout=.5) as response:
                    json.load(response)
                break
            except Exception:
                if process.poll() is not None: raise RuntimeError('App exited before WebView started')
                time.sleep(.2)
        with sync_playwright() as p:
            browser = p.chromium.connect_over_cdp(f'http://127.0.0.1:{port}')
            page = browser.contexts[0].pages[0]
            page.wait_for_selector('#document h1')
            page.wait_for_selector('.katex')
            assert page.locator('table td').count() == 2
            assert page.locator('.task-marker').all_text_contents() == ['☑','☐']
            assert page.locator('pre code .hljs-keyword').count() > 0
            page.wait_for_function('() => Array.from(document.images).some(i=>i.getAttribute("src")?.startsWith("/image/") && i.complete && i.naturalWidth>0)')
            assert page.locator('img[src^="file:"]').count() == 0
            assert page.evaluate('window.pwned') is None
            assert page.locator('[href^="javascript:"],script:not([src]),iframe,object').count() == 0
            assert page.evaluate('fetch("/../../Cargo.toml").then(r=>r.status)') == 404
            assert page.evaluate('fetch("/image/0/0").then(r=>r.status)') == 404
            # Bring both diagrams into the lazy render range.
            page.locator('.diagram').first.scroll_into_view_if_needed()
            page.wait_for_selector('.diagram svg text', state='attached')
            assert '开始' in page.locator('.diagram').first.inner_text(), 'Node label must be real SVG text, not a removed foreignObject'
            assert page.locator('.diagram svg foreignObject').count() == 0
            assert page.locator('.diagram img').count() == 0
            page.locator('.diagram').nth(1).scroll_into_view_if_needed()
            page.wait_for_selector('.diagram-error')
            assert 'Text after broken Mermaid.' in page.locator('#document').inner_text()
            page.evaluate('readerFind("English selectable text",0)')
            assert page.evaluate('getSelection().toString()') == 'English selectable text'
            page.locator('img[src^="/image/"]').click()
            page.wait_for_selector('dialog[open]')
            page.get_by_role('button',name='Close',exact=True).click()
            assert page.locator('dialog[open]').count() == 0
            first = page.locator('.diagram').first
            first.scroll_into_view_if_needed()
            stage = first.locator('.diagram-stage')
            width = first.locator('svg').evaluate('(e)=>e.getBoundingClientRect().width')
            for _ in range(5): first.get_by_role('button', name='+', exact=True).click()
            assert first.locator('svg').evaluate('(e)=>e.getBoundingClientRect().width') > width*2
            first.get_by_role('button', name='Pan', exact=True).click()
            box = stage.bounding_box()
            page.mouse.move(box['x']+box['width']*.7, box['y']+80)
            page.mouse.down(); page.mouse.move(box['x']+30, box['y']+80, steps=8); page.mouse.up()
            assert stage.evaluate('(e)=>e.scrollLeft') > 0, 'Drag must pan the actual SVG viewport'
            first.get_by_role('button', name='Pan', exact=True).click()
            first.get_by_role('button', name='Fit', exact=True).click()
            first.locator('svg text').filter(has_text='开始').first.dblclick()
            assert page.evaluate('getSelection().toString()').strip() == '开始', 'SVG labels must support direct pointer selection'
            selected = first.locator('svg text').filter(has_text='开始').first.evaluate('''(e)=>{const r=document.createRange();r.selectNodeContents(e);const s=getSelection();s.removeAllRanges();s.addRange(r);return s.toString();}''')
            assert selected.strip()
            # Test actual Ctrl+C on a DOM text selection; inspect OS clipboard in PowerShell afterwards.
            page.keyboard.press('Control+c')
            page.screenshot(path=str(repo/'tmp/web-preview-acceptance.png'))
            # Expand the actual SVG without losing the inline zoom, pan or scroll state.
            for _ in range(3): first.get_by_role('button',name='+',exact=True).click()
            first.get_by_role('button',name='Pan',exact=True).click()
            stage.evaluate('(e)=>{e.scrollLeft=80;e.scrollTop=10;}')
            inline_state = first.evaluate("""e=>{const s=e.querySelector('.diagram-stage'),svg=s.querySelector('svg');
                window.inlineDiagram=svg;
                return {style:svg.getAttribute('style'),left:s.scrollLeft,top:s.scrollTop,
                    pan:s.classList.contains('pan'),scroll:scrollY};}""")
            for cycle in range(3):
                first.get_by_role('button',name='Expand',exact=True).click()
                viewer=page.get_by_role('dialog',name='Mermaid diagram viewer')
                viewer.wait_for(state='visible')
                assert viewer.locator('svg').evaluate('(e)=>e===window.inlineDiagram')
                assert first.locator('svg').count() == 0, 'Keep a single SVG with unique IDs'
                assert viewer.get_by_role('button',name='Move',exact=True).get_attribute('aria-pressed') == 'true'
                view_stage=viewer.locator('.diagram-stage')
                assert view_stage.evaluate('(e)=>e.clientHeight') > stage.evaluate('(e)=>e.clientHeight')
                for _ in range(6): viewer.get_by_role('button',name='+',exact=True).click()
                box=view_stage.bounding_box()
                page.mouse.move(box['x']+box['width']*.7,box['y']+80)
                page.mouse.down();page.mouse.move(box['x']+30,box['y']+80,steps=8);page.mouse.up()
                assert view_stage.evaluate('(e)=>e.scrollLeft') > 0
                viewer.get_by_role('button',name='Fit',exact=True).click()
                viewer.get_by_role('button',name='Select text',exact=True).click()
                viewer.locator('svg text').filter(has_text='开始').first.dblclick()
                assert page.evaluate('getSelection().toString()').strip() == '开始'
                assert viewer.get_by_role('button',name='Select text',exact=True).get_attribute('aria-pressed') == 'true'
                viewer.get_by_role('button',name='100%',exact=True).click()
                assert viewer.locator('.viewer-zoom').inner_text() == '100%'
                before=viewer.locator('svg').evaluate('(e)=>e.getBoundingClientRect().width')
                page.keyboard.press('Control++')
                assert viewer.locator('svg').evaluate('(e)=>e.getBoundingClientRect().width') > before
                page.keyboard.press('Control+0')
                assert viewer.locator('.viewer-zoom').inner_text() == '100%'
                page.keyboard.down('Control')
                page.mouse.move(box['x']+50,box['y']+50);page.mouse.wheel(0,-120)
                page.keyboard.up('Control')
                page.wait_for_function("document.querySelector('.viewer-zoom').textContent !== '100%'")
                if cycle == 0: page.screenshot(path=str(repo/'tmp/web-preview-mermaid-expanded.png'))
                if cycle % 2: viewer.get_by_role('button',name='Close',exact=True).click()
                else: page.keyboard.press('Escape')
                assert page.locator('dialog[open]').count() == 0
                restored=first.evaluate("""e=>{const s=e.querySelector('.diagram-stage'),svg=s.querySelector('svg');
                    return {style:svg.getAttribute('style'),left:s.scrollLeft,top:s.scrollTop,
                        pan:s.classList.contains('pan'),scroll:scrollY};}""")
                assert restored == inline_state, (restored,inline_state)
            first.get_by_role('button',name='Pan',exact=True).click()
            first.get_by_role('button',name='Fit',exact=True).click()
            # Search lifecycle: next/previous occurrence and clearing both CSS marks and selection.
            page.evaluate("""() => {
                const base={zoom:1,heading:null,navigation:0,query:'Text',occurrence:0,
                    searchRequest:100,searchOpen:true,nativeSearchFocus:false};
                window.readerConfigure(base);
                if (!CSS.highlights.has('search') || !getSelection().toString())
                    throw Error('Search must highlight and select a match');
                window.readerConfigure({...base,query:'',searchOpen:false,searchRequest:101});
                if (CSS.highlights.has('search') || getSelection().rangeCount)
                    throw Error('Closing search must clear highlights and selection');
                window.readerConfigure({...base,searchRequest:102});
                if (!CSS.highlights.has('search') || !getSelection().toString())
                    throw Error('Reopening search must restore the match');
                window.readerConfigure({...base,query:'',searchOpen:false,searchRequest:103});
            }""")
            assert page.locator('blockquote').first.evaluate('(e)=>getComputedStyle(e).borderLeftWidth') == '1px'
            # An explicit app theme wins over the operating system preference.
            page.emulate_media(color_scheme='dark')
            page.wait_for_function("document.documentElement.dataset.theme === 'paper'")
            assert page.evaluate('getComputedStyle(document.documentElement).backgroundColor') == 'rgb(250, 251, 248)'
            for theme_id, canvas, text, dark in [
                ('sepia','#f8f1e3','#443b30',False),
                ('sage','#f0f5ef','#2c3b30',False),
                ('midnight','#202329','#e2e5eb',True),
                ('nord','#2e3440','#e5e9f0',True),
                ('paper','#fafbf8','#292e2b',False),
            ]:
                first.get_by_role('button',name='Expand',exact=True).click()
                assert page.locator('dialog[open]').count() == 1
                page.evaluate("""({id,canvas,text,dark})=>readerConfigure({zoom:1,heading:null,navigation:0,
                    query:'',occurrence:0,searchRequest:104,theme:{id,dark,colors:{canvas,text}}})""",
                    dict(id=theme_id,canvas=canvas,text=text,dark=dark))
                assert page.locator('dialog[open]').count() == 0, 'Theme changes must release the active viewer'
                assert page.evaluate('document.documentElement.dataset.theme') == theme_id
                first.scroll_into_view_if_needed()
                page.wait_for_selector('.diagram svg text')
                assert first.locator('svg text').count() > 0
                assert page.locator('pre code.hljs').first.evaluate('(e)=>getComputedStyle(e).backgroundColor') == 'rgba(0, 0, 0, 0)'
            page.screenshot(path=str(repo/'tmp/web-preview-themes.png'))
            first.get_by_role('button',name='Expand',exact=True).click()
            page.evaluate('readerReload(false)')
            page.wait_for_selector('dialog[open]',state='detached')
            first.scroll_into_view_if_needed()
            page.wait_for_selector('.diagram svg text')
            first.get_by_role('button',name='Expand',exact=True).click()
            page.keyboard.press('Escape')
            assert first.locator('svg').count() == 1

            if not args.poc:
                # Drive the real shortcut bridge: saving preserves the original Markdown byte-for-byte.
                before=fixture.stat().st_mtime_ns
                page.keyboard.press('Control+s')
                for _ in range(30):
                    if fixture.stat().st_mtime_ns != before: break
                    time.sleep(.1)
                assert fixture.stat().st_mtime_ns != before, 'Ctrl+S must reach the Rust atomic-save path'
                assert fixture.read_bytes() == original
                width=page.evaluate('innerWidth')
                page.keyboard.press('Control+2')
                page.wait_for_function('(width) => innerWidth < width * .8', arg=width)
                page.keyboard.press('Control+1')
                page.wait_for_function('(width) => innerWidth > width * .9', arg=width)
            print(json.dumps({'result':'pass', 'svgSelection':selected, 'expandedViewer':True, 'inlineStatePreserved':True, 'inlineSVG':True, 'zoom':True, 'dragPan':True, 'localImage':True, 'math':True, 'codeHighlight':True, 'failureIsolation':True, 'sanitization':True, 'darkTheme':True}, ensure_ascii=False))
            browser.close()
    except Exception:
        log.flush()
        print('Application log:',log_path)
        print('\n'.join(log_path.read_text(encoding='utf-8').splitlines()[:25]))
        raise
    finally:
        subprocess.run(['taskkill','/PID',str(process.pid),'/T','/F'], capture_output=True)
        process.wait(timeout=15)
        log.close()
        (repo/'tmp/web-preview-test.log').write_bytes(log_path.read_bytes())
        # WebView2 can release profile file handles just after its process tree exits.
        # Wait for that release instead of reporting a successful UI test as a cleanup failure.
        for attempt in range(50):
            try:
                shutil.rmtree(directory)
                break
            except PermissionError:
                if attempt == 49:
                    raise
                time.sleep(.1)
