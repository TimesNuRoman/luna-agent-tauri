#!/usr/bin/env python3
"""Patch Luna Agent HTML for Tauri compatibility"""
import re, sys, os

TAURI_HTML = '/workspace/luna-tauri/src-tauri/index.html'
with open(TAURI_HTML) as f: html = f.read()

print(f"Original size: {len(html)}")

# 1. Add Tauri API detection after <head>
tauri_api = '''
<script>
window.__TAURI__ = typeof window.__TAURI__ !== 'undefined';
window.__LUNA_TAURI__ = {
  async invoke(cmd, args) {
    if (typeof window.__TAURI__ !== 'undefined' && window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
      return await window.__TAURI__.core.invoke(cmd, args);
    }
    throw new Error('Not in Tauri');
  }
};
</script>
'''

if 'window.__TAURI__' not in html:
    html = html.replace('<head>', '<head>' + tauri_api)
    print("Added Tauri API detection")

# 2. Patch batchWebSearch to support Tauri IPC
# Find and replace the function
old_bs = "async function batchWebSearch(cfg) {\n  // Use OpenClaw MCP tool via internal API"
if old_bs in html:
    new_bs = """async function batchWebSearch(cfg) {
  // Try Tauri IPC first
  try {
    if (typeof window.__LUNA_TAURI__ !== 'undefined') {
      var q = cfg.queries[0].query;
      var num = cfg.queries[0].num_results || 5;
      var result = await window.__LUNA_TAURI__.invoke('search_news', {query: q, numResults: num});
      return { results: [{ results: result.results || [] }] };
    }
  } catch(e) {}
  // Use OpenClaw MCP tool via internal API"""
    html = html.replace(old_bs, new_bs)
    print("Patched batchWebSearch for Tauri IPC")
else:
    print("batchWebSearch pattern not found:", repr(html[html.find('function batchWebSearch'):html.find('function batchWebSearch')+100]))

# 3. Patch callMiniMax to support Tauri IPC
old_cm = "async function callMiniMax(messages) {\n  var api='https://api.minimax.chat/v/text/chatfunction_v2';"
if old_cm in html:
    new_cm = """async function callMiniMax(messages) {
  // Try Tauri IPC first (hides API key)
  try {
    if (typeof window.__LUNA_TAURI__ !== 'undefined') {
      return await window.__LUNA_TAURI__.invoke('call_minimax', {messages: messages});
    }
  } catch(e) {}
  // Direct API call
  var api='https://api.minimax.chat/v/text/chatfunction_v2';"""
    html = html.replace(old_cm, new_cm)
    print("Patched callMiniMax for Tauri IPC")
else:
    print("callMiniMax pattern not found")

# 4. Fix callOpenClawTool - make it Tauri-compatible
old_oc = "function callOpenClawTool(tool,params){\n  return new Promise(function(resolve,reject){\n    var id='oc_'+(Date.now()+'_'+Math.random().toString(36).substr(2,9));\n    var pending=_ocPending=_ocPending||{};\n    pending[id]={resolve:resolve,reject:reject};\n    var timeout=setTimeout(function(){delete pending[id];reject(new Error('Timeout: '+tool+' (20s)'));},20000);\n    pending[id].timeout=timeout;\n    window.parent.postMessage(JSON.stringify({jsonrpc:'2.0',id:id,method:'tools/call',params:{name:tool,arguments:params}}),'*');\n  });\n}"
if old_oc in html:
    new_oc = """function callOpenClawTool(tool, params) {
  // Try Tauri IPC first
  try {
    if (typeof window.__LUNA_TAURI__ !== 'undefined') {
      return window.__LUNA_TAURI__.invoke('tool_' + tool, params);
    }
  } catch(e) {}
  // Fallback: OpenClaw postMessage bridge
  return new Promise(function(resolve, reject) {
    var id = 'oc_' + (Date.now() + '_' + Math.random().toString(36).substr(2, 9));
    var pending = _ocPending = _ocPending || {};
    pending[id] = { resolve: resolve, reject: reject };
    var timeout = setTimeout(function() { delete pending[id]; reject(new Error('Timeout: ' + tool + ' (20s)')); }, 20000);
    pending[id].timeout = timeout;
    window.parent.postMessage(JSON.stringify({ jsonrpc: '2.0', id: id, method: 'tools/call', params: { name: tool, arguments: params } }), '*');
  });
}"""
    html = html.replace(old_oc, new_oc)
    print("Patched callOpenClawTool for Tauri IPC")
else:
    print("callOpenClawTool pattern found (old format)")

# 5. Add appType meta for Tauri window
old_viewport = 'content="width=device-width, initial-scale=1.0, viewport-fit=cover"'
new_viewport = 'content="width=device-width, initial-scale=1.0, viewport-fit=cover, user-scalable=no, maximum-scale=1.0"'
if old_viewport in html and 'maximum-scale' not in html:
    html = html.replace(old_viewport, new_viewport)
    print("Fixed viewport meta for Tauri")

# 6. Update title
if '<title>Luna Agent</title>' in html:
    html = html.replace('<title>Luna Agent</title>', '<title>Luna Agent</title>')
    print("Title OK")

# 7. Add Tauri-specific styles (hide desktop sidebar on mobile-like Tauri window)
# Check if responsive CSS exists
if '@media(min-width:769px)' not in html:
    tauri_css = '''
<style>
.app { display: flex; flex-direction: column; height: 100dvh; width: 100%; overflow: hidden; }
.desktop-sidebar { display: none !important; }
@media (min-width: 769px) { .desktop-sidebar { display: flex !important; width: 260px; } .app { flex-direction: row; } }
#bottomNav { display: flex !important; }
@media (min-width: 769px) { #bottomNav { display: none !important; } }
</style>'''
    html = html.replace('<head>', '<head>' + tauri_css)
    print("Added Tauri CSS fixes")
else:
    print("Responsive CSS already present")

print(f"Patched size: {len(html)}")

with open(TAURI_HTML, 'w') as f:
    f.write(html)
print("Saved to", Tauri_HTML)

# Verify key patterns
checks = [
    ('window.__TAURI__', 'Tauri detection'),
    ('window.__LUNA_TAURI__', 'Luna Tauri bridge'),
    ('invoke("call_minimax"', 'MiniMax Tauri IPC'),
    ('invoke("search_news"', 'News Tauri IPC'),
    ('</html>', 'HTML closes properly'),
]
for pat, name in checks:
    print(f"  {'OK' if pat in html else 'MISSING':6} {name}")
