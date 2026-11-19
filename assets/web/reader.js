/* Trusted application code. Document data enters only through readerSetDocument. */
'use strict';
const root = document.getElementById('document');
const scripts = new Map();
let revision = 0;
let observer;
let highlightObserver;
let diagramQueue = Promise.resolve();
let state = {zoom:1, heading:null, navigation:0, query:'', occurrence:0, searchRequest:0};
let appliedNavigation = -1, appliedSearch = -1, reloadTicket = 0, loading = false;
let imageDialog;
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
  const began = performance.now();
  try {
    if (index >= 64 || code.length > 262144) throw new Error('Diagram exceeds the 64 diagrams / 256 KiB limit');
    if (/%%\s*\{|^\s*---/m.test(code)) throw new Error('Document Mermaid configuration is disabled');
    await loadScript('mermaid');
    if (revision !== currentRevision) return;
    mermaid.initialize({startOnLoad: false, securityLevel: 'strict', htmlLabels: false, maxTextSize: 262144, maxEdges: 1500,
      suppressErrorRendering: true, flowchart: {htmlLabels: false}, theme: matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'default'});
    const {svg} = await mermaid.render(`diagram-${currentRevision}-${index}`, code);
    if (revision !== currentRevision || !box.isConnected) return;
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
    if (revision !== currentRevision || !box.isConnected) return;
    const message = document.createElement('div'); message.className = 'diagram-error'; message.textContent = `Mermaid: ${error.message || error}`;
    const source = document.createElement('pre'); source.textContent = code; box.replaceChildren(message,source);
    notify('diagram-error', {index});
  }
}
window.readerSetDocument = async function(html, reset = false) {
  imageDialog?.close(); imageDialog?.remove(); imageDialog=undefined;
  const current = ++revision, began = performance.now(), scroll = scrollY;
  observer?.disconnect();
  highlightObserver?.disconnect();
  // Rust already sanitizes; repeat here to keep future callers from introducing executable HTML.
  if (html !== null) root.innerHTML = DOMPurify.sanitize(html, {FORBID_TAGS:['style','script','iframe','object','form','input'], FORBID_ATTR:['style','id','name']});
  root.querySelectorAll('img').forEach(img => { img.loading='lazy'; img.decoding='async'; });
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
window.readerNavigate = (heading) => root.querySelector(`[data-heading-index="${heading}"]`)?.scrollIntoView();
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
window.readerZoom = factor => {document.documentElement.style.fontSize=`${16*factor}px`;};
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
window.readerConfigure = next => {state=next;applyState();};
root.addEventListener('click', event => {
  const image=event.target.closest('img');
  if(image && !image.closest('a') && image.naturalWidth) {showImage(image);return;}
  const link=event.target.closest('a'); if(!link) return;
  const href=link.getAttribute('href'); event.preventDefault();
  if(href?.startsWith('#')) {try{document.getElementById(decodeURIComponent(href.slice(1)))?.scrollIntoView();}catch{}}
  else if(href?.startsWith('/link/')) notify('document-link',{id:href});
  else if(href && /^https?:|^mailto:/i.test(href)) notify('link',{url:href});
});
function showImage(image) {
  imageDialog?.remove();
  const dialog=document.createElement('dialog');imageDialog=dialog;dialog.className='image-viewer';
  const tools=document.createElement('div');tools.className='diagram-tools';
  const stage=document.createElement('div');stage.className='diagram-stage pan';
  const picture=image.cloneNode();picture.draggable=false;picture.loading='eager';picture.style.maxWidth='none';stage.append(picture);
  let scale=1;
  const zoom=factor=>{scale=Math.max(.05,Math.min(8,scale*factor));picture.style.width=`${image.naturalWidth*scale}px`;picture.style.height=`${image.naturalHeight*scale}px`;};
  button('−',()=>zoom(1/1.25),tools);button('+',()=>zoom(1.25),tools);
  button('Fit',()=>{scale=Math.min(1,(stage.clientWidth-32)/image.naturalWidth,(stage.clientHeight-32)/image.naturalHeight);zoom(1);stage.scrollTo(0,0);},tools);
  button('100%',()=>{scale=1;zoom(1);},tools);button('Close',()=>dialog.close(),tools);
  let drag;
  stage.addEventListener('pointerdown',e=>{if(e.button!==0)return;drag={x:e.clientX,y:e.clientY,left:stage.scrollLeft,top:stage.scrollTop};stage.setPointerCapture(e.pointerId);e.preventDefault();});
  stage.addEventListener('pointermove',e=>{if(drag){stage.scrollLeft=drag.left-e.clientX+drag.x;stage.scrollTop=drag.top-e.clientY+drag.y;}});
  stage.addEventListener('pointerup',()=>drag=undefined);stage.addEventListener('pointercancel',()=>drag=undefined);
  stage.addEventListener('wheel',e=>{if(e.ctrlKey){e.preventDefault();e.stopPropagation();zoom(Math.exp(-e.deltaY*.002));}},{passive:false});
  dialog.append(tools,stage);document.body.append(dialog);dialog.showModal();zoom(1);
  dialog.addEventListener('close',()=>{dialog.remove();if(imageDialog===dialog)imageDialog=undefined;});
}
document.addEventListener('keydown', event => {
  if(event.key==='Enter' && state.searchOpen && !imageDialog && !event.ctrlKey && !event.altKey && !event.metaKey) {
    event.preventDefault(); notify('search-step',{backward:event.shiftKey}); return;
  }
  if(event.key==='Escape' && !imageDialog) {notify('escape');return;}
  if(!event.ctrlKey || event.altKey) return;
  const key=event.key.toLowerCase();
  if(['o','s','f','e','n','1','2','3','+','=','-','0'].includes(key)) {event.preventDefault(); notify('shortcut',{key,shift:event.shiftKey});}
});
document.addEventListener('wheel',event=>{
  if(event.ctrlKey && !event.target.closest('.diagram-stage')) {event.preventDefault();notify('zoom-wheel',{delta:event.deltaY});}
},{passive:false});
matchMedia('(prefers-color-scheme: dark)').addEventListener('change',()=>notify('theme-changed'));
notify('ready');
window.readerSetDocument(null, true);
window.readerReload = async reset => { const ticket = ++reloadTicket; loading=true; const response = await fetch('/document'); const html = await response.text(); if(ticket===reloadTicket) {loading=false;window.readerSetDocument(html,reset);} };
