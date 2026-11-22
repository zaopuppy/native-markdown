"""Real WebView2 outline-follow checks and same-build enabled/disabled scroll comparison.
Run: uv run --with playwright --with psutil python scripts/test-outline-follow.py --binary <exe>
All fixtures, profiles and app settings are isolated. No production telemetry is added.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import statistics
import subprocess
import tempfile
import time
import urllib.request

import psutil
from playwright.sync_api import sync_playwright

parser = argparse.ArgumentParser()
parser.add_argument('--binary', default='target/release/native-markdown.exe')
parser.add_argument('--cpu-rate', type=float, default=1)
args = parser.parse_args()


def cpu_seconds(pid):
    try:
        root = psutil.Process(pid)
        processes = [root] + root.children(recursive=True)
    except psutil.Error:
        return 0
    total = 0
    for process in processes:
        try:
            value = process.cpu_times()
            total += value.user + value.system
        except psutil.Error:
            pass
    return total


def run(count):
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix='markdown-outline-', ignore_cleanup_errors=True) as folder:
        directory = Path(folder)
        fixture = directory / 'outline.md'
        intro = ('Introduction before any heading.\n\n' * 32)
        body = ''.join(f'## Section {i} 中文\n\n' + ('Reading paragraph. ' * 22) + '\n\n'
                       + ('> ### Nested heading\n> Nested content.\n\n' if i == 2 else '')
                       for i in range(count))
        fixture.write_text(intro + body, encoding='utf-8')
        env = dict(os.environ, APPDATA=str(directory/'appdata'),
                   NATIVE_MARKDOWN_WEB_PROFILE=str(directory/'profile'),
                   NATIVE_MARKDOWN_WEB_TEST_ARGS=f'--remote-debugging-port={port}')
        with (directory/'app.log').open('w', encoding='utf-8') as log:
            process = subprocess.Popen([str(Path(args.binary).resolve()), str(fixture)],
                                       env=env, stdout=log, stderr=log)
            try:
                for _ in range(150):
                    try:
                        with urllib.request.urlopen(f'http://127.0.0.1:{port}/json', timeout=.5) as response:
                            if json.load(response):
                                break
                    except OSError:
                        pass
                    if process.poll() is not None:
                        raise RuntimeError('App exited before WebView startup')
                    time.sleep(.2)
                with sync_playwright() as playwright:
                    browser = playwright.chromium.connect_over_cdp(f'http://127.0.0.1:{port}')
                    page = browser.contexts[0].pages[0]
                    page.wait_for_function('()=>typeof state!=="undefined" && state.outline?.enabled && !loading')
                    session = page.context.new_cdp_session(page)
                    session.send('Emulation.setCPUThrottlingRate', {'rate': args.cpu_rate})
                    page.evaluate("""() => {
                        window.outlineEvents=[]; window.outlineReads=0; window.outlineReadMs=0;
                        const send=chrome.webview.postMessage.bind(chrome.webview);
                        chrome.webview.postMessage=value=>{
                            const message=JSON.parse(value);
                            if(message.kind==='outline-position') outlineEvents.push({...message,time:performance.now()});
                            send(value);
                        };
                        const rect=Element.prototype.getBoundingClientRect;
                        Element.prototype.getBoundingClientRect=function(){
                            if(!this.matches('.reading-section,h1,h2,h3,h4,h5,h6')) return rect.call(this);
                            const start=performance.now(), result=rect.call(this);
                            outlineReads++; outlineReadMs+=performance.now()-start;return result;
                        };
                        window.setOutlineEnabled=enabled=>readerConfigure({...state,outline:{...state.outline,enabled}});
                    }""")
                    page.wait_for_timeout(350)
                    # Introduction, nested headings and both directions must report correct indexes.
                    for text in ['Section 3 中文', 'Section 1 中文', 'Nested heading']:
                        target = page.locator('h2,h3').filter(has_text=text).first
                        expected = int(target.get_attribute('data-heading-index'))
                        navigation = page.evaluate('state.navigation')
                        target.evaluate('(e)=>scrollTo(0,e.getBoundingClientRect().top+scrollY-40)')
                        page.wait_for_function('index=>outlineEvents.at(-1)?.heading===index', arg=expected)
                        page.wait_for_timeout(150)
                        assert page.evaluate('state.navigation') == navigation, 'Following outline must not request navigation'
                    page.evaluate('scrollTo(0,0)')
                    page.wait_for_function('()=>outlineEvents.at(-1)?.heading===null')
                    # Static documents do not keep sampling in the background.
                    page.wait_for_timeout(350)
                    idle = page.evaluate('({reads:outlineReads,messages:outlineEvents.length})')
                    page.wait_for_timeout(350)
                    assert page.evaluate('({reads:outlineReads,messages:outlineEvents.length})') == idle, 'Idle polling'
                    # Disabled tracking does no heading geometry work.
                    page.evaluate('setOutlineEnabled(false);outlineEvents=[];outlineReads=0;scrollBy(0,300)')
                    page.wait_for_timeout(300)
                    assert page.evaluate('outlineEvents.length===0 && outlineReads===0'), 'Hidden/disabled outline still sampled'
                    records = []
                    for trial in range(3):
                        for enabled in ([False, True] if trial % 2 == 0 else [True, False]):
                            page.evaluate('enabled=>{setOutlineEnabled(enabled);scrollTo(0,0)}', enabled)
                            page.wait_for_timeout(300)
                            before = cpu_seconds(process.pid)
                            result = page.evaluate("""async () => {
                                outlineEvents=[];outlineReads=0;outlineReadMs=0;
                                const frames=[],began=performance.now();let previous=began;
                                await new Promise(resolve=>{
                                    const tick=now=>{
                                        frames.push(now-previous);previous=now;scrollBy(0,45);
                                        if(now-began<1400)requestAnimationFrame(tick);else resolve();
                                    }; requestAnimationFrame(tick);
                                });
                                await new Promise(resolve=>setTimeout(resolve,160));
                                frames.sort((a,b)=>a-b);
                                return {wallMs:performance.now()-began,frames:frames.length,
                                    p95FrameMs:frames[Math.floor(frames.length*.95)],
                                    reads:outlineReads,readMs:outlineReadMs,messages:outlineEvents.length,
                                    gaps:outlineEvents.slice(1).map((e,i)=>e.time-outlineEvents[i].time)};
                            }""")
                            result.update(enabled=enabled, trial=trial,
                                          treeCpuMs=(cpu_seconds(process.pid)-before)*1000)
                            if enabled:
                                assert result['messages'] <= result['wallMs']/100+2, result
                                assert all(gap >= 85 for gap in result['gaps']), result
                            else:
                                assert result['reads'] == 0 and result['messages'] == 0, result
                            records.append(result)
                    browser.close()
                    summary = {}
                    for enabled in [False, True]:
                        group = [record for record in records if record['enabled'] == enabled]
                        summary['enabled' if enabled else 'disabled'] = {
                            name: round(statistics.median(record[name] for record in group), 3)
                            for name in ['treeCpuMs', 'p95FrameMs', 'frames', 'messages', 'reads', 'readMs']}
                    return {'headings': count+1, 'cpuThrottle': args.cpu_rate,
                            'summary': summary, 'runs': records}
            except Exception:
                log.flush()
                print((directory/'app.log').read_text(encoding='utf-8', errors='replace')[-3000:])
                raise
            finally:
                subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'], capture_output=True)
                process.wait(timeout=15)


result = {'result': 'pass', 'binary': str(Path(args.binary).resolve()), 'cases': [run(60), run(1600)]}
output = Path(__file__).resolve().parents[1]/'tmp'/f'outline-follow-{Path(args.binary).resolve().parent.name}-cpu-{args.cpu_rate:g}.json'
output.parent.mkdir(exist_ok=True)
output.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
print(json.dumps({'result': result['result'], 'cases': [
    {key: value for key, value in case.items() if key != 'runs'} for case in result['cases']],
    'artifact': str(output)}, ensure_ascii=False))
