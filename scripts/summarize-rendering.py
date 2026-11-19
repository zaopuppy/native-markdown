"""Summarize process-tree samples without treating shared Working Set as private."""
import json
import statistics
import re
import sys
from pathlib import Path

for filename in sys.argv[1:]:
    data = json.loads(Path(filename).read_text(encoding='utf-8-sig'))
    print(filename)
    print('scenario | final private commit median [min,max] MiB | peak private commit | final private WS | sum WS (shared counted) | window handle ms')
    for scenario in dict.fromkeys(r['scenario'] for r in data['results']):
        rows = [r for r in data['results'] if r['scenario'] == scenario]
        def total(sample, key): return sum(p[key] for p in sample['processes']) / 1048576
        values = [total(r['samples'][-1], 'privateBytes') for r in rows if r['samples']]
        if not values: continue
        peak = [max(total(s, 'privateBytes') for s in r['samples']) for r in rows if r['samples']]
        pws = [total(r['samples'][-1], 'privateWS') for r in rows if r['samples']]
        ws = [total(r['samples'][-1], 'ws') for r in rows if r['samples']]
        windows = [r['windowVisibleMs'] for r in rows if r['windowVisibleMs'] is not None]
        print(f"{scenario} | {statistics.median(values):.1f} [{min(values):.1f},{max(values):.1f}] | {statistics.median(peak):.1f} | {statistics.median(pws):.1f} | {statistics.median(ws):.1f} | {statistics.median(windows) if windows else 0:.0f}")
        cpu, readable, diagram, switches = [], [], [], []
        for r in rows:
            samples = [s for s in r['samples'] if s['processes']]
            if len(samples)>1:
                last=samples[-1]; first=min(samples,key=lambda s:abs(s['ms']-(last['ms']-3000)))
                before={p['pid']:p['cpuSeconds'] for p in first['processes']}
                delta=sum(max(0,p['cpuSeconds']-before[p['pid']]) for p in last['processes'] if p['pid'] in before)
                if last['ms']>first['ms']: cpu.append(delta*100000/(last['ms']-first['ms']))
            for line in r['stdout'].splitlines():
                match=re.search(r'READER_EVENT.*?body=(.*)',line)
                if match:
                    event=json.loads(match[1])
                    if event['kind']=='document-readable':
                        stamp=re.search(r'process_ms=(\d+)',line)
                        if stamp: readable.append(int(stamp[1]))
                    if event['kind']=='diagram-ready': diagram.append(event['ms'])
                match=re.search(r'READER_SWITCH.*handler_ms=(\d+)',line)
                if match: switches.append(int(match[1]))
        if cpu: print(f'  idle CPU % of one logical core: {statistics.median(cpu):.2f} [{min(cpu):.2f},{max(cpu):.2f}]; divide by {data["logicalCPUs"]} for machine percent')
        if readable: print(f'  DOM-readable process ms: {statistics.median(readable):.0f} [{min(readable)},{max(readable)}]')
        if diagram: print(f'  visible diagram render ms: {statistics.median(diagram):.1f} [{min(diagram):.1f},{max(diagram):.1f}], completed={len(diagram)}')
        if switches: print(f'  Rust document switch handler ms: {statistics.median(switches):.0f} [{min(switches)},{max(switches)}]')
    # Last run includes the process-level attribution, not just the host executable.
    for r in [r for r in data['results'] if r['run'] == max(x['run'] for x in data['results'])]:
        print(r['scenario'], [(p['pid'], p['name'], round(p['privateBytes']/1048576,1), round(p['privateWS']/1048576,1), round(p['ws']/1048576,1)) for p in r['samples'][-1]['processes']])
