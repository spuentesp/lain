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
    // `list_workspaces` is only registered when a workspaces file is
    // loaded, so a federation with no `workspaces.yaml` — the ordinary
    // setup — gets "Unknown tool" here. That is a configuration state,
    // not a failure, and painting it red made a healthy dashboard look
    // broken.
    const msg = unwrapText(result) || 'error';
    if (/unknown tool|no workspaces file/i.test(msg)) {
      ul.innerHTML = '<li class="muted">no workspaces configured</li>';
      return;
    }
    ul.innerHTML = `<li class="error">${escapeHtml(msg)}</li>`;
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
      const r = await mcpCall('get_repo_info', {repo_id: repo.id});
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

// ── Tab: graph (D-M8) ──────────────────────────────────────────────────────
//
// The server holds exactly one workspace — the one it was started with
// (`lain server --workspace <name>`), and `get_workspace_graph` derives that
// workspace from the loaded repo set rather than taking a name. So this
// picker is a client-side affordance over a server-side fact. See
// docs/opinions/graph-tab-data-source.md.

// Pure selection rule, unit-tested in tests/js/graph_tab.test.js.
//   'none'   — nothing indexed; show the "no workspace indexed yet" message.
//   'auto'   — render this workspace immediately (single-repo mode, or the
//              server told us which one is active).
//   'picker' — several workspaces and no active one; make the operator choose.
function pickWorkspaceForGraph(list) {
  const named = Array.isArray(list)
    ? list.filter(ws => ws && typeof ws.name === 'string' && ws.name !== '')
    : [];
  if (named.length === 0) return { mode: 'none', workspace: null };
  const active = named.find(ws => ws.is_active === true);
  if (active) return { mode: 'auto', workspace: active.name };
  if (named.length === 1) return { mode: 'auto', workspace: named[0].name };
  return { mode: 'picker', workspace: null };
}

// Module-level so the picker's change handler and renderGraphTab() agree on
// which workspace is on screen. `null` means "nothing selected yet".
let selectedGraphWorkspace = null;

// Fill the picker from a list_workspaces payload, marking the current
// selection. Hides the picker entirely when there is at most one workspace —
// there is nothing to choose (approach (a): single-repo mode pre-picks).
function populateGraphPicker(list, selected) {
  const select = document.getElementById('graph-workspace');
  const header = document.querySelector('#tab-graph .graph-header');
  if (!select) return;
  const named = Array.isArray(list)
    ? list.filter(ws => ws && typeof ws.name === 'string' && ws.name !== '')
    : [];
  select.innerHTML = '';
  if (named.length <= 1) {
    if (header) header.classList.add('no-picker');
    return;
  }
  if (header) header.classList.remove('no-picker');
  if (!selected) {
    const blank = document.createElement('option');
    blank.value = '';
    blank.textContent = '— pick a workspace —';
    select.appendChild(blank);
  }
  for (const ws of named) {
    const opt = document.createElement('option');
    opt.value = ws.name;
    const count = (ws.member_count == null) ? '?' : ws.member_count;
    opt.textContent = `${ws.name} (${count} repos)${ws.is_active ? ' — active' : ''}`;
    if (ws.name === selected) opt.selected = true;
    select.appendChild(opt);
  }
}

// Paint the tab when there is no graph to draw. `state` is the {mode, ...}
// object from pickWorkspaceForGraph, or a synthetic {mode:'error', message}
// / {mode:'not-loaded', workspace} for the two runtime cases.
function renderGraphTabEmpty(state, list) {
  const empty = document.getElementById('graph-empty');
  const meta = document.getElementById('graph-meta');
  const svg = document.getElementById('graph-canvas');
  if (svg) svg.innerHTML = '';
  if (meta) meta.textContent = '';
  if (!empty) return;
  populateGraphPicker(list, (state && state.workspace) || null);
  const mode = (state && state.mode) || 'none';
  if (mode === 'none') {
    empty.className = 'muted';
    empty.textContent = 'No workspace indexed yet. Start the server with ' +
      '`lain server --config <repos.yaml> --workspace <name>`.';
    return;
  }
  if (mode === 'picker') {
    empty.className = 'muted';
    empty.textContent = 'Pick a workspace above to draw its graph.';
    return;
  }
  if (mode === 'not-loaded') {
    // The server holds one workspace at a time; we cannot draw a workspace it
    // never loaded. Hand the operator the exact restart line instead.
    empty.className = 'muted';
    empty.innerHTML =
      `Workspace <code>${escapeHtml(state.workspace || '')}</code> is not loaded by this ` +
      `server. Restart it with ` +
      `<code>lain server --config &lt;repos.yaml&gt; --workspace ` +
      `${escapeHtml(state.workspace || '')}</code>.`;
    return;
  }
  empty.className = 'error';
  empty.textContent = (state && state.message) || 'graph unavailable';
}

// `list_workspaces` is only registered when the server loaded a
// workspaces.yaml, so a plain federation answers "Unknown tool". That is a
// configuration state, not a failure — same reasoning (and same regex) as
// renderWorkspaces() above. Pure, so tests/js/graph_tab.test.js can cover it.
function classifyWorkspacesResult(result, parsed) {
  if (result && result.isError) {
    const msg = unwrapText(result) || 'error';
    if (/unknown tool|no workspaces file/i.test(msg)) {
      return { ok: true, list: [], message: null, configless: true };
    }
    return { ok: false, list: [], message: msg, configless: false };
  }
  const list = Array.isArray(parsed) ? parsed : [];
  return { ok: true, list, message: null, configless: false };
}

// Fetch the workspace list and decide what the tab should show. Commits an
// 'auto' verdict to `selectedGraphWorkspace` so single-repo mode needs no
// operator interaction at all (defect D-M8, fix (a)).
async function loadGraphWorkspaces() {
  let classified;
  try {
    const result = await mcpCall('list_workspaces');
    classified = classifyWorkspacesResult(result, parseJson(result));
  } catch (e) {
    return {
      state: { mode: 'error', message: `list_workspaces failed: ${e.message}` },
      list: [],
    };
  }
  if (!classified.ok) {
    return { state: { mode: 'error', message: classified.message }, list: [] };
  }
  const state = pickWorkspaceForGraph(classified.list);
  if (state.mode === 'auto') {
    selectedGraphWorkspace = state.workspace;
  }
  return { state, list: classified.list };
}

// Defensive reshaping of a get_workspace_graph payload. d3.forceLink() throws
// on an edge whose endpoint id is not in the node array, and the server caps
// nodes at 5000 before it caps edges at 10000 — so a truncated response can
// legitimately carry dangling edges. Pure; covered by tests/js/graph_tab.test.js.
function normalizeGraphPayload(payload) {
  const rawNodes = (payload && Array.isArray(payload.nodes)) ? payload.nodes : [];
  const rawEdges = (payload && Array.isArray(payload.edges)) ? payload.edges : [];
  const nodes = rawNodes
    .filter(n => n && typeof n.id === 'string' && n.id !== '')
    .map(n => ({
      id: n.id,
      name: typeof n.name === 'string' ? n.name : n.id,
      path: typeof n.path === 'string' ? n.path : '',
      repo_id: typeof n.repo_id === 'string' ? n.repo_id : '',
      kind: typeof n.kind === 'string' ? n.kind : '',
    }));
  const ids = new Set(nodes.map(n => n.id));
  const edges = rawEdges
    .filter(e => e && ids.has(e.source) && ids.has(e.target))
    .map(e => ({
      source: e.source,
      target: e.target,
      edge_type: typeof e.edge_type === 'string' ? e.edge_type : '',
      cross_repo: e.cross_repo === true,
    }));
  return { nodes, edges, truncated: !!(payload && payload.truncated) };
}

// Pure helpers — DOM-free so node --test can import them. Used by
// drawGraphSvg and tested in tests/js/graph_tab.test.js.

const REPO_PALETTE = ['graph-repo-a', 'graph-repo-b', 'graph-repo-c', 'graph-repo-d', 'graph-repo-e'];

function computeRepoPalette(graph) {
  // Stable mapping repo_id → palette index. Repos are sorted
  // alphabetically so the same repo gets the same colour across re-renders.
  const ids = Array.from(new Set(
    (graph.nodes || []).map(n => n.repo_id).filter(Boolean)
  )).sort();
  const out = new Map();
  ids.forEach((id, i) => out.set(id, REPO_PALETTE[i] || 'graph-repo-fallback'));
  return out;
}

function repoColour(repoId, palette) {
  if (!repoId) return 'graph-repo-fallback';
  return palette.get(repoId) || 'graph-repo-fallback';
}

function nodeShape(kind) {
  if (kind === 'Method') return 'diamond';
  if (kind === 'Class')  return 'square';
  return 'circle';
}

function nodeRadius(role) {
  return role === 'focus' ? 7 : role === 'neighbour' ? 6 : 5;
}

function applyFilters(graph, state) {
  const visibleNodes = [];
  const hiddenNodeIds = new Set();
  const acceptedRepos = state.repos;
  const acceptedKinds = state.kinds;
  for (const n of graph.nodes) {
    const repoOk = acceptedRepos.has(n.repo_id);
    const kindOk = acceptedKinds.has(n.kind);
    if (!repoOk || !kindOk) {
      hiddenNodeIds.add(n.id);
    } else {
      visibleNodes.push(n);
    }
  }
  if (state.crossRepoOnly) {
    const touchingCross = new Set();
    for (const e of graph.edges) {
      if (e.cross_repo) {
        touchingCross.add(e.source);
        touchingCross.add(e.target);
      }
    }
    for (const n of graph.nodes) {
      if (!touchingCross.has(n.id)) hiddenNodeIds.add(n.id);
    }
  }
  const visibleEdges = [];
  const hiddenEdgeIds = new Set();
  for (const e of graph.edges) {
    const sHidden = hiddenNodeIds.has(e.source);
    const tHidden = hiddenNodeIds.has(e.target);
    if (sHidden || tHidden) {
      hiddenEdgeIds.add(e);
    } else {
      visibleEdges.push(e);
    }
  }
  // The crossRepoOnly pass above only adds to hiddenNodeIds; drop those from
  // visibleNodes here so the two lists stay in sync (applyFilters callers
  // treat visibleNodes as the post-filter set).
  const visibleNodesFiltered = visibleNodes.filter(n => !hiddenNodeIds.has(n.id));
  return { visibleNodes: visibleNodesFiltered, visibleEdges, hiddenNodeIds, hiddenEdgeIds };
}

// ── Graph tab: filter bar, minimap, legend helpers (Task 5, 2026-08-31) ──
//
// paintLegend / buildFilterBar / applyFiltersToDom / paintMinimap / wireZoom
// are split out of drawGraphSvg so each step of the upgrade is testable in
// isolation. paintLegend paints a static grid keyed on (repo × kind);
// buildFilterBar builds the chip rows and wires their click handlers;
// applyFiltersToDom toggles `.is-hidden` on the existing nodes / links
// without rebuilding them; paintMinimap paints dots + the viewport frame;
// wireZoom attaches a d3.zoom behaviour to the SVG and applies the
// transform to the viewport <g>.

// Repo × kind → cell. Each cell gets a `<span>` whose class drives the
// per-repo CSS variable (currentColor → fill on the inner <path>). `palette`
// already contains `graph-repo-*` class strings; do not double-prefix.
function paintLegend(graph, palette, container) {
  if (!container) return;
  container.innerHTML = '';
  // Stable set of repos + kinds for the grid axes.
  const repos = Array.from(new Set(graph.nodes.map(n => n.repo_id).filter(Boolean))).sort();
  const kinds = ['Function', 'Method', 'Class'];

  for (const repo of repos) {
    for (const kind of kinds) {
      const hasData = graph.nodes.some(n => n.repo_id === repo && n.kind === kind);
      const cell = document.createElement('div');
      cell.className = 'graph-legend-cell' + (hasData ? '' : ' is-empty');
      const repoCls = palette.get(repo) || 'graph-repo-fallback';
      cell.innerHTML = `
        <span class="${repoCls}">
          <svg viewBox="-10 -10 20 20" aria-hidden="true">
            <path d="${d3.symbol().size(64).type(d3[nodeShape(kind)])()}"/>
          </svg>
        </span>
        <span class="graph-legend-name">${escapeHtml(repo)} · ${escapeHtml(kind)}</span>
      `;
      container.appendChild(cell);
    }
  }
}

// Three rows of chips (repos, kinds, view toggles) + a reset-zoom button.
// Each row's container is pre-stamped by Task 3's HTML; we only inject the
// chips and their click handlers.
function buildFilterBar(graph, palette, state, container, onChange) {
  if (!container) return;
  container.innerHTML = '';
  const repos = Array.from(new Set(graph.nodes.map(n => n.repo_id).filter(Boolean))).sort();
  const kinds = ['Function', 'Method', 'Class'];
  const make = (row, label, content, after) => {
    row.innerHTML = `
      <span class="graph-filter-label muted">${escapeHtml(label)}</span>
      ${content}
      ${after ? `<span class="graph-filter-after">${after}</span>` : ''}
    `;
  };
  const wrap = (label, items) => items.map(({key, text, on}) => `
    <button class="graph-chip ${on ? 'is-on' : ''}" data-filter="${escapeHtml(key)}">
      ${text}
    </button>`).join('');
  const reprows = container.querySelector('[data-filter-row="repos"]');
  make(reprows, 'repos', wrap('repos', repos.map(r => {
    const swatchCls = palette.get(r) || 'graph-repo-fallback';
    return {
      key: `repo:${r}`,
      text: `<span class="graph-chip-swatch ${swatchCls}"></span>${escapeHtml(r)}`,
      on: state.repos.has(r),
    };
  })));

  const kindrows = container.querySelector('[data-filter-row="kinds"]');
  make(kindrows, 'kind', wrap('kinds', kinds.map(k => ({
    key: `kind:${k}`,
    text: `${escapeHtml(k)} <span class="muted">(circle|diamond|square)</span>`,
    on: state.kinds.has(k),
  }))));

  const togglerow = container.querySelector('[data-filter-row="toggles"]');
  make(togglerow, 'view', wrap('toggles', [
    { key: 'cross-repo-only', text: 'cross-repo only', on: state.crossRepoOnly },
    { key: 'labels',          text: 'labels always',   on: state.labelsAlwaysOn },
  ]) + `<button class="graph-chip" data-zoom-reset>reset zoom</button>`);

  container.querySelectorAll('[data-filter]').forEach(btn => {
    btn.addEventListener('click', () => {
      const k = btn.dataset.filter;
      if (k.startsWith('repo:')) {
        const repo = k.slice(5);
        if (state.repos.has(repo)) state.repos.delete(repo);
        else state.repos.add(repo);
      } else if (k.startsWith('kind:')) {
        const kind = k.slice(5);
        if (state.kinds.has(kind)) state.kinds.delete(kind);
        else state.kinds.add(kind);
      } else if (k === 'cross-repo-only') {
        state.crossRepoOnly = !state.crossRepoOnly;
      } else if (k === 'labels') {
        state.labelsAlwaysOn = !state.labelsAlwaysOn;
      }
      btn.classList.toggle('is-on');
      onChange();
    });
  });

  const reset = container.querySelector('[data-zoom-reset]');
  if (reset) reset.addEventListener('click', () => onChange({ resetZoom: true }));
}

// Mark nodes + links hidden according to `state`. Single pass per
// element type, single `.is-hidden` toggle per line. The link key is the
// canonical (min,max) edge id used by drawGraphSvg at line-join time, so
// the Set membership test is O(1).
function applyFiltersToDom(svgEl, graph, state) {
  const computed = applyFilters(graph, state);
  const hiddenNodes = computed.hiddenNodeIds;
  const visibleEdgeKeys = new Set(computed.visibleEdges.map(e =>
    e.source < e.target ? `${e.source}|${e.target}` : `${e.target}|${e.source}`
  ));
  svgEl.querySelectorAll('.graph-node').forEach(p => {
    p.classList.toggle('is-hidden', hiddenNodes.has(p.dataset.nodeId));
  });
  svgEl.querySelectorAll('.graph-link').forEach(line => {
    line.classList.toggle('is-hidden', !visibleEdgeKeys.has(line.dataset.edgeKey));
  });
  // Labels follow the same threshold as drawGraphSvg's initial render.
  const labelHide = !state.labelsAlwaysOn && graph.nodes.length > 150;
  svgEl.querySelectorAll('.graph-label').forEach(text => {
    text.style.display = labelHide ? 'none' : null;
  });
}

// Paint the minimap: one dot per visible node, plus a rectangle marking the
// viewport's position in the graph. The transform math assumes the SVG
// viewport <g> receives `translate(tx, ty) scale(s)` from d3.zoom.
function paintMinimap(graph, minimapEl, viewportTransform, filterState) {
  if (!minimapEl) return;
  const w = minimapEl.clientWidth || 150;
  const h = minimapEl.clientHeight || 100;
  minimapEl.setAttribute('viewBox', `0 0 ${w} ${h}`);
  minimapEl.innerHTML = '';
  if (!graph.nodes.length) return;
  const bounds = graph.nodes.reduce((acc, n) => ({
    xmin: Math.min(acc.xmin, n.x ?? acc.xmin), xmax: Math.max(acc.xmax, n.x ?? acc.xmax),
    ymin: Math.min(acc.ymin, n.y ?? acc.ymin), ymax: Math.max(acc.ymax, n.y ?? acc.ymax),
  }), { xmin: Infinity, xmax: -Infinity, ymin: Infinity, ymax: -Infinity });
  const dx = bounds.xmax - bounds.xmin || 1;
  const dy = bounds.ymax - bounds.ymin || 1;
  const sx = (w - 6) / dx, sy = (h - 6) / dy, s = Math.min(sx, sy);
  const tx = (w - s * (bounds.xmin + bounds.xmax)) / 2;
  const ty = (h - s * (bounds.ymin + bounds.ymax)) / 2;
  const computed = applyFilters(graph, filterState);
  const visible = new Set(computed.visibleNodes.map(n => n.id));
  for (const n of graph.nodes) {
    if (!visible.has(n.id)) continue;
    if (typeof n.x !== 'number' || typeof n.y !== 'number') continue;
    const dot = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
    dot.setAttribute('cx', String(n.x * s + tx));
    dot.setAttribute('cy', String(n.y * s + ty));
    dot.setAttribute('r', '1');
    dot.setAttribute('fill', 'rgba(255,255,255,0.6)');
    minimapEl.appendChild(dot);
  }
  // Frame: convert from graph coordinates (the area visible at the current
  // zoom) to minimap coordinates (graph * s + t). The `-x/scale` term gives
  // the graph-coord top-left of the visible window; multiplying by s and
  // adding the minimap's translation puts it in minimap-coord space.
  if (viewportTransform && viewportTransform.scale) {
    const rect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
    rect.setAttribute('class', 'graph-minimap-frame');
    const cw = Number(minimapEl.dataset.canvasW) || 0;
    const ch = Number(minimapEl.dataset.canvasH) || 0;
    const k = viewportTransform.scale;
    const vw = cw / k;
    const vh = ch / k;
    const vx = (-viewportTransform.x) / k * s + tx;
    const vy = (-viewportTransform.y) / k * s + ty;
    rect.setAttribute('x', String(vx));
    rect.setAttribute('y', String(vy));
    rect.setAttribute('width', String(vw * s));
    rect.setAttribute('height', String(vh * s));
    minimapEl.appendChild(rect);
  }
}

// Attach d3.zoom to the SVG (not its parent chain — wheel events must land
// on the SVG) and route the transform to `svgViewport` (a d3 selection of
// the <g class="graph-viewport">). `callback` fires after every zoom/pan
// event so the caller can repaint the minimap frame.
function wireZoom(svgEl, svgViewport, zoomState, callback) {
  if (!svgEl || !svgViewport || typeof d3.zoom !== 'function') return;
  const viewportSel = (typeof svgViewport.node === 'function') ? svgViewport : d3.select(svgViewport);
  const z = d3.zoom().scaleExtent([0.2, 8]).on('zoom', (event) => {
    const t = event.transform;
    viewportSel.attr('transform', `translate(${t.x},${t.y}) scale(${t.k})`);
    zoomState.transform = { x: t.x, y: t.y, k: t.k, scale: t.k };
    if (callback) callback();
  });
  d3.select(svgEl).call(z);
  zoomState.api = z;
}

// Paint a force-directed graph into `svgEl`. Same simulation idiom as
// src/ui/blast-radius.html; colours and shapes come from styles.css classes
// so the drawing follows the phosphor/paper theme without any JS literals.
// Two-axis visual encoding: shape (per kind via D3 symbols) × colour (per
// repo via CSS variables set by `.graph-repo-*`). Hover-focus dims non-
// neighbours; filters toggle `.is-hidden` without rebuilding the DOM.
function drawGraphSvg(svgEl, graph) {
  if (!svgEl || typeof d3 === 'undefined') return;
  svgEl.innerHTML = '';
  const width = svgEl.clientWidth || 800;
  const height = svgEl.clientHeight || 500;
  svgEl.setAttribute('viewBox', `0 0 ${width} ${height}`);
  svgEl.setAttribute('preserveAspectRatio', 'xMidYMid meet');

  const palette = computeRepoPalette(graph);
  const state = {
    repos: new Set(graph.nodes.map(n => n.repo_id).filter(Boolean)),
    kinds: new Set(['Function', 'Method', 'Class']),
    crossRepoOnly: false,
    labelsAlwaysOn: false,
  };

  const container = svgEl.closest('.graph-canvas-wrap') || svgEl.parentElement;
  const viewport = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  viewport.classList.add('graph-viewport');
  svgEl.appendChild(viewport);

  // forceLink mutates the edge objects (replacing ids with node refs), so
  // hand it copies — renderGraphTab may redraw from the same payload.
  const nodes = graph.nodes.map(n => Object.assign({}, n));
  const links = graph.edges.map(e => Object.assign({}, e));

  const simulation = d3.forceSimulation(nodes)
    .force('link', d3.forceLink(links).id(d => d.id).distance(60))
    .force('charge', d3.forceManyBody().strength(-160))
    .force('center', d3.forceCenter(width / 2, height / 2));

  // Edges as <line>.
  const link = viewport.append('g')
    .selectAll('line')
    .data(links)
    .join('line')
    .attr('class', d => 'graph-link' + (d.cross_repo ? ' cross-repo' : ''))
    .attr('data-edge-key', d => d.source < d.target ? `${d.source}|${d.target}` : `${d.target}|${d.source}`);

  // Nodes as <path> via D3 symbols. Two visual axes: shape per kind, colour per repo.
  const node = viewport.append('g')
    .selectAll('path')
    .data(nodes)
    .join('path')
    .attr('class', d => {
      const cls = ['graph-node', `graph-node--kind-${d.kind}`];
      cls.push(repoColour(d.repo_id, palette));
      return cls.join(' ');
    })
    .attr('data-node-id', d => d.id)
    .attr('d', d => d3.symbol().size(64).type(d3[nodeShape(d.kind)])())
    .call(d3.drag()
      .on('start', (event, d) => {
        if (!event.active) simulation.alphaTarget(0.3).restart();
        d.fx = d.x; d.fy = d.y;
      })
      .on('drag', (event, d) => { d.fx = event.x; d.fy = event.y; })
      .on('end', (event, d) => {
        if (!event.active) simulation.alphaTarget(0);
        d.fx = null; d.fy = null;
      }));

  // Precompute neighbours for hover focus.
  const neighboursById = new Map();
  for (const e of graph.edges) {
    if (!neighboursById.has(e.source)) neighboursById.set(e.source, new Set());
    if (!neighboursById.has(e.target)) neighboursById.set(e.target, new Set());
    neighboursById.get(e.source).add(e.target);
    neighboursById.get(e.target).add(e.source);
  }

  // Tooltip — styled <g class="graph-tooltip"> following the cursor. Updated by
  // mouseover / mousemove and cleared by mouseout.
  const tooltipGroup = viewport.append('g').attr('class', 'graph-tooltip').style('display', 'none');
  const tooltipBg = tooltipGroup.append('rect').attr('class', 'graph-tooltip-bg');
  const tooltipText = tooltipGroup.append('text').attr('class', 'graph-tooltip');
  const updateTooltip = (d, evt) => {
    if (!d) { tooltipGroup.style('display', 'none'); return; }
    const deg = neighboursById.get(d.id)?.size ?? 0;
    const text = `${d.name}\n${d.repo_id} · ${d.kind}\n${d.path}\ndegree: ${deg}`;
    tooltipText.selectAll('tspan').remove();
    text.split('\n').forEach((line, i) => {
      tooltipText.append('tspan').attr('x', 8).attr('dy', i === 0 ? 12 : 14).text(line);
    });
    const lines = text.split('\n');
    const longest = lines.reduce((a, b) => b.length > a.length ? b : a, '');
    tooltipBg.attr('width', String(8 + longest.length * 6.5)).attr('height', String(2 + lines.length * 14));
    tooltipGroup.attr('transform', `translate(${evt.offsetX + 12}, ${evt.offsetY + 12})`).style('display', null);
  };

  // Hover focus handlers.
  node
    .on('mouseover', (event, d) => {
      node.classed('is-dim', n => n.id !== d.id && !(neighboursById.get(d.id)?.has(n.id)));
      node.classed('is-focus', n => n.id === d.id);
      node.classed('is-neighbour', n => neighboursById.get(d.id)?.has(n.id));
      link.classed('is-dim', e => {
        const sId = (typeof e.source === 'object') ? e.source.id : e.source;
        const tId = (typeof e.target === 'object') ? e.target.id : e.target;
        return sId !== d.id && tId !== d.id;
      });
      updateTooltip(d, event);
    })
    .on('mousemove', (event, d) => updateTooltip(d, event))
    .on('mouseout', () => {
      node.classed('is-focus', false).classed('is-neighbour', false).classed('is-dim', false);
      link.classed('is-dim', false);
      tooltipGroup.style('display', 'none');
    });

  // Labels — rendered only when the count is small or the toggle is on.
  const labelGroup = viewport.append('g').attr('class', 'graph-labels');
  const updateLabels = () => {
    const show = state.labelsAlwaysOn || nodes.length <= 150;
    labelGroup.selectAll('text').remove();
    if (!show) return;
    labelGroup.selectAll('text')
      .data(nodes)
      .join('text')
      .attr('class', 'graph-label')
      .attr('dx', 8).attr('dy', 3)
      .text(d => d.name);
  };
  updateLabels();

  simulation.on('tick', () => {
    link
      .attr('x1', d => d.source.x).attr('y1', d => d.source.y)
      .attr('x2', d => d.target.x).attr('y2', d => d.target.y);
    node
      .attr('transform', d => `translate(${d.x},${d.y})`);
    labelGroup.selectAll('text')
      .attr('x', d => d.x)
      .attr('y', d => d.y);
  });

  // Zoom + filter wiring — using the filter bar from the HTML in Task 3.
  const filterBar = container.parentElement.querySelector('[data-filter-bar]');
  const minimapEl = container.parentElement.querySelector('#graph-minimap');
  const legendEl = container.parentElement.querySelector('[data-graph-legend]');
  const zoomState = { transform: { x: 0, y: 0, k: 1, scale: 1 } };
  minimapEl.dataset.canvasW = String(width);
  minimapEl.dataset.canvasH = String(height);

  // Wire zoom BEFORE buildFilterBar so onFilterChange can use zoomState.api
  // on the very first chip click (synchronous handlers are fine, but the
  // ordering documents intent).
  wireZoom(svgEl, viewport, zoomState, () => paintMinimap(graph, minimapEl, zoomState.transform, state));

  const onFilterChange = (extra) => {
    if (extra && extra.resetZoom && zoomState.api) {
      d3.select(svgEl).transition().duration(250).call(zoomState.api.transform, d3.zoomIdentity);
    }
    applyFiltersToDom(svgEl, graph, state);
    updateLabels();
    paintMinimap(graph, minimapEl, zoomState.transform, state);
  };
  buildFilterBar(graph, palette, state, filterBar, onFilterChange);
  paintLegend(graph, palette, legendEl);
  paintMinimap(graph, minimapEl, zoomState.transform, state);
  applyFiltersToDom(svgEl, graph, state);

  // Click on minimap → pan the main canvas to that point.
  minimapEl.addEventListener('click', (event) => {
    if (!zoomState.api) return;
    const rect = minimapEl.getBoundingClientRect();
    const vb = minimapEl.getAttribute('viewBox').split(' ').map(Number);
    const x = (event.clientX - rect.left) / rect.width * vb[2];
    const y = (event.clientY - rect.top) / rect.height * vb[3];
    const transform = d3.zoomIdentity.translate(width / 2 - x * zoomState.transform.scale, height / 2 - y * zoomState.transform.scale);
    d3.select(svgEl).transition().duration(250).call(zoomState.api.transform, transform);
  });
}

// Entry point for the tab dispatch (resolved as window.renderGraphTab in
// init()). Idempotent: safe to call on every tab click and on every picker
// change.
async function renderGraphTab() {
  const empty = document.getElementById('graph-empty');
  const meta = document.getElementById('graph-meta');
  const svg = document.getElementById('graph-canvas');
  if (!empty || !svg) return;

  const { state, list } = await loadGraphWorkspaces();
  if (state.mode !== 'auto' && !selectedGraphWorkspace) {
    renderGraphTabEmpty(state, list);
    return;
  }

  const target = selectedGraphWorkspace || state.workspace;
  const active = list.find(ws => ws && ws.is_active === true);
  // The server holds one workspace at a time and get_workspace_graph derives
  // it from the loaded repos, so a non-active choice cannot be drawn.
  // See docs/opinions/graph-tab-data-source.md.
  if (active && target !== active.name) {
    renderGraphTabEmpty({ mode: 'not-loaded', workspace: target }, list);
    return;
  }

  populateGraphPicker(list, target);
  empty.className = 'muted';
  empty.textContent = 'Loading graph…';
  svg.innerHTML = '';

  let result;
  try {
    result = await mcpCall('get_workspace_graph', {});
  } catch (e) {
    renderGraphTabEmpty(
      { mode: 'error', message: `get_workspace_graph failed: ${e.message}` }, list);
    return;
  }
  if (result && result.isError) {
    const msg = unwrapText(result) || 'error';
    // The server is up but holds no matching workspace — same actionable
    // hint as a non-active pick.
    if (/no workspace matches|requires federation mode/i.test(msg)) {
      renderGraphTabEmpty({ mode: 'not-loaded', workspace: target }, list);
      return;
    }
    renderGraphTabEmpty({ mode: 'error', message: msg }, list);
    return;
  }

  const graph = normalizeGraphPayload(parseJson(result));
  if (graph.nodes.length === 0) {
    empty.className = 'muted';
    empty.textContent =
      `Workspace ${target} has no Function/Method/Class nodes to draw yet.`;
    if (meta) meta.textContent = '';
    return;
  }

  empty.textContent = '';
  if (meta) {
    const cross = graph.edges.filter(e => e.cross_repo).length;
    meta.textContent =
      `${graph.nodes.length} nodes · ${graph.edges.length} edges · ` +
      `${cross} cross-repo${graph.truncated ? ' · truncated' : ''}`;
  }
  drawGraphSvg(svg, graph);
}

// Attach the graph workspace picker's `change` listener exactly once. Called
// from init() — NOT from renderGraphTab(), which re-runs on every tab click
// and would stack duplicate listeners. Enforced by
// `graph_picker_is_wired_once_from_init` in command_center_assets_tests.rs.
function wireGraphPicker() {
  const select = document.getElementById('graph-workspace');
  if (!select) return;
  select.addEventListener('change', () => {
    // '' is the "— pick a workspace —" placeholder; treat it as "nothing
    // chosen" so the tab falls back to its picker empty state.
    selectedGraphWorkspace = select.value || null;
    renderGraphTab();
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

// ── Theme ──────────────────────────────────────────────────────────────────

// The effective theme, mirroring the cascade in theme.css: an explicit
// [data-theme] wins; otherwise light applies only when the system asks for it
// and everything else falls through to the phosphor default.
function currentTheme() {
  const explicit = document.documentElement.getAttribute('data-theme');
  if (explicit === 'light' || explicit === 'dark') return explicit;
  return window.matchMedia('(prefers-color-scheme: light)').matches
    ? 'light'
    : 'dark';
}

function wireThemeToggle() {
  const btn = document.getElementById('theme-toggle');
  if (!btn) return;

  const paint = () => {
    const t = currentTheme();
    btn.textContent = t === 'dark' ? 'phosphor' : 'paper';
    const next = t === 'dark' ? 'paper (light)' : 'phosphor (dark)';
    btn.title = 'Switch to ' + next;
    btn.setAttribute('aria-label', btn.title);
  };

  btn.addEventListener('click', () => {
    const next = currentTheme() === 'dark' ? 'light' : 'dark';
    document.documentElement.setAttribute('data-theme', next);
    try { localStorage.setItem('lain-theme', next); } catch (e) { /* ignore */ }
    paint();
  });

  // Keep following the system until the user makes an explicit choice.
  window.matchMedia('(prefers-color-scheme: light)').addEventListener(
    'change',
    () => { if (!document.documentElement.hasAttribute('data-theme')) paint(); }
  );

  paint();
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
      // Light up the selected tab in the tab bar.
      document.querySelectorAll('[data-tab]').forEach(b => {
        b.classList.toggle('active', b === btn);
      });
      const fn = window['render' + name.charAt(0).toUpperCase() + name.slice(1) + 'Tab'];
      if (typeof fn === 'function') fn();
    });
  });

  // Show the first tab by default.
  const first = document.querySelector('.tab');
  if (first) first.style.display = 'block';
  const firstBtn = document.querySelector('[data-tab]');
  if (firstBtn) firstBtn.classList.add('active');

  wireThemeToggle();
  wireGraphPicker();

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

// Only boot in a browser. Under Node (the `node --test` unit tests in
// tests/js/) there is no `document`, and the pure helpers below are the
// only thing the tests touch.
if (typeof document !== 'undefined') {
  init();
}

// CommonJS export footer — present only so `node --test tests/js/` can import
// the pure helpers directly instead of re-implementing them. Browsers ignore
// this branch (`module` is undefined there).
if (typeof module !== 'undefined' && module.exports) {
  module.exports = {
    collapseBursts,
    filterConflictEvents,
    pickWorkspaceForGraph,
    classifyWorkspacesResult,
    normalizeGraphPayload,
    // SPA graph upgrade (2026-08-31):
    computeRepoPalette,
    applyFilters,
    repoColour,
    nodeShape,
    nodeRadius,
  };
}
