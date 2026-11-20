/* Trusted application code. Document data enters only through readerSetDocument. */
'use strict';
const root = document.getElementById('document');
const scripts = new Map();
let revision = 0;
let observer;
let highlightObserver;
let diagramQueue = Promise.resolve();
let state = {zoom:1, heading:null, navigation:0, query:'', occurrence:0, searchRequest:0,
  theme:{id:document.documentElement.dataset.theme,dark:document.documentElement.dataset.dark==='true'}};
let appliedNavigation = -1, appliedSearch = -1, reloadTicket = 0, loading = false;
let viewerDialog;
const notify = (kind, detail = {}) => window.ipc?.postMessage(JSON.stringify({kind, ...detail}));
function loadScript(name) {
  if (!scripts.has(name)) scripts.set(name, new Promise((resolve, reject) => {
    const script = document.createElement('script'); script.src = `/vendor/${name}.js`;
    script.onload = resolve; script.onerror = () => reject(new Error(`Cannot load offline ${name}`)); document.head.append(script);
  }));
  return scripts.get(name);
}
function loadStyle(name) {
  if (document.querySelector(`link[data-library="${name}"]`)) return;
  const css = document.createElement('link'); css.rel = 'stylesheet'; css.href = `/vendor/${name}.css`; css.dataset.library = name; document.head.append(css);
}
function button(label, action, tools) {
  const element = document.createElement('button'); element.type = 'button'; element.textContent = label;
  element.addEventListener('click', action); tools.append(element); return element;
}
async function renderDiagram(box, code, index, currentRevision) {
  if (revision !== currentRevision || !box.isConnected) return;
  const began = performance.now(), requestedTheme = state.theme?.id;
  try {
    if (index >= 64 || code.length > 262144) throw new Error('Diagram exceeds the 64 diagrams / 256 KiB limit');
    if (/%%\s*\{|^\s*---/m.test(code)) throw new Error('Document Mermaid configuration is disabled');
    await loadScript('mermaid');
    if (revision !== currentRevision || requestedTheme !== state.theme?.id) return;
    mermaid.initialize({startOnLoad: false, securityLevel: 'strict', htmlLabels: false, maxTextSize: 262144, maxEdges: 1500,
      suppressErrorRendering: true, flowchart: {htmlLabels: false}, theme: state.theme?.dark ? 'dark' : 'default'});
    const {svg} = await mermaid.render(`diagram-${currentRevision}-${index}`, code);
    if (revision !== currentRevision || !box.isConnected || requestedTheme !== state.theme?.id) return;
    const tools = document.createElement('div'); tools.className = 'diagram-tools';
    const stage = document.createElement('div'); stage.className = 'diagram-stage';
    stage.innerHTML = DOMPurify.sanitize(svg, {USE_PROFILES: {svg:true, svgFilters:true}, FORBID_TAGS: ['foreignObject', 'a', 'image', 'script']});
    const drawing = stage.querySelector('svg');
    if (!drawing) throw new Error('Renderer did not produce SVG');
    // SVG-only labels preserve text selection. Never mount an <img> or bind diagram event handlers.
    stage.querySelectorAll('*').forEach(el => { for (const attr of [...el.attributes]) {
      if (/^on/i.test(attr.name) || /href$/i.test(attr.name)) el.removeAttribute(attr.name);
    }});
    const viewBox = drawing.viewBox.baseVal;
    const width = viewBox.width || Number.parseFloat(drawing.getAttribute('width')) || 800;
    const height = viewBox.height || Number.parseFloat(drawing.getAttribute('height')) || 400;
    let scale = Math.min(1, Math.max(0.05, (box.clientWidth - 34) / width));
    const label = document.createElement('span');
    const zoom = factor => { scale = Math.max(0.05, Math.min(8, scale * factor)); drawing.style.width = `${width*scale}px`; drawing.style.height = `${height*scale}px`; label.textContent = `${Math.round(scale*100)}%`; };
    button('−', () => zoom(1/1.25), tools); button('+', () => zoom(1.25), tools);
    button('Fit', () => { scale = Math.min(1, Math.max(0.05, (stage.clientWidth-32)/width)); zoom(1); stage.scrollTo(0,0); }, tools);
    const pan = button('Pan', () => { stage.classList.toggle('pan'); pan.setAttribute('aria-pressed', String(stage.classList.contains('pan'))); }, tools);
    pan.setAttribute('aria-pressed', 'false');
    const expand = button('Expand', () => showDiagram(drawing, stage, width, height, expand), tools);
    expand.title = '展开查看';
    tools.append(label, document.createTextNode('Select text normally · enable Pan to drag · Ctrl+wheel to zoom'));
    let drag;
    stage.addEventListener('pointerdown', event => {
      if (!stage.classList.contains('pan') || event.button !== 0) return;
      drag = {x:event.clientX, y:event.clientY, left:stage.scrollLeft, top:stage.scrollTop};
      stage.setPointerCapture(event.pointerId); event.preventDefault();
    });
    stage.addEventListener('pointermove', event => { if (drag) { stage.scrollLeft=drag.left-(event.clientX-drag.x); stage.scrollTop=drag.top-(event.clientY-drag.y); }});
    const release = () => {drag=undefined;}; stage.addEventListener('pointerup', release); stage.addEventListener('pointercancel', release);
    stage.addEventListener('wheel', event => { if(event.ctrlKey) { event.preventDefault(); event.stopPropagation(); zoom(Math.exp(-event.deltaY*.002)); } }, {passive:false});
    box.replaceChildren(tools, stage); zoom(1);
    notify('diagram-ready', {index, ms:performance.now()-began, nodes:drawing.querySelectorAll('*').length});
  } catch(error) {
    if (revision !== currentRevision || !box.isConnected || requestedTheme !== state.theme?.id) return;
    const message = document.createElement('div'); message.className = 'diagram-error'; message.textContent = `Mermaid: ${error.message || error}`;
    const source = document.createElement('pre'); source.textContent = code; box.replaceChildren(message,source);
    notify('diagram-error', {index});
  }
}
window.readerSetDocument = async function(html, reset = false) {
  dismissViewer();
  const current = ++revision, began = performance.now(), scroll = scrollY;
  observer?.disconnect();
  highlightObserver?.disconnect();
  // Rust already sanitizes; repeat here to keep future callers from introducing executable HTML.
  if (html !== null) root.innerHTML = DOMPurify.sanitize(html, {FORBID_TAGS:['style','script','iframe','object','form','input'], FORBID_ATTR:['style','id','name']});
  root.querySelectorAll('img').forEach(img => { img.loading='lazy'; img.decoding='async'; });
  scrollSync.indexDocument(reset);
  const slugs = new Map();
  root.querySelectorAll('h1,h2,h3,h4,h5,h6').forEach((h,i) => {
    h.dataset.headingIndex=i;
    const slug=h.textContent.trim().toLowerCase().replace(/[^\p{L}\p{N}_\-\s]/gu,'').replace(/\s+/g,'-');
    const count=slugs.get(slug)||0;slugs.set(slug,count+1);h.id=count?`${slug}-${count}`:slug;
  });
  observer = new IntersectionObserver(entries => {
    for(const entry of entries) if(entry.isIntersecting) { observer.unobserve(entry.target); const {code,index} = entry.target.readerDiagram;
      diagramQueue = diagramQueue.catch(()=>{}).then(()=>renderDiagram(entry.target,code,index,current));
    }
  }, {rootMargin:'300px'});
  root.querySelectorAll('pre > code.language-mermaid').forEach((code,index) => {
    const box=document.createElement('section'); box.className='diagram'; box.readerDiagram={code:code.textContent,index}; box.textContent='Mermaid · waiting to render'; code.parentElement.replaceWith(box); observer.observe(box);
  });
  if(root.querySelector('pre > code:not(.language-mermaid)')) {
    loadStyle('highlight'); loadScript('highlight').then(()=>{ if(revision===current) {
      highlightObserver = new IntersectionObserver(entries=>{for(const entry of entries) if(entry.isIntersecting) {
        highlightObserver.unobserve(entry.target); if(revision===current) try{hljs.highlightElement(entry.target);}catch{}
      }}, {rootMargin:'300px'});
      root.querySelectorAll('pre > code').forEach(code=>highlightObserver.observe(code));
    }}).catch(()=>{});
  }
  if(root.querySelector('.math-inline,.math-display')) {
    loadStyle('katex'); loadScript('katex').then(()=>{if(revision===current) root.querySelectorAll('.math-inline,.math-display').forEach(el=>{
      try{katex.render(el.textContent,el,{displayMode:el.classList.contains('math-display'),throwOnError:false,trust:false,strict:'error',maxExpand:1000});}catch{}
    });}).catch(()=>{});
  }
  scrollTo(0,reset?0:scroll);
  applyState();
  requestAnimationFrame(()=>requestAnimationFrame(()=>{if(revision===current) notify('document-readable',{ms:performance.now()-began, revision:current});}));
};
window.readerNavigate = (heading) => {
  scrollSync.previewIntent();
  root.querySelector(`[data-heading-index="${heading}"]`)?.scrollIntoView();
};
window.readerFind = (query, occurrence=0) => {
  getSelection().removeAllRanges(); CSS.highlights?.delete('search');
  if(!query) return;
  const walker = document.createTreeWalker(root,NodeFilter.SHOW_TEXT,{acceptNode(node){return node.parentElement.closest('style,script,.diagram-tools,.katex-mathml') ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT;}});
  const nodes=[]; let text='',node;
  while((node=walker.nextNode())) {nodes.push({node,start:text.length,end:text.length+node.length});text+=node.textContent;}
  const pattern = new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g,'\\$&'),'giu');
  const ranges=[];
  for(const match of text.matchAll(pattern)) {
    const start=nodes.find(n=>n.end>match.index),end=nodes.find(n=>n.end>=match.index+match[0].length);
    if(!start || !end) continue;
    const range=document.createRange();range.setStart(start.node,match.index-start.start);range.setEnd(end.node,match.index+match[0].length-end.start);ranges.push(range);
  }
  if(!ranges.length) return;
  if(CSS.highlights && window.Highlight) CSS.highlights.set('search',new Highlight(...ranges));
  const selected=ranges[Math.min(occurrence,ranges.length-1)]; getSelection().addRange(selected);
  selected.startContainer.parentElement.scrollIntoView({block:'center'});
};
window.readerZoom = factor => {
  document.documentElement.style.fontSize=`${16*factor}px`;
  // The configured width is a 100% baseline. Enlarging type can use spare
  // viewport space; the block's auto width still prevents horizontal overflow.
  const width=Number.isFinite(state.readingWidth)?state.readingWidth:980;
  root.style.setProperty('--reading-width',width===0?'none':`${width*(state.widthFollowsZoom===false?1:Math.max(1,factor))}px`);
};
function applyState() {
  window.readerZoom(state.zoom);
  if(loading) return;
  if(appliedNavigation!==state.navigation) {appliedNavigation=state.navigation;if(state.heading!==null)window.readerNavigate(state.heading);}
  if(appliedSearch!==state.searchRequest) {
    appliedSearch=state.searchRequest;window.readerFind(state.query,state.occurrence);
    // Updating the browser selection can move native HWND focus into WebView2.
    if(state.nativeSearchFocus) notify('search-applied',{request:state.searchRequest});
  }
}
window.readerConfigure = next => {
  const themeChanged = next.theme && state.theme?.id !== next.theme.id;
  if (themeChanged) dismissViewer();
  state=next;
  scrollSync.configure(next.sync);
  if (next.theme) {
    const element = document.documentElement;
    element.dataset.theme = next.theme.id;
    element.dataset.dark = String(next.theme.dark);
    element.style.colorScheme = next.theme.dark ? 'dark' : 'light';
    for (const [name, value] of Object.entries(next.theme.colors)) element.style.setProperty(`--${name}`, value);
    if (themeChanged) root.querySelectorAll('.diagram').forEach(box => {
      if (!box.readerDiagram) return;
      box.textContent = 'Mermaid · waiting to render';
      observer?.observe(box);
    });
  }
  applyState();
};
root.addEventListener('click', event => {
  const image=event.target.closest('img');
  if(image && !image.closest('a') && image.naturalWidth) {showImage(image);return;}
  const link=event.target.closest('a'); if(!link) return;
  const href=link.getAttribute('href'); event.preventDefault();
  if(href?.startsWith('#')) {try{document.getElementById(decodeURIComponent(href.slice(1)))?.scrollIntoView();}catch{}}
  else if(href?.startsWith('/link/')) notify('document-link',{id:href});
  else if(href && /^https?:|^mailto:/i.test(href)) notify('link',{url:href});
});
function dismissViewer() {
  viewerDialog?.readerClose();
}
function showDiagram(drawing, sourceStage, width, height, opener) {
  dismissViewer();
  // Move the original SVG, avoiding duplicate IDs, another Mermaid render, or
  // rasterization. A same-size placeholder keeps inline layout and scroll range.
  const originalStyle = drawing.getAttribute('style');
  const left = sourceStage.scrollLeft, top = sourceStage.scrollTop;
  const placeholder = document.createElement('div');
  const bounds = drawing.getBoundingClientRect();
  placeholder.style.width = `${bounds.width}px`; placeholder.style.height = `${bounds.height}px`;
  drawing.replaceWith(placeholder);
  showViewer(drawing, width, height, {
    diagram:true, opener,
    restore:() => {
      if (originalStyle === null) drawing.removeAttribute('style');
      else drawing.setAttribute('style', originalStyle);
      if (placeholder.isConnected) {
        placeholder.replaceWith(drawing);
        sourceStage.scrollLeft=left; sourceStage.scrollTop=top;
      }
    }
  });
}
function showImage(image) {
  dismissViewer();
  const picture=image.cloneNode(); picture.draggable=false; picture.loading='eager'; picture.style.maxWidth='none';
  showViewer(picture,image.naturalWidth,image.naturalHeight,{opener:image});
}
function showViewer(content, width, height, {diagram=false, opener, restore=()=>{}}={}) {
  const dialog=document.createElement('dialog'); viewerDialog=dialog; dialog.className='image-viewer';
  dialog.setAttribute('aria-label',diagram?'Mermaid diagram viewer':'Image viewer');
  const documentScroll={x:scrollX,y:scrollY};
  const tools=document.createElement('div'); tools.className='diagram-tools';
  const stage=document.createElement('div'); stage.className='diagram-stage pan'; stage.append(content);
  let scale=1, drag, closed=false;
  const label=document.createElement('span'); label.className='viewer-zoom'; label.setAttribute('aria-live','polite');
  const zoom=factor=>{
    scale=Math.max(.05,Math.min(8,scale*factor));
    content.style.width=`${width*scale}px`; content.style.height=`${height*scale}px`;
    label.textContent=`${Math.round(scale*100)}%`;
  };
  const fit=()=>{scale=Math.min(1,Math.max(.05,Math.min((stage.clientWidth-32)/width,(stage.clientHeight-32)/height)));zoom(1);stage.scrollTo(0,0);};
  button('−',()=>zoom(1/1.25),tools); button('+',()=>zoom(1.25),tools);
  button('Fit',fit,tools); button('100%',()=>{scale=1;zoom(1);},tools);
  if (diagram) {
    const setMode=pan=>{
      drag=undefined;
      stage.classList.toggle('pan',pan);
      move.setAttribute('aria-pressed',String(pan)); select.setAttribute('aria-pressed',String(!pan));
      if(pan) getSelection().removeAllRanges();
    };
    const move=button('Move',()=>setMode(true),tools); move.title='移动图表';
    const select=button('Select text',()=>setMode(false),tools); select.title='选择文字';
    setMode(true);
  }
  const close=()=>{
    if(closed) return;
    closed=true;
    if(dialog.open) dialog.close();
    restore(); dialog.remove();
    if(viewerDialog===dialog) viewerDialog=undefined;
    opener?.focus({preventScroll:true}); scrollTo(documentScroll.x,documentScroll.y);
  };
  dialog.readerClose=close;
  button('Close',close,tools); tools.append(label);
  stage.addEventListener('pointerdown',e=>{
    if(e.button!==0 || !stage.classList.contains('pan')) return;
    drag={x:e.clientX,y:e.clientY,left:stage.scrollLeft,top:stage.scrollTop};
    stage.setPointerCapture(e.pointerId); e.preventDefault();
  });
  stage.addEventListener('pointermove',e=>{if(drag){stage.scrollLeft=drag.left-e.clientX+drag.x;stage.scrollTop=drag.top-e.clientY+drag.y;}});
  stage.addEventListener('pointerup',()=>drag=undefined); stage.addEventListener('pointercancel',()=>drag=undefined);
  stage.addEventListener('wheel',e=>{if(e.ctrlKey){e.preventDefault();e.stopPropagation();zoom(Math.exp(-e.deltaY*.002));}},{passive:false});
  // Native dialog modality does not isolate scrolling. The content area uses
  // overscroll containment; toolbar/backdrop wheels must not reach the article.
  dialog.addEventListener('wheel',e=>{
    if(!stage.contains(e.target) || e.ctrlKey) {e.preventDefault();e.stopPropagation();}
  },{passive:false});
  dialog.addEventListener('keydown',e=>{
    if(!e.ctrlKey || e.altKey || e.metaKey) return;
    if(!['+','=','-','0'].includes(e.key)) return;
    e.preventDefault(); e.stopPropagation();
    if(e.key==='0') {scale=1;zoom(1);} else zoom(e.key==='-'?1/1.25:1.25);
  });
  dialog.append(tools,stage); document.body.append(dialog);
  dialog.addEventListener('cancel',e=>{e.preventDefault();close();});
  dialog.addEventListener('close',close);
  dialog.showModal(); if(diagram) fit(); else zoom(1);
}
document.addEventListener('keydown', event => {
  if(event.key==='Enter' && state.searchOpen && !viewerDialog && !event.ctrlKey && !event.altKey && !event.metaKey) {
    event.preventDefault(); notify('search-step',{backward:event.shiftKey}); return;
  }
  if(event.key==='Escape' && !viewerDialog) {notify('escape');return;}
  if(!event.ctrlKey || event.altKey) return;
  const key=event.key.toLowerCase();
  if(['o','s','f','e','n','1','2','3','+','=','-','0'].includes(key)) {event.preventDefault(); notify('shortcut',{key,shift:event.shiftKey});}
});
document.addEventListener('wheel',event=>{
  if(event.ctrlKey && !event.target.closest('.diagram-stage')) {event.preventDefault();notify('zoom-wheel',{delta:event.deltaY});}
},{passive:false});

// Source ranges are generated by Rust; geometry is read from the live layout.
// Only root scrolling is synchronized, never a diagram/image viewer's viewport.
function createScrollSync() {
  let config={enabled:false,revision:0,base:0}, blocks=[], pending, leader='preview', frame=0;
  const enabled=()=>config.enabled && !loading && !viewerDialog && blocks.length;
  const maxScroll=()=>Math.max(0,document.documentElement.scrollHeight-innerHeight);
  const top=block=>block.element.getBoundingClientRect().top+scrollY;
  const end=index=>index+1<blocks.length?top(blocks[index+1]):Math.max(top(blocks[index])+blocks[index].element.getBoundingClientRect().height,document.documentElement.scrollHeight);
  const indexAt=y=>{
    let low=0,high=blocks.length;
    while(low<high) {const middle=(low+high)>>1;if(top(blocks[middle])<=y+.5)low=middle+1;else high=middle;}
    return Math.max(0,low-1);
  };
  const report=()=>{
    if(!enabled() || leader!=='preview') return;
    const index=indexAt(scrollY), block=blocks[index], start=top(block), finish=end(index);
    notify('scroll-position',{revision:config.revision,start:block.start+config.base,
      fraction:Math.max(0,Math.min(1,(scrollY-start)/Math.max(1,finish-start))),
      edge:scrollY<=.5?-1:scrollY>=maxScroll()-.5?1:0});
  };
  const apply=()=>{
    if(!enabled() || leader!=='source' || !pending || pending.revision!==config.revision) return;
    const index=blocks.findIndex(block=>block.start+config.base===pending.start);
    if(index<0) return;
    const y=pending.edge===-1?0:pending.edge===1?maxScroll():top(blocks[index])+(end(index)-top(blocks[index]))*pending.fraction;
    scrollTo(scrollX,Math.max(0,Math.min(maxScroll(),y)));
  };
  const schedule=()=>{if(!frame)frame=requestAnimationFrame(()=>{frame=0;if(leader==='source')apply();else report();});};
  const previewIntent=()=>{if(viewerDialog)return;leader='preview';pending=undefined;};
  document.addEventListener('wheel',e=>{if(!e.ctrlKey)previewIntent();},{capture:true,passive:true});
  document.addEventListener('pointerdown',e=>{if(!e.target.closest('button,a'))previewIntent();},{capture:true});
  document.addEventListener('keydown',e=>{if(!e.ctrlKey && ['PageDown','PageUp','Home','End','ArrowDown','ArrowUp',' '].includes(e.key))previewIntent();},{capture:true});
  window.addEventListener('scroll',()=>{if(leader==='preview')schedule();},{passive:true});
  // Layout changes are event-driven, not a timer or a disk-backed cache.
  new ResizeObserver(schedule).observe(root);
  window.addEventListener('resize',schedule);
  return {
    configure(next) {
      const changed=next?.revision!==config.revision || !!next?.enabled!==config.enabled;
      const wasEnabled=config.enabled;
      config={enabled:false,revision:0,base:0,...next};
      if(changed) {pending=undefined;leader=wasEnabled?'source':'preview';schedule();}
    },
    indexDocument(reset) {
      blocks=[...root.querySelectorAll('.reading-section > .source-block[data-source-start][data-source-end]')]
        .map(element=>({element,start:Number(element.dataset.sourceStart),end:Number(element.dataset.sourceEnd)}))
        .filter(block=>Number.isSafeInteger(block.start)&&block.start>=0&&block.end>=block.start);
      if(reset) {leader='preview';pending=undefined;}
      schedule();
    },
    previewIntent,
    follow(position) {
      if(!config.enabled || position.revision!==config.revision || !Number.isSafeInteger(position.start)
        || !Number.isFinite(position.fraction) || position.fraction<0 || position.fraction>1) return;
      leader='source';pending=position;schedule();
    },
  };
}
const scrollSync=createScrollSync();
window.readerScrollTo=position=>scrollSync.follow(position);

notify('ready');
window.readerSetDocument(null, true);
window.readerReload = async reset => { const ticket = ++reloadTicket; loading=true; const response = await fetch('/document'); const html = await response.text(); if(ticket===reloadTicket) {loading=false;window.readerSetDocument(html,reset);} };
