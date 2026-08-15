// LAIN Command Center — SPA shell (Task 4.3)
//
// Vanilla JS, no framework. Talks to the running MCP server over the
// HTTP transport at POST /mcp. Vendored D3 v7 is available globally as
// `d3` for later tabs (graph rendering).

async function mcpCall(name, args) {
  const r = await fetch('/mcp', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({
      jsonrpc: '2.0',
      method: 'tools/call',
      params: {name, arguments: args || {}},
      id: 1,
    }),
  });
  const body = await r.json();
  return body.result;
}

function unwrapText(result) {
  // MCP tool results come back as { content: [{ type: "text", text: "..." }] }
  if (!result || !Array.isArray(result.content)) return null;
  const block = result.content.find(b => b && b.type === 'text');
  return block ? block.text : null;
}

async function init() {
  const reload = document.getElementById('status-reload');
  reload.textContent = 'reload: loading…';
  try {
    const statusResult = await mcpCall('get_server_status');
    const status = statusResult ? JSON.parse(unwrapText(statusResult) || '{}') : {};
    document.getElementById('status-pid').textContent = `pid: ${status.pid ?? '?'}`;
    document.getElementById('status-transport').textContent = `transport: ${status.transport ?? '?'}`;
    document.getElementById('status-repos').textContent = `repos: ${status.repo_count ?? '?'}`;
    if (status.active_workspace) {
      document.getElementById('active-workspace').textContent =
        `workspace: ${status.active_workspace}`;
    }
  } catch (e) {
    reload.textContent = `reload: error (${e})`;
    return;
  }
  reload.textContent = 'reload: idle';
}

document.querySelectorAll('[data-tab]').forEach(btn => {
  btn.addEventListener('click', () => {
    document.querySelectorAll('.tab').forEach(t => { t.style.display = 'none'; });
    const target = document.getElementById('tab-' + btn.dataset.tab);
    if (target) target.style.display = 'block';
  });
});

init();
