"""Real WebView2 regression for preserving unchanged preview DOM and render state.

Run: uv run --with playwright python scripts/test-incremental-preview.py --binary <exe>
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
parser.add_argument('--script', help='Override reader.js for development; omit for packaged verification')
args = parser.parse_args()
with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0))
    port = listener.getsockname()[1]

with tempfile.TemporaryDirectory(prefix='markdown-incremental-', ignore_cleanup_errors=True) as directory:
    directory = Path(directory)
    fixture = directory / 'incremental.md'
    fixture.write_text('# Incremental\n\nEditable paragraph.\n\n```mermaid\nflowchart LR\nA[Alpha]-->B[Beta]\n```\n\n'
                       '$x^2$\n\n```rust\nfn main() {}\n```\n\n'
                       '![Local](image.svg)\n\n[Next](next.md)\n\nRepeated.\n\nRepeated.\n\n'
                       + ''.join(f'## Section {i}\n\nParagraph {i}.\n\n' for i in range(40)), encoding='utf-8')
    (directory / 'image.svg').write_text('<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="teal"/></svg>', encoding='utf-8')
    (directory / 'next.md').write_text('# Next', encoding='utf-8')
    env = dict(os.environ, APPDATA=str(directory / 'appdata'), TEMP=str(directory), TMP=str(directory),
               NATIVE_MARKDOWN_WEB_PROFILE=str(directory / 'profile'),
               NATIVE_MARKDOWN_WEB_TEST_ARGS=f'--remote-debugging-port={port}')
    with (directory / 'app.log').open('wb') as log:
        process = subprocess.Popen([str(Path(args.binary).resolve()), str(fixture)], env=env, stdout=log, stderr=log)
        try:
            for _ in range(150):
                try:
                    with urllib.request.urlopen(f'http://127.0.0.1:{port}/json', timeout=.5) as response:
                        if json.load(response):
                            break
                except OSError:
                    pass
                assert process.poll() is None, 'App exited during startup'
                time.sleep(.2)
            with sync_playwright() as playwright:
                browser = playwright.chromium.connect_over_cdp(f'http://127.0.0.1:{port}')
                page = browser.contexts[0].pages[0]
                errors = []
                page.on('pageerror', lambda error: errors.append(str(error)))
                if args.script:
                    page.route('**/reader.js', lambda route: route.fulfill(path=str(Path(args.script).resolve()), content_type='text/javascript'))
                    page.reload()
                page.wait_for_selector('.diagram svg')
                page.wait_for_selector('.katex')
                page.wait_for_selector('code.hljs')
                page.evaluate('''async () => {
                    window.rawHTML=await (await fetch('/document')).text();
                    window.kept={diagram:root.querySelector('.diagram'),svg:root.querySelector('.diagram svg'),
                        math:root.querySelector('.katex'),code:root.querySelector('code.hljs'),image:root.querySelector('img'),
                        repeated:[...root.querySelectorAll('p')].filter(p=>p.textContent==='Repeated.')};
                }''')
                page.locator('.diagram').get_by_role('button', name='+', exact=True).click()
                width = page.locator('.diagram svg').evaluate('(e)=>e.style.width')
                # A paragraph edit must retain all unrelated render results and duplicated blocks.
                page.evaluate("readerSetDocument(rawHTML.replace('Editable paragraph.', 'Changed paragraph.'), false)")
                assert page.evaluate('kept.svg===root.querySelector(".diagram svg")'), 'Unchanged Mermaid SVG was rebuilt'
                assert page.locator('.diagram svg').evaluate('(e)=>e.style.width') == width
                assert page.evaluate('kept.math===root.querySelector(".katex")'), 'Unchanged math was rebuilt'
                assert page.evaluate('kept.code===root.querySelector("code.hljs")'), 'Unchanged highlighting was rebuilt'
                assert page.evaluate('kept.image===root.querySelector("img")'), 'Unchanged image was rebuilt'
                assert page.evaluate('kept.repeated.every((p,i)=>p===[...root.querySelectorAll("p")].filter(p=>p.textContent==="Repeated.")[i])')
                # Source positions can shift without changing the identity of the content.
                page.evaluate('''() => {
                    const template=document.createElement('template');template.innerHTML=rawHTML;
                    window.oldImageStart=Number(kept.image.closest('.source-block').dataset.sourceStart);
                    template.content.querySelectorAll('.source-block').forEach(block=>{
                        block.dataset.sourceStart=Number(block.dataset.sourceStart)+23;
                        block.dataset.sourceEnd=Number(block.dataset.sourceEnd)+23;
                    });
                    readerSetDocument(template.innerHTML,false);
                }''')
                assert page.evaluate('kept.image===root.querySelector("img") && Number(kept.image.closest(".source-block").dataset.sourceStart)===oldImageStart+23')
                # An unrelated update must keep an expanded diagram open with the same drawing.
                page.locator('.diagram').get_by_role('button', name='Expand', exact=True).click()
                page.evaluate("readerSetDocument(rawHTML.replace('Editable paragraph.', 'Another edit.'), false)")
                assert page.locator('dialog[open]').count() == 1, 'Unrelated edit closed the viewer'
                assert page.evaluate('kept.svg===document.querySelector("dialog svg")')
                page.keyboard.press('Escape')
                # Hold Mermaid's asynchronous work: the old drawing must stay until commit.
                page.evaluate('''() => {
                    window.realRender=mermaid.render.bind(mermaid);
                    mermaid.render=async(...args)=>{await new Promise(resolve=>window.finishRender=resolve);return realRender(...args);};
                    window.readerSetDocument(rawHTML.replace('A[Alpha]', 'A[Updated]'), false);
                }''')
                page.wait_for_function('()=>typeof finishRender==="function"')
                assert page.evaluate('kept.svg===root.querySelector(".diagram svg")'), 'Old diagram vanished before replacement was ready'
                missing_frames = page.evaluate('''async () => {
                    let missing=0;
                    for(let i=0;i<12;i++) {await new Promise(requestAnimationFrame);if(root.querySelector('.diagram svg')!==kept.svg)missing++;}
                    return missing;
                }''')
                assert missing_frames == 0, f'Old diagram disappeared for {missing_frames} sampled frames'
                # A later edit supersedes the in-flight render without publishing the old result.
                page.evaluate("readerSetDocument(rawHTML.replace('A[Alpha]', 'A[Newest]'), false)")
                page.evaluate('()=>{finishRender();mermaid.render=realRender;}')
                page.wait_for_function('()=>root.querySelector(".diagram svg")?.textContent.includes("Newest")')
                # Insert a section above the viewport: preserve node identity, text selection and anchor.
                page.wait_for_timeout(250)
                page.locator('h2').nth(15).scroll_into_view_if_needed()
                page.wait_for_timeout(100)
                page.evaluate('''() => {
                    window.anchor=[...root.querySelectorAll('h2')][15];window.anchorY=anchor.getBoundingClientRect().top;
                    const range=document.createRange();range.selectNodeContents(anchor);getSelection().removeAllRanges();getSelection().addRange(range);
                }''')
                page.evaluate('''() => readerSetDocument('<section class="reading-section"><div class="source-block" data-source-start="0" data-source-end="1"><h2>Inserted above</h2><p>Extra height</p></div></section>'+rawHTML, false)''')
                page.wait_for_timeout(200)
                assert page.evaluate('anchor.isConnected'), 'Unchanged scrolled block was replaced'
                delta = page.evaluate('anchor.getBoundingClientRect().top-anchorY')
                assert abs(delta) < 3, f'Viewport anchor jumped by {delta}px'
                assert page.evaluate('getSelection().toString()') == 'Section 15', 'Selection in unchanged text was lost'
                # Unexpected raw HTML structure falls back safely, still through the sanitizer.
                page.evaluate("readerSetDocument('<p>Fallback<script>window.pwned=true</script></p>',false)")
                assert page.locator('#document').inner_text() == 'Fallback'
                assert page.evaluate('window.pwned') is None
                # New document/reset is deliberately a full replacement and resets scroll.
                page.evaluate("readerSetDocument('<section class=\"reading-section\"><div class=\"source-block\" data-source-start=\"0\" data-source-end=\"3\"><p>New document</p></div></section>',true)")
                assert not page.evaluate('kept.image.isConnected')
                assert page.evaluate('scrollY') == 0
                assert not errors, errors
                browser.close()
            print('PASS: unchanged DOM, duplicates, diagram zoom/viewer, atomic diagram replacement, scroll anchor, selection and document reset')
        finally:
            process.terminate()
            process.wait(timeout=10)
