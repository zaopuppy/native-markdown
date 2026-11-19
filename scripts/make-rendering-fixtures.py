"""Deterministic common documents for native and WebView measurements."""
from pathlib import Path

root = Path(__file__).resolve().parents[1] / 'tests/fixtures/rendering'
root.mkdir(parents=True, exist_ok=True)
section = '''## Reading section {i}

English and 中文正文 with **bold**, *emphasis*, ~~deleted~~, [link](https://example.com), and `inline code`.

- First item
- Second item
  - Nested item

| Name | Explanation | Value |
| :--- | :---: | ---: |
| Alpha | A long table cell that can wrap when the window is narrow. | 42 |

```rust
fn main() {{ println!("hello"); }}
```

'''
(root / 'ordinary.md').write_text('# Ordinary document\n\n' + ''.join(section.format(i=i) for i in range(12)), encoding='utf-8')
(root / 'large.md').write_text('# Large document\n\n' + ''.join(section.format(i=i) for i in range(1600)), encoding='utf-8')
diagrams = ['# Multiple Mermaid diagrams\n\n正文 remains selectable.\n\n']
for i in range(8):
    diagrams.append(f'## Diagram {i}\n\n```mermaid\nflowchart LR\n')
    diagrams.extend(f' N{j}[节点 {j}] --> N{j+1}[节点 {j+1}]\n' for j in range(24))
    diagrams.append('```\n\nText after diagram.\n\n')
(root / 'mermaid.md').write_text(''.join(diagrams), encoding='utf-8')
(root / 'mermaid-large.md').write_text('# Large Mermaid graph\n\n```mermaid\nflowchart LR\n' + ''.join(f' N{j}[Node {j}] --> N{j+1}[Node {j+1}]\n' for j in range(350)) + '```\n', encoding='utf-8')
print([(p.name, p.stat().st_size) for p in root.glob('*.md')])
