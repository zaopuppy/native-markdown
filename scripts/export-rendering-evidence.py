"""Keep compact, reviewable per-run evidence alongside the evaluation.

Usage: python scripts/export-rendering-evidence.py docs/measurements/file.json tmp/baseline-final.json tmp/final-webview.json ...
Full time series remain at the supplied paths; this exports peaks, the final process breakdown,
an idle CPU estimate, and application timing/error records. All memory values are bytes.
"""
import hashlib
import json
from pathlib import Path
import sys

output, *inputs = sys.argv[1:]
exports=[]
for filename in inputs:
    data=json.loads(Path(filename).read_text(encoding='utf-8-sig'))
    rows=[]
    for run in data['results']:
        samples=[s for s in run['samples'] if s['processes']]
        last=samples[-1]
        first=min(samples,key=lambda s:abs(s['ms']-(last['ms']-3000)))
        before={p['pid']:p['cpuSeconds'] for p in first['processes']}
        cpu=sum(max(0,p['cpuSeconds']-before[p['pid']]) for p in last['processes'] if p['pid'] in before)*100000/(last['ms']-first['ms'])
        processes=[]
        for p in last['processes']:
            role='host'
            if '--native-markdown-mermaid-worker' in p['command']: role='merman-worker'
            elif p['name']=='msedgewebview2.exe':
                role=next((part.removeprefix('--type=') for part in p['command'].split() if part.startswith('--type=')),'browser')
            processes.append(dict(p,role=role))
        rows.append({'run':run['run'],'scenario':run['scenario'],'windowHandleMs':run['windowVisibleMs'],
                     'peakPrivateBytes':max(sum(p['privateBytes'] for p in s['processes']) for s in samples),
                     'peakPrivateWS':max(sum(p['privateWS'] for p in s['processes']) for s in samples),
                     'finalSampleMs':last['ms'],'idleCPUPercentOfOneCore':cpu,'finalProcesses':processes,
                     'events':[line for line in run['stdout'].splitlines() if line.startswith(('READER_EVENT','READER_SWITCH','NATIVE_MARKDOWN_BENCHMARK event=mermaid'))],
                     'stderr':run['stderr']})
    metadata={k:v for k,v in data.items() if k!='results'}
    metadata['rawSource']=filename
    if 'binarySHA256' not in metadata and Path(data['binary']).is_file():
        metadata['binarySHA256']=hashlib.sha256(Path(data['binary']).read_bytes()).hexdigest()
    exports.append({'environment':metadata,'runs':rows})
path=Path(output);path.parent.mkdir(parents=True,exist_ok=True)
path.write_text(json.dumps({'units':'bytes, milliseconds, percent of one logical core','batches':exports},ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
print(path, path.stat().st_size)
