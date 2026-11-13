import mermaid from './mermaid.esm.min.mjs';
import { renderToString } from './chunks/mermaid.esm.min/katex-QVRI4OIV.mjs';

// This module runs only in the generation-pinned, allow-scripts-only iframe.
// Its output is XML for an image, never markup admitted into the parent DOM.
const moduleUrl = new URL(import.meta.url);
const parentOrigin = moduleUrl.origin;
const generation = moduleUrl.pathname.split('/').at(-2);
if (!/^[a-f0-9]{64}$/.test(generation)) throw Error('renderer requires an immutable generation');
const protocol = 3;
const sourceLimit = 50000;
const resultLimit = 8 * 1024 * 1024;
const nodeLimit = 128;
const coordinateLimit = 1000000;
const nodeIdPattern = /^[A-Za-z][A-Za-z0-9_-]{0,63}$/;
const encoder = new TextEncoder();
const secure = [
  'secure', 'securityLevel', 'startOnLoad', 'maxTextSize', 'maxEdges',
  'theme', 'themeVariables', 'themeCSS', 'look', 'fontFamily', 'altFontFamily',
  'fontSize', 'htmlLabels', 'dompurifyConfig', 'suppressErrorRendering',
  'legacyMathML', 'forceLegacyMathML', 'deterministicIds',
  'deterministicIDSeed', 'arrowMarkerAbsolute',
];
let busy = false;
let serial = 0;
const renderHost = document.createElement('div');
document.body.appendChild(renderHost);

function validNodeIds(ids) {
  if (!Array.isArray(ids) || ids.length > nodeLimit) return false;
  const unique = new Set();
  for (const id of ids) {
    if (typeof id !== 'string' || !nodeIdPattern.test(id) || unique.has(id)) return false;
    unique.add(id);
  }
  return true;
}

function finiteRect(values) {
  return Array.isArray(values) && values.length === 4
    && values.every(value => Number.isFinite(value) && Math.abs(value) <= coordinateLimit)
    && values[2] > 0 && values[3] > 0;
}

function readViewBox(root) {
  const values = (root.getAttribute('viewBox') || '').trim().split(/[\s,]+/).map(Number);
  if (!finiteRect(values)) throw Error('renderer returned an invalid view box');
  return values;
}

function nodeRect(root, element) {
  const inverse = root.getScreenCTM()?.inverse();
  const bounds = element.getBoundingClientRect();
  if (!inverse || bounds.width <= 0 || bounds.height <= 0) {
    throw Error('renderer returned an unmeasurable node');
  }
  const points = [
    new DOMPoint(bounds.left, bounds.top),
    new DOMPoint(bounds.right, bounds.top),
    new DOMPoint(bounds.left, bounds.bottom),
    new DOMPoint(bounds.right, bounds.bottom),
  ].map(point => point.matrixTransform(inverse));
  const xs = points.map(point => point.x);
  const ys = points.map(point => point.y);
  const rect = [
    Math.min(...xs),
    Math.min(...ys),
    Math.max(...xs) - Math.min(...xs),
    Math.max(...ys) - Math.min(...ys),
  ];
  if (!finiteRect(rect)) throw Error('renderer returned invalid node geometry');
  return rect;
}

function readGeometry(root, nodeIds) {
  const expected = new Map(nodeIds.map(id => [id, {
    element: null,
    prefix: `${root.id}-flowchart-${id}-`,
  }]));
  for (const element of root.querySelectorAll('.node')) {
    const authored = element.getAttribute('data-id');
    for (const [id, entry] of expected) {
      const generated = element.id.startsWith(entry.prefix)
        && /^\d+$/.test(element.id.slice(entry.prefix.length));
      if (authored !== id && !generated) continue;
      if (entry.element && entry.element !== element) {
        throw Error('renderer returned duplicate node geometry');
      }
      entry.element = element;
    }
  }
  const nodes = nodeIds.map(id => {
    const { element } = expected.get(id);
    if (!element) throw Error('renderer omitted requested node geometry');
    return { id, rect: nodeRect(root, element) };
  });
  return { viewBox: readViewBox(root), nodes };
}

function isRequest(data) {
  const kind = data?.kind;
  return data && data.type === 'render' && data.protocol === protocol
    && data.generation === generation
    && (kind === 'diagram' || kind === 'inline-math' || kind === 'display-math')
    && typeof data.requestId === 'string' && /^[a-z0-9-]{1,64}$/.test(data.requestId)
    && typeof data.channel === 'string' && /^[a-z0-9-]{1,64}$/.test(data.channel)
    && Number.isSafeInteger(data.revision) && data.revision >= 0
    && (data.theme === 'light' || data.theme === 'dark')
    && typeof data.source === 'string' && data.source.length <= sourceLimit
    && validNodeIds(data.nodeIds)
    && (kind === 'diagram' || data.nodeIds.length === 0);
}

async function renderDiagram(source, theme) {
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: 'strict',
    htmlLabels: true,
    theme: theme === 'dark' ? 'dark' : 'default',
    look: 'classic',
    fontFamily: 'Arial, sans-serif',
    maxTextSize: sourceLimit,
    maxEdges: 500,
    suppressErrorRendering: true,
    legacyMathML: false,
    forceLegacyMathML: false,
    arrowMarkerAbsolute: false,
    secure,
  });
  const rendered = await mermaid.render(`spice-diagram-${++serial}`, source, renderHost);
  // Mermaid serializes HTML namespaces. The opaque browser context repairs
  // them before XML serialization so foreignObject and native MathML survive.
  const inert = new DOMParser().parseFromString(rendered.svg, 'text/html');
  const parsedRoot = inert.querySelector('svg');
  if (!parsedRoot || parsedRoot.namespaceURI !== 'http://www.w3.org/2000/svg') {
    throw Error('renderer did not return SVG');
  }
  return document.importNode(parsedRoot, true);
}

function renderMath(source, display, theme) {
  const markup = renderToString(source, {
    displayMode: display,
    output: 'mathml',
    throwOnError: true,
    strict: 'error',
    trust: false,
    maxSize: 100,
    maxExpand: 1000,
  });
  const inert = new DOMParser().parseFromString(markup, 'text/html');
  const parsedMath = inert.querySelector('math');
  if (!parsedMath || parsedMath.namespaceURI !== 'http://www.w3.org/1998/Math/MathML') {
    throw Error('math renderer did not return MathML');
  }

  const body = document.createElement('span');
  body.setAttribute('xmlns', 'http://www.w3.org/1999/xhtml');
  body.style.cssText = [
    'display:inline-flex',
    'align-items:center',
    'width:max-content',
    'margin:0',
    'padding:1px',
    'white-space:nowrap',
    'font:16px/1.25 system-ui,-apple-system,BlinkMacSystemFont,\"Segoe UI\",sans-serif',
    `color:${theme === 'dark' ? '#f4f6f5' : '#171a18'}`,
  ].join(';');
  body.appendChild(document.importNode(parsedMath, true));
  renderHost.replaceChildren(body);
  containOverflow(body);
  const bounds = body.getBoundingClientRect();
  const width = Math.ceil(bounds.width);
  const height = Math.ceil(bounds.height);
  if (!finiteRect([0, 0, width, height])) throw Error('math renderer returned invalid geometry');

  const root = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  root.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
  root.setAttribute('viewBox', `0 0 ${width} ${height}`);
  root.setAttribute('width', String(width));
  root.setAttribute('height', String(height));
  const foreign = document.createElementNS('http://www.w3.org/2000/svg', 'foreignObject');
  foreign.setAttribute('width', String(width));
  foreign.setAttribute('height', String(height));
  foreign.appendChild(body);
  root.appendChild(foreign);
  return root;
}

// Firefox sizes a MathML box before display-style operators grow, so laid-out
// descendants can extend past it. Padding the host by that overflow keeps every
// glyph inside the measured box that becomes the image viewport.
function containOverflow(body) {
  const box = body.getBoundingClientRect();
  let { left, top, right, bottom } = box;
  for (const element of body.querySelectorAll('*')) {
    const rect = element.getBoundingClientRect();
    if (rect.width <= 0 && rect.height <= 0) continue;
    left = Math.min(left, rect.left);
    top = Math.min(top, rect.top);
    right = Math.max(right, rect.right);
    bottom = Math.max(bottom, rect.bottom);
  }
  const pad = overflow => `${1 + Math.ceil(overflow)}px`;
  body.style.padding = [box.top - top, right - box.right, bottom - box.bottom, box.left - left]
    .map(pad).join(' ');
}

addEventListener('message', async event => {
  if (event.source !== parent || event.origin !== parentOrigin || !isRequest(event.data)) return;
  const { requestId, channel, revision, theme, source, nodeIds, kind } = event.data;
  const envelope = { protocol, generation, requestId, channel, revision, theme, kind };
  const reply = data => parent.postMessage({ ...envelope, ...data }, parentOrigin);
  if (busy) {
    reply({ type: 'error', error: 'renderer busy' });
    return;
  }
  busy = true;
  try {
    const root = kind === 'diagram'
      ? await renderDiagram(source, theme)
      : renderMath(source, kind === 'display-math', theme);
    renderHost.replaceChildren(root);
    const geometry = readGeometry(root, nodeIds);
    const svg = new XMLSerializer().serializeToString(root);
    if (encoder.encode(svg).length > resultLimit) throw Error('SVG exceeds output bound');
    reply({ type: 'rendered', svg, geometry });
  } catch (error) {
    reply({ type: 'error', error: String(error).slice(0, 500) });
  } finally {
    renderHost.replaceChildren();
    busy = false;
  }
});
parent.postMessage({ type: 'ready', protocol, generation }, parentOrigin);
