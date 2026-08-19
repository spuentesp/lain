// LAIN Command Center — SPA (Tasks 4.3–4.9)
//
// Vanilla JS, no framework. Talks to the running MCP server over the
// HTTP transport at POST /mcp. Tool responses come back as
// {content:[{type:"text",text:"..."}]}; use `unwrapText` to get the
// payload and `parseJson` to decode the JSON body.
//
// Sections:
//   - Topbar: active project / active workspace
//   - Sidebar: workspaces list, repos list, recent projects list
//   - Tabs: overview, graph, repos, query, tools
//   - Status bar: pid, transport, repo count + last sync — polled every 2s

// ── MCP helpers ────────────────────────────────────────────────────────────

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
  if (body.error) throw new Error(body.error.message || 'rpc error');
  return body.result;
}

function unwrapText(result) {
  if (!result || !Array.isArray(result.content)) return null;
  const block = result.content.find(b => b && b.type === 'text');
  return block ? block.text : null;
}

function parseJson(result) {
  const text = unwrapText(result);
  if (text == null) return null;
  try { return JSON.parse(text); } catch (_) { return null; }
}

function escapeHtml(s) {
  return String(s == null ? '' : s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

// ── Sidebar: workspaces ────────────────────────────────────────────────────

async function renderWorkspaces() {
  const ul = document.getElementById('workspaces');
  ul.innerHTML = '';
  let result;
  try {
    result = await mcpCall('list_workspaces');
  } catch (e) {
    ul.innerHTML = `<li class="error">list_workspaces failed: ${escapeHtml(e.message)}</li>`;
    return;
  }
  if (result && result.isError) {
    ul.innerHTML = `<li class="error">${escapeHtml(unwrapText(result) || 'error')}</li>`;
    return;
  }
  const list = parseJson(result) || [];
  if (!Array.isArray(list) || list.length === 0) {
    ul.innerHTML = '<li class="muted">no workspaces</li>';
    return;
  }
  for (const ws of list) {
    const li = document.createElement('li');
    li.textContent = `${ws.name} (${ws.member_count})`;
    if (ws.is_active) li.classList.add('active');
    ul.appendChild(li);
  }
  // Reflect active workspace in the topbar.
  const active = list.find(ws => ws.is_active);
  if (active) {
    document.getElementById('active-workspace').textContent =
      `workspace: ${active.name}`;
  }
}

// ── Sidebar: repos (compact view) ──────────────────────────────────────────

async function renderReposSidebar() {
  const ul = document.getElementById('repos');
  ul.innerHTML = '';
  let list;
  try {
    const result = await mcpCall('list_repos');
    list = parseJson(result) || [];
  } catch (e) {
    ul.innerHTML = `<li class="error">list_repos failed: ${escapeHtml(e.message)}</li>`;
    return;
  }
  if (!Array.isArray(list) || list.length === 0) {
    ul.innerHTML = '<li class="muted">no repos</li>';
    return;
  }
  for (const r of list) {
    const li = document.createElement('li');
    li.textContent = `${r.id} (${r.health})`;
    ul.appendChild(li);
  }
}

// ── Sidebar: recent projects ───────────────────────────────────────────────

async function renderRecentProjects() {
  const ul = document.getElementById('recent-projects');
  ul.innerHTML = '';
  let list;
  try {
    const result = await mcpCall('list_recent_projects');
    list = parseJson(result) || [];
  } catch (e) {
    ul.innerHTML = `<li class="error">list_recent_projects failed: ${escapeHtml(e.message)}</li>`;
    return;
  }
  if (!Array.isArray(list) || list.length === 0) {
    ul.innerHTML = '<li class="muted">no recent projects</li>';
    return;
  }
  for (const p of list) {
    const li = document.createElement('li');
    li.classList.add('recent-project');
    const ws = p.active_workspace ? ` (active: ${escapeHtml(p.active_workspace)})` : '';
    li.innerHTML = `
      <code>${escapeHtml(p.path)}</code>
      <span class="muted">${p.workspace_count} ws / ${p.repo_count} repos${ws}</span>
      <button data-path="${escapeHtml(p.path)}" data-ws="${escapeHtml(p.active_workspace || '')}">Copy restart cmd</button>
    `;
    li.querySelector('button').addEventListener('click', () => {
      const cmd = `lain server --config ${p.path}${p.active_workspace ? ' --workspace ' + p.active_workspace : ''}`;
      navigator.clipboard.writeText(cmd).catch(() => {});
      li.querySelector('button').textContent = 'Copied!';
      setTimeout(() => { li.querySelector('button').textContent = 'Copy restart cmd'; }, 1500);
    });
    ul.appendChild(li);
  }
}

// ── Sidebar: agents online (Task 9) ─────────────────────────────────────────

async function renderAgentsOnline() {
  const ul = document.getElementById('agents-online');
  if (!ul) return;
  ul.innerHTML = '';
  let list;
  try {
    const result = await mcpCall('list_active_agents', {include_background: false});
    list = parseJson(result) || [];
  } catch (e) {
    ul.innerHTML = `<li class="error">list_active_agents failed: ${escapeHtml(e.message)}</li>`;
    return;
  }
  if (!Array.isArray(list) || list.length === 0) {
    ul.innerHTML = '<li class="muted">no agents online</li>';
    return;
  }
  for (const a of list) {
    const li = document.createElement('li');
    li.className = 'agent-row';
    li.dataset.agentId = a.agent_id;
    li.innerHTML = `
      <strong>${escapeHtml(a.name)}</strong>
      <span class="kind">${escapeHtml(a.kind || '')}</span>
      <span class="mode">${escapeHtml(a.mode || '')}</span>
      <span class="claims">${a.claims_count ?? 0} claims</span>
    `;
    ul.appendChild(li);
  }
}

// ── Sidebar: rooms (claimed files, Task 9) ──────────────────────────────────

async function renderRooms() {
  const ul = document.getElementById('rooms-list');
  if (!ul) return;
  ul.innerHTML = '';
  let list;
  try {
    const result = await mcpCall('list_occupancy', {});
    list = parseJson(result) || [];
  } catch (e) {
    ul.innerHTML = `<li class="error">list_occupancy failed: ${escapeHtml(e.message)}</li>`;
    return;
  }
  if (!Array.isArray(list) || list.length === 0) {
    ul.innerHTML = '<li class="muted">no active rooms</li>';
    return;
  }
  for (const room of list) {
    const li = document.createElement('li');
    const names = (room.agent_names || []).join(', ') || '(none)';
    li.innerHTML = `<code>${escapeHtml(room.path)}</code>: ${escapeHtml(names)}`;
    ul.appendChild(li);
  }
}

// ── Live SSE subscription (Task 9) ──────────────────────────────────────────

// PR 3 / Task 3.2 — "Only my session" toggle. Pure filter helper so the
// logic is independently testable without a DOM. An event passes when
// either (a) the toggle is off, (b) `myAgentId` is blank, or (c) the
// event's `agent_id` matches the bound session.
function filterConflictEvents(events, opts) {
  const onlyMySession = !!(opts && opts.onlyMySession);
  const myAgentId = (opts && typeof opts.myAgentId === 'string') ? opts.myAgentId.trim() : '';
  if (!onlyMySession || !myAgentId) return Array.isArray(events) ? events : [];
  return (Array.isArray(events) ? events : []).filter(e => e && e.agent_id === myAgentId);
}

function loadMySessionPrefs() {
  let onlyMySession = false;
  let myAgentId = '';
  try {
    onlyMySession = localStorage.getItem('lain_only_my_session') === '1';
    myAgentId = localStorage.getItem('lain_my_agent_id') || '';
  } catch (_) { /* localStorage unavailable — fall back to defaults */ }
  return { onlyMySession, myAgentId };
}

function saveMySessionPrefs(prefs) {
  try {
    localStorage.setItem('lain_only_my_session', prefs.onlyMySession ? '1' : '0');
    localStorage.setItem('lain_my_agent_id', prefs.myAgentId || '');
  } catch (_) { /* persist best-effort */ }
}

function wireOnlyMySessionToggle() {
  const toggle = document.getElementById('only-my-session-toggle');
  const idInput = document.getElementById('only-my-session-id');
  if (!toggle || !idInput) return;
  const prefs = loadMySessionPrefs();
  toggle.checked = prefs.onlyMySession;
  idInput.value = prefs.myAgentId;
  const onChange = () => {
    saveMySessionPrefs({ onlyMySession: toggle.checked, myAgentId: idInput.value });
    // PR 3 / Task 3.3 — the session filter is now applied at render time
    // against the in-memory conflict buffer, so just re-render.
    renderConflictsList();
  };
  toggle.addEventListener('change', onChange);
  idInput.addEventListener('input', onChange);
}

// ── PR 3 / Task 3.3 — burst collapsing ────────────────────────────────────
//
// Pure helper: groups `events` (objects with `{ts, path, ...}`) by `path` in
// chronological order. Two adjacent same-path events whose `ts` differ by at
// most `window_ms` are joined into one card. Runs of fewer than 3 items are
// expanded back into single-item cards so 1- or 2-event groups stay
// separate (per the brief).
//
// Output card shape: { path, count, first_ts, last_ts, items: [...] }
//
// --- Snapshot tests (brief) ---
// The repo has no JS test harness (verified — only npm-shim has *.test.js).
// These tests are documented inline; a future harness can pick them up as-is.
//
//   test('burst of 3 events same path within 5s collapses to one card', () => {
//     const base = Date.now();
//     const events = [
//       { ts: base,        path: '/x.rs' },
//       { ts: base + 1000, path: '/x.rs' },
//       { ts: base + 2000, path: '/x.rs' },
//     ];
//     const cards = collapseBursts(events, { window_ms: 5000 });
//     expect(cards).toHaveLength(1);
//     expect(cards[0].count).toBe(3);
//   });
//
//   test('events outside the window stay separate', () => {
//     const base = Date.now();
//     const events = [
//       { ts: base,        path: '/x.rs' },
//       { ts: base + 6000, path: '/x.rs' },
//     ];
//     const cards = collapseBursts(events, { window_ms: 5000 });
//     expect(cards).toHaveLength(2);
//   });
function collapseBursts(events, opts) {
  const window_ms = (opts && Number(opts.window_ms)) || 5000;
  const list = Array.isArray(events)
    ? events.filter(e => e && typeof e.path === 'string')
    : [];
  const sorted = list.slice().sort(
    (a, b) => (Number(a.ts) || 0) - (Number(b.ts) || 0)
  );
  const runs = [];
  for (const ev of sorted) {
    const ts = Number(ev.ts) || 0;
    const last = runs[runs.length - 1];
    if (
      last &&
      last.path === ev.path &&
      (ts - last.last_ts) <= window_ms
    ) {
      last.count += 1;
      last.last_ts = ts;
      last.items.push(ev);
    } else {
      runs.push({
        path: ev.path,
        count: 1,
        first_ts: ts,
        last_ts: ts,
        items: [ev],
      });
    }
  }
  // Collapse only runs of 3+; smaller runs stay as one-card-per-item so
  // the renderer doesn't need a "tiny burst" branch.
  const out = [];
  for (const run of runs) {
    if (run.count >= 3) {
      out.push(run);
    } else {
      for (const it of run.items) {
        out.push({
          path: it.path,
          count: 1,
          first_ts: Number(it.ts) || 0,
          last_ts: Number(it.ts) || 0,
          items: [it],
        });
      }
    }
  }
  return out;
}

// Buffer of recent conflict items + burst expansion state. Module-level so
// the SSE handler and the toggle both share the same view.
const CONFLICT_BUFFER_TTL_MS = 60000; // keep items 60s — comfortably longer
                                      // than the 5s collapse window even on
                                      // a slow event stream.
let conflictBuffer = []; // [{ ts, path, event: <raw conflict_detected payload> }]
const expandedBursts = new Set(); // keys: "<path>:<first_ts>"

function flattenConflictEvent(event, ts) {
  if (!event || !Array.isArray(event.conflicts)) return [];
  const t = Number(ts) || Date.now();
  const out = [];
  for (const c of event.conflicts) {
    if (!c || typeof c.path !== 'string') continue;
    out.push({ ts: t, path: c.path, event });
  }
  return out;
}

function pruneConflictBuffer() {
  const cutoff = Date.now() - CONFLICT_BUFFER_TTL_MS;
  while (conflictBuffer.length > 0 &&
         (Number(conflictBuffer[0].ts) || 0) < cutoff) {
    conflictBuffer.shift();
  }
}

function burstKey(card) {
  return `${card.path}:${card.first_ts}`;
}

function formatTs(ts) {
  const t = Number(ts) || 0;
  if (!t) return '?';
  try {
    return new Date(t).toLocaleTimeString();
  } catch (_) {
    return '?';
  }
}

function pickBurstSeverity(items) {
  const allowed = new Set(['none', 'low', 'medium', 'high']);
  const rank = { none: 0, low: 1, medium: 2, high: 3 };
  let best = 'none';
  for (const it of items) {
    const e = it.event || {};
    const s = allowed.has(e.severity) ? e.severity : 'none';
    if ((rank[s] || 0) > (rank[best] || 0)) best = s;
  }
  return best;
}

function renderConflictsList() {
  const list = document.getElementById('conflicts-list');
  if (!list) return;
  // PR 3 / Task 3.2 — re-apply session filter against the buffer at render
  // time so toggling the filter doesn't lose buffered events.
  const prefs = loadMySessionPrefs();
  const items = (!prefs.onlyMySession || !prefs.myAgentId)
    ? conflictBuffer.slice()
    : conflictBuffer.filter(
        i => i.event && i.event.agent_id === prefs.myAgentId
      );
  const cards = collapseBursts(items, { window_ms: 5000 });
  // Drop expansion keys that no longer correspond to a card in the buffer.
  const activeKeys = new Set(cards.map(burstKey));
  for (const k of Array.from(expandedBursts)) {
    if (!activeKeys.has(k)) expandedBursts.delete(k);
  }
  list.innerHTML = '';
  if (cards.length === 0) {
    list.innerHTML = '<li class="muted">no conflicts</li>';
    return;
  }
  for (const card of cards) {
    const li = document.createElement('li');
    li.className = 'conflict-card';
    const severity = pickBurstSeverity(card.items);
    const firstEvent = card.items[0].event || {};
    li.dataset.agentId = firstEvent.agent_id || '';
    if (card.count >= 3) {
      const key = burstKey(card);
      const expanded = expandedBursts.has(key);
      li.classList.add('burst');
      if (expanded) li.classList.add('expanded');
      const header = `
        <span class="severity severity-${severity}">${escapeHtml(severity)}</span>
        <strong>${escapeHtml(firstEvent.agent_id || 'unknown agent')}</strong>
        <code>${escapeHtml(card.path || 'unknown path')}</code>
        <span class="burst-count" title="events in this burst">×${card.count}</span>
        <span class="burst-window">${escapeHtml(formatTs(card.first_ts))} → ${escapeHtml(formatTs(card.last_ts))}</span>
        <button class="burst-toggle" data-key="${escapeHtml(key)}">${expanded ? 'hide' : 'show all'}</button>
      `;
      if (expanded) {
        const inner = card.items.map(it => {
          const e = it.event || {};
          const sev = (new Set(['none', 'low', 'medium', 'high']).has(e.severity))
            ? e.severity : 'none';
          return `<li class="burst-item">
            <span class="severity severity-${sev}">${escapeHtml(sev)}</span>
            <strong>${escapeHtml(e.agent_id || 'unknown')}</strong>
            <code>${escapeHtml(it.path)}</code>
            <span class="burst-item-ts">${escapeHtml(formatTs(it.ts))}</span>
          </li>`;
        }).join('');
        li.innerHTML = `${header}<ul class="burst-items">${inner}</ul>`;
      } else {
        li.innerHTML = header;
      }
    } else {
      // Single item — legacy card shape (matches pre-Task 3.3 layout).
      const item = card.items[0];
      const e = item.event || {};
      li.innerHTML = `
        <span class="severity severity-${severity}">${escapeHtml(severity)}</span>
        <strong>${escapeHtml(e.agent_id || 'unknown agent')}</strong>
        <code>${escapeHtml(item.path || 'unknown path')}</code>
      `;
    }
    list.appendChild(li);
  }
  // Wire show-all / hide buttons.
  list.querySelectorAll('.burst-toggle').forEach(btn => {
    btn.addEventListener('click', () => {
      const key = btn.dataset.key;
      if (!key) return;
      if (expandedBursts.has(key)) expandedBursts.delete(key);
      else expandedBursts.add(key);
      renderConflictsList();
    });
  });
}

function subscribePresenceEvents() {
  try {
    const ev = new EventSource('/events');
    const rerender = (which) => {
      if (which === 'agents' || which === 'both') renderAgentsOnline();
      if (which === 'rooms' || which === 'both') renderRooms();
    };
    ev.addEventListener('agent_joined', () => rerender('agents'));
    ev.addEventListener('agent_left', () => rerender('agents'));
    ev.addEventListener('heartbeat_expired', () => rerender('agents'));
    ev.addEventListener('claim_granted', () => rerender('both'));
    ev.addEventListener('claim_released', () => rerender('both'));
    ev.addEventListener('conflict_detected', (event) => {
      rerender('rooms');
      let conflict;
      try { conflict = JSON.parse(event.data); } catch (_) { return; }
      // PR 3 / Task 3.3 — flatten the conflict_detected payload into
      // per-path items, append to the rolling buffer, and re-render with
      // burst collapsing. Session filtering happens at render time against
      // the buffer so toggling the filter later still re-filters correctly.
      const items = flattenConflictEvent(conflict, Date.now());
      if (items.length === 0) return;
      conflictBuffer.push(...items);
      pruneConflictBuffer();
      renderConflictsList();
    });
    ev.addEventListener('ready', () => rerender('both'));
    ev.addEventListener('error', () => {
      // EventSource auto-reconnects on transient errors; log once so the
      // operator can see a dead stream in the JS console.
      console.warn('presence SSE connection error; EventSource will retry');
    });
  } catch (e) {
    console.warn('failed to open /events EventSource:', e.message);
  }
}

// ── Topbar: active project ─────────────────────────────────────────────────

async function renderActiveProject() {
  // The "active project" is the most-recently-used recent project.
  let list;
  try {
    const result = await mcpCall('list_recent_projects');
    list = parseJson(result) || [];
  } catch (_) {
    return;
  }
  if (Array.isArray(list) && list.length > 0) {
    document.getElementById('active-project').textContent =
      `project: ${list[0].path}`;
  }
}

// ── Status bar ─────────────────────────────────────────────────────────────

async function renderStatusBar() {
  let s;
  try {
    const result = await mcpCall('get_server_status');
    s = parseJson(result);
  } catch (e) {
    document.getElementById('status-pid').textContent = 'pid: error';
    return;
  }
  if (!s) return;
  document.getElementById('status-pid').textContent = `pid: ${s.pid ?? '?'}`;
  document.getElementById('status-transport').textContent =
    `transport: ${s.transport ?? '?'}`;
  document.getElementById('status-repos').textContent =
    `repos: ${s.repo_count ?? '?'} / ws: ${s.workspace_count ?? '?'}`;
  const last = s.last_sync_at ? new Date(s.last_sync_at * 1000).toLocaleTimeString() : 'n/a';
  document.getElementById('status-reload').textContent = `last sync: ${last}`;
  return s;
}

// ── Tab: overview ──────────────────────────────────────────────────────────

async function renderOverviewTab() {
  const tab = document.getElementById('tab-overview');
  let health, status;
  try {
    const h = await mcpCall('get_federation_health');
    health = parseJson(h);
  } catch (_) { health = null; }
  try {
    const s = await mcpCall('get_health');
    status = parseJson(s);
  } catch (_) { status = null; }
  const lines = [];
  if (status) {
    lines.push(`<h3>Server health</h3>`);
    lines.push(`<pre>${escapeHtml(JSON.stringify(status, null, 2))}</pre>`);
  }
  if (health) {
    lines.push(`<h3>Federation health</h3>`);
    lines.push(`<pre>${escapeHtml(JSON.stringify(health, null, 2))}</pre>`);
  }
  if (lines.length === 0) {
    lines.push('<p class="muted">No health data available.</p>');
  }
  tab.innerHTML = lines.join('');
}

// ── Tab: repos ─────────────────────────────────────────────────────────────

async function renderReposTab() {
  const tab = document.getElementById('tab-repos');
  tab.innerHTML = '<p class="muted">Loading…</p>';
  let list;
  try {
    const result = await mcpCall('list_repos');
    list = parseJson(result) || [];
  } catch (e) {
    tab.innerHTML = `<p class="error">list_repos failed: ${escapeHtml(e.message)}</p>`;
    return;
  }
  if (!Array.isArray(list) || list.length === 0) {
    tab.innerHTML = '<p class="muted">No repos registered.</p>';
    return;
  }
  const rows = [];
  for (const repo of list) {
    let info = repo;
    try {
      const r = await mcpCall('get_repo_info', {id: repo.id});
      info = parseJson(r) || repo;
    } catch (_) { /* fall back to the row from list_repos */ }
    rows.push(`<tr>
      <td><code>${escapeHtml(info.id)}</code></td>
      <td><code>${escapeHtml(info.path)}</code></td>
      <td>${escapeHtml(info.health || '?')}</td>
      <td>${info.node_count ?? '?'}</td>
      <td>${info.edge_count ?? '?'}</td>
    </tr>`);
  }
  tab.innerHTML = `
    <table class="repo-table">
      <thead><tr><th>id</th><th>path</th><th>health</th><th>nodes</th><th>edges</th></tr></thead>
      <tbody>${rows.join('')}</tbody>
    </table>
  `;
}

// ── Tab: query ─────────────────────────────────────────────────────────────

async function renderQueryTab() {
  const tab = document.getElementById('tab-query');
  tab.innerHTML = `
    <div class="query-form">
      <label>repo_id
        <input id="query-repo" placeholder="alpha" list="repo-list">
        <datalist id="repo-list"></datalist>
      </label>
      <label>op
        <select id="query-op">
          <option value="find">find</option>
        </select>
      </label>
      <label>type
        <input id="query-type" placeholder="Function" value="Function">
      </label>
      <label>limit
        <input id="query-limit" type="number" value="10" min="1" max="1000">
      </label>
      <button id="query-run">Run</button>
    </div>
    <pre id="query-output" class="output">…</pre>
  `;
  // Populate the repo datalist.
  try {
    const result = await mcpCall('list_repos');
    const list = parseJson(result) || [];
    const dl = tab.querySelector('#repo-list');
    for (const r of list) {
      const opt = document.createElement('option');
      opt.value = r.id;
      dl.appendChild(opt);
    }
  } catch (_) { /* leave empty */ }

  tab.querySelector('#query-run').addEventListener('click', async () => {
    const repo = tab.querySelector('#query-repo').value.trim();
    const type = tab.querySelector('#query-type').value.trim();
    const limit = parseInt(tab.querySelector('#query-limit').value, 10) || 10;
    const args = {
      query: {
        ops: [{
          op: 'find',
          type,
          limit,
        }],
      },
    };
    if (repo) args.repo_id = repo;
    const out = tab.querySelector('#query-output');
    out.textContent = 'running…';
    try {
      const result = await mcpCall('query_graph', args);
      out.textContent = unwrapText(result) || JSON.stringify(result, null, 2);
    } catch (e) {
      out.textContent = 'error: ' + e.message;
    }
  });
}

// ── Tab: tools (MCP tool tester) ───────────────────────────────────────────

async function renderToolsTab() {
  const tab = document.getElementById('tab-tools');
  tab.innerHTML = `
    <div class="tools-layout">
      <ul id="tools-list" class="tools-list"></ul>
      <div id="tool-form" class="tool-form"><p class="muted">Pick a tool on the left.</p></div>
    </div>
  `;
  let list;
  try {
    const r = await fetch('/mcp', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({jsonrpc: '2.0', method: 'tools/list', params: {}, id: 1}),
    });
    const body = await r.json();
    list = (body.result && body.result.tools) || [];
    if (!Array.isArray(list)) {
      tab.querySelector('#tool-form').textContent = 'tools/list returned no array';
      return;
    }
  } catch (e) {
    tab.querySelector('#tool-form').textContent = 'tools/list failed: ' + e.message;
    return;
  }
  const ul = tab.querySelector('#tools-list');
  for (const tool of list) {
    const li = document.createElement('li');
    li.innerHTML = `
      <button data-name="${escapeHtml(tool.name)}">${escapeHtml(tool.name)}</button>
      <span class="muted">${escapeHtml(tool.description || '')}</span>
    `;
    li.querySelector('button').addEventListener('click', () => {
      document.querySelectorAll('#tools-list .active').forEach(el => el.classList.remove('active'));
      li.classList.add('active');
      renderToolForm(tool);
    });
    ul.appendChild(li);
  }
}

function renderToolForm(tool) {
  const container = document.getElementById('tool-form');
  const schema = tool.inputSchema || {type: 'object', properties: {}};
  const props = (schema.properties) || {};
  const required = Array.isArray(schema.required) ? schema.required : [];
  const fields = Object.entries(props).map(([k, v]) => {
    const desc = (v && v.description) || '';
    const type = (v && v.type) || 'string';
    if (type === 'integer' || type === 'number') {
      return `<label>${escapeHtml(k)}${required.includes(k) ? ' *' : ''}
        <input type="number" name="${escapeHtml(k)}" placeholder="${escapeHtml(desc)}">
      </label>`;
    }
    if (type === 'boolean') {
      return `<label>${escapeHtml(k)}${required.includes(k) ? ' *' : ''}
        <select name="${escapeHtml(k)}">
          <option value=""></option>
          <option value="true">true</option>
          <option value="false">false</option>
        </select>
      </label>`;
    }
    return `<label>${escapeHtml(k)}${required.includes(k) ? ' *' : ''}
      <input name="${escapeHtml(k)}" placeholder="${escapeHtml(desc)}">
    </label>`;
  }).join('');
  container.innerHTML = `
    <h3>${escapeHtml(tool.name)}</h3>
    <p class="muted">${escapeHtml(tool.description || '')}</p>
    <form id="tool-args">${fields || '<p class="muted">no arguments</p>'}</form>
    <div class="tool-buttons">
      <button id="tool-call">Call</button>
      <button id="tool-curl">Copy as cURL</button>
    </div>
    <pre id="tool-result" class="output">…</pre>
  `;
  container.querySelector('#tool-call').addEventListener('click', async () => {
    const args = {};
    container.querySelectorAll('#tool-args input, #tool-args select').forEach(i => {
      const v = i.value;
      if (v === '') return;
      if (i.type === 'number') {
        const n = Number(v);
        if (!Number.isNaN(n)) args[i.name] = n;
      } else if (v === 'true' || v === 'false') {
        args[i.name] = (v === 'true');
      } else {
        args[i.name] = v;
      }
    });
    const out = container.querySelector('#tool-result');
    out.textContent = 'calling…';
    try {
      const result = await mcpCall(tool.name, args);
      const text = unwrapText(result);
      out.textContent = text != null ? text : JSON.stringify(result, null, 2);
    } catch (e) {
      out.textContent = 'error: ' + e.message;
    }
  });
  container.querySelector('#tool-curl').addEventListener('click', () => {
    const args = {};
    container.querySelectorAll('#tool-args input, #tool-args select').forEach(i => {
      if (i.value !== '') args[i.name] = i.value;
    });
    const payload = {jsonrpc: '2.0', method: 'tools/call', params: {name: tool.name, arguments: args}, id: 1};
    const curl = `curl -s -X POST http://localhost:9999/mcp \\\n  -H 'Content-Type: application/json' \\\n  -d '${JSON.stringify(payload).replace(/'/g, "'\\''")}'`;
    navigator.clipboard.writeText(curl).catch(() => {});
    container.querySelector('#tool-curl').textContent = 'Copied!';
    setTimeout(() => { container.querySelector('#tool-curl').textContent = 'Copy as cURL'; }, 1500);
  });
}

// ── Boot ───────────────────────────────────────────────────────────────────

async function init() {
  // Tab switching (idempotent — re-runs the tab's render fn on each switch
  // so the data is fresh).
  document.querySelectorAll('[data-tab]').forEach(btn => {
    btn.addEventListener('click', () => {
      const name = btn.dataset.tab;
      document.querySelectorAll('.tab').forEach(t => { t.style.display = 'none'; });
      const target = document.getElementById('tab-' + name);
      if (target) target.style.display = 'block';
      const fn = window['render' + name.charAt(0).toUpperCase() + name.slice(1) + 'Tab'];
      if (typeof fn === 'function') fn();
    });
  });

  // Show the first tab by default.
  const first = document.querySelector('.tab');
  if (first) first.style.display = 'block';

  // Sidebar / topbar (best-effort; one failure shouldn't block the rest).
  await Promise.allSettled([
    renderWorkspaces(),
    renderReposSidebar(),
    renderRecentProjects(),
    renderActiveProject(),
    renderStatusBar(),
    renderOverviewTab(),
    renderAgentsOnline(),
    renderRooms(),
  ]);

  // Live presence: open the EventSource once the initial render has run so
  // the panels have data before the first event redraws them.
  subscribePresenceEvents();

  // PR 3 / Task 3.2 — restore the "Only my session" toggle from
  // localStorage and keep existing conflict cards in sync when it flips.
  wireOnlyMySessionToggle();

  // Status bar poll.
  setInterval(renderStatusBar, 2000);
}

init();
