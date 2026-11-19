"""Regression: start empty, then open Markdown while GPUI timers are active.

Uses the existing benchmark's document-open path, without native input automation.
Run: python scripts/test-delayed-web-preview.py --binary target/release/native-markdown.exe
"""
import argparse
import os
from pathlib import Path
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument('--binary', required=True)
parser.add_argument('--runs', type=int, default=3)
args = parser.parse_args()
repo = Path(__file__).resolve().parents[1]
(repo/'tmp').mkdir(exist_ok=True)
fixture = repo/'tmp/delayed-open-regression.md'
fixture.write_text('# Delayed open\n\nSelectable document after empty startup.\n', encoding='utf-8')
for run in range(args.runs):
    env = dict(os.environ, RUST_BACKTRACE='1',
               NATIVE_MARKDOWN_BENCHMARK='image-release',
               NATIVE_MARKDOWN_BENCHMARK_SECONDARY_DOCUMENT=str(fixture),
               NATIVE_MARKDOWN_BENCHMARK_WARMUP_MS='500',
               NATIVE_MARKDOWN_BENCHMARK_SWITCH_STEP='1',
               NATIVE_MARKDOWN_BENCHMARK_STEP_MS='50',
               NATIVE_MARKDOWN_BENCHMARK_SECONDS='5',
               NATIVE_MARKDOWN_BENCHMARK_MAX_PRIVATE_BYTES_MIB='4096',
               NATIVE_MARKDOWN_BENCHMARK_MAX_PRIVATE_WS_MIB='4096',
               NATIVE_MARKDOWN_BENCHMARK_MAX_GROWTH_MIB='4096')
    # Exercise the normal profile; do not accidentally inherit a remote-debugging override.
    env.pop('NATIVE_MARKDOWN_WEB_TEST_ARGS', None)
    log = repo/f'tmp/delayed-open-{time.time_ns()}.log'
    with log.open('wb') as output:
        process = subprocess.Popen([str(Path(args.binary).resolve())], env=env,
                                   stdout=output, stderr=output)
        try:
            code = process.wait(timeout=25)
        except subprocess.TimeoutExpired:
            subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'], capture_output=True)
            process.wait(timeout=10)
            raise AssertionError(f'Opening document hung; log: {log}')
    captured = log.read_text(encoding='utf-8', errors='replace')
    assert code == 0, f'Opening document crashed (exit {code}); log: {log}\n{captured[:1500]}'
    assert 'event=document_switched' in captured, f'Document-open trigger missing: {log}'
    assert 'document-readable' in captured, f'Preview never became readable: {log}'
    assert 'panicked' not in captured and 'event=error' not in captured, f'GPUI failure: {log}'
    print(f'PASS delayed open {run+1}/{args.runs}: document readable, timers active, clean exit')
