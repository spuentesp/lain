// End-to-end Playwright test for the lain Command Center SPA.
//
// Boots `lain server --transport http --port <chosen>` against a fresh Rust
// fixture in a temp dir, drives Chromium (system binary at /usr/bin/chromium)
// through every advertised tab, captures screenshots, and verifies the SSE
// feed endpoint opens.
//
// Run: PLAYWRIGHT_BROWSERS_PATH=0 node tests/js/spa_e2e.test.js
// Env:
//   LAIN_BIN           path to the lain release binary (default: ./target/release/lain)
//   SPA_E2E_PORT       fixed port instead of an ephemeral one (debug aid)
//   SPA_E2E_KEEP_DIR   if set, the temp workdir is preserved for inspection
//
// Exits 0 on full pass, 1 if any assertion fails.

'use strict';

const { chromium } = require('playwright');
const { spawn, execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');

const LAIN_BIN = process.env.LAIN_BIN
  || path.resolve(__dirname, '..', '..', 'target', 'release', 'lain');
const SCREENSHOT_DIR = path.join(__dirname, 'screenshots');
const CHROMIUM_BIN = '/usr/bin/chromium';
const TAB_NAMES = ['overview', 'repos', 'query', 'tools', 'graph'];

// ── Result accounting ─────────────────────────────────────────────────────

let passCount = 0;
let failCount = 0;
const results = [];

function record(tab, name, ok, detail) {
  results.push({ tab, name, ok, detail: detail || '' });
  if (ok) {
    passCount++;
    console.log(`  PASS  [${tab}] ${name}`);
  } else {
    failCount++;
    console.log(`  FAIL  [${tab}] ${name}${detail ? ' :: ' + detail : ''}`);
  }
}

function assertTrue(tab, name, cond, detail) {
  record(tab, name, !!cond, detail || '');
  return !!cond;
}

// ── Test fixture + server lifecycle ──────────────────────────────────────

function findFreePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.unref();
    srv.on('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const port = srv.address().port;
      srv.close(() => resolve(port));
    });
  });
}

// Duplicates the spirit of `scripts/legacy/demo-federation-fixture.sh`
// (Rust crate + git init + `lain init`) but with a single-crate
// fixture for the SPA boot path. Task 4 left this out of scope; if the
// synthetic fixture script changes shape, update here.
function buildFixture(workdir) {
  // Minimal Cargo crate. `lain init` only needs a `.git` to walk up, but
  // giving it real Rust source means the indexer has something to chew on
  // and the SPA's Repos tab can show a non-trivial table.
  fs.writeFileSync(
    path.join(workdir, 'Cargo.toml'),
    '[package]\n' +
    'name = "spa_e2e_fixture"\n' +
    'version = "0.1.0"\n' +
    'edition = "2021"\n',
  );
  fs.mkdirSync(path.join(workdir, 'src'));
  fs.writeFileSync(
    path.join(workdir, 'src', 'lib.rs'),
    '/// Anchor: called by `entrypoint`, coordinates two helpers.\n' +
    'pub fn orchestrate() -> u32 {\n' +
    '    helper_a() + helper_b()\n' +
    '}\n' +
    'pub fn entrypoint() -> u32 { orchestrate() }\n' +
    'pub fn helper_a() -> u32 { 1 }\n' +
    'pub fn helper_b() -> u32 { 2 }\n',
  );

  execFileSync('git', ['init', '-q'], { cwd: workdir });
  execFileSync('git', ['config', 'user.email', 'spa-e2e@lain'], { cwd: workdir });
  execFileSync('git', ['config', 'user.name', 'spa-e2e'], { cwd: workdir });
  execFileSync('git', ['add', '-A'], { cwd: workdir });
  execFileSync('git', ['commit', '-q', '-m', 'fixture'], { cwd: workdir });

  // Scaffold repos.yaml. Use --workspace so we don't depend on git's CWD walk.
  execFileSync(LAIN_BIN, ['init', '--workspace', workdir], {
    cwd: workdir,
    stdio: ['ignore', 'pipe', 'pipe'],
  });

  // The Graph tab's `renderGraphTab` only enters the drawing path when
  // `list_workspaces` returns a workspace whose members match the loaded
  // repos. `lain init` writes `repos.yaml` but not `workspaces.yaml`, so
  // the server would register no workspace tools and the tab would land
  // in the "No workspace indexed yet" empty state. The Task 6 wait
  // (`path.graph-node > 0`) explicitly requires an actual graph, so
  // promote the fixture to a single-member workspace and let the SPA
  // auto-select it (`pickWorkspaceForGraph` returns `mode: 'auto'` when
  // there is exactly one workspace).
  const repoId = path.basename(workdir);
  fs.writeFileSync(
    path.join(workdir, 'workspaces.yaml'),
    `workspaces:\n` +
    `  - name: e2e-workspace\n` +
    `    members: [${repoId}]\n`,
  );
}

async function waitFor(url, predicate, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let lastErr = null;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(url);
      if (predicate(res)) return res;
      lastErr = new Error(`status=${res.status}`);
    } catch (e) {
      lastErr = e;
    }
    await new Promise(r => setTimeout(r, 250));
  }
  throw new Error(`timeout waiting for ${label} (${url}): ${lastErr && lastErr.message}`);
}

function startServer(workdir, port) {
  const configPath = path.join(workdir, 'repos.yaml');
  const proc = spawn(
    LAIN_BIN,
    [
      'server',
      '--config', configPath,
      '--transport', 'http',
      '--port', String(port),
      '--log-level', 'warn',
    ],
    {
      cwd: workdir,
      env: {
        ...process.env,
        // Keep dev mode: no API key, no rate limit.
        LAIN_API_KEYS: '',
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    },
  );
  // Drain stdout/stderr so the child never blocks on a full pipe.
  proc.stdout.on('data', () => {});
  proc.stderr.on('data', () => {});
  return proc;
}

// ── Tab assertions ───────────────────────────────────────────────────────

async function assertChrome(page) {
  const tab = 'chrome';
  await page.waitForSelector('header.topbar h1', { timeout: 10_000 });
  assertTrue(tab, 'topbar renders <h1>', !!(await page.$('header.topbar h1')));
  const heading = await page.textContent('header.topbar h1');
  assertTrue(tab, 'topbar heading contains LAIN',
    (heading || '').trim() === 'LAIN', `got: ${JSON.stringify(heading)}`);

  assertTrue(tab, 'sidebar rendered',
    !!(await page.$('aside.sidebar')));
  assertTrue(tab, 'sidebar Workspaces section present',
    !!(await page.$('aside.sidebar #workspaces')));

  assertTrue(tab, 'statusbar rendered',
    !!(await page.$('footer.statusbar')));
  assertTrue(tab, 'statusbar pid field present',
    !!(await page.$('footer.statusbar #status-pid')));
  assertTrue(tab, 'statusbar transport field present',
    !!(await page.$('footer.statusbar #status-transport')));

  // Wait for status bar transport text to be populated by renderStatusBar().
  await page.waitForFunction(
    () => {
      const el = document.getElementById('status-transport');
      return el && /transport:\s*http/i.test(el.textContent || '');
    },
    { timeout: 15_000 },
  );
  assertTrue(tab, 'statusbar transport text = http',
    true);
}

async function assertTabVisible(page, tab) {
  // The tab section is shown when its `display` style is `block`. Other
  // tabs are hidden via inline style.
  return page.evaluate((t) => {
    const el = document.getElementById('tab-' + t);
    if (!el) return false;
    const style = window.getComputedStyle(el);
    return style.display !== 'none';
  }, tab);
}

async function clickTab(page, tab) {
  await page.click(`nav.tabs button[data-tab="${tab}"]`);
  // Re-render is async; wait until this tab is the visible one.
  await page.waitForFunction(
    (t) => {
      const el = document.getElementById('tab-' + t);
      if (!el) return false;
      return window.getComputedStyle(el).display !== 'none';
    },
    tab,
    { timeout: 10_000 },
  );
}

async function tabHasContent(page, tab) {
  // Tab content area non-empty (innerHTML has something meaningful — at
  // minimum a <p>, <pre>, <table>, or <svg>).
  return page.evaluate((t) => {
    const el = document.getElementById('tab-' + t);
    if (!el) return false;
    const html = (el.innerHTML || '').trim();
    return html.length > 0;
  }, tab);
}

async function assertOverview(page) {
  const tab = 'overview';
  await clickTab(page, tab);
  assertTrue(tab, 'tab-overview visible', await assertTabVisible(page, tab));
  // Wait for either the health JSON <pre> blocks or the empty-state <p>.
  await page.waitForFunction(() => {
    const el = document.getElementById('tab-overview');
    if (!el) return false;
    return el.querySelector('pre, p, h3') !== null;
  }, { timeout: 15_000 });
  assertTrue(tab, 'tab content rendered', await tabHasContent(page, tab));
  // The Overview tab is built from two MCP calls: `get_federation_health`
  // (always populated when a federation is wired) and `get_health` (the
  // tool returns a markdown report, which `parseJson` can't read so the
  // "Server health" <h3> is conditional). Accept either heading — the
  // federation blob is the one operators actually look at on a fresh
  // federation-mode server.
  const heading = await page.evaluate(() => {
    const el = document.getElementById('tab-overview');
    if (!el) return null;
    if (/Server health/i.test(el.textContent || '')) return 'server';
    if (/Federation health/i.test(el.textContent || '')) return 'federation';
    return null;
  });
  assertTrue(tab, 'Overview shows Server or Federation health heading',
    heading !== null, heading || '');
  // Federation-mode specifics: the JSON blob should mention
  // `total_repos`/`ready`/etc.
  const hasFederationDetail = await page.evaluate(() => {
    const el = document.getElementById('tab-overview');
    if (!el) return false;
    return /total_repos|"ready"|"healthy"/i.test(el.textContent || '');
  });
  assertTrue(tab, 'Federation health JSON rendered with detail',
    hasFederationDetail);
}

async function assertRepos(page) {
  const tab = 'repos';
  await clickTab(page, tab);
  assertTrue(tab, 'tab-repos visible', await assertTabVisible(page, tab));
  await page.waitForFunction(() => {
    const el = document.getElementById('tab-repos');
    if (!el) return false;
    // Either a table (rows indexed) or the "No repos registered" / error msg.
    return el.querySelector('table, p, h3') !== null;
  }, { timeout: 20_000 });
  assertTrue(tab, 'tab content rendered', await tabHasContent(page, tab));
  // With our fixture + WorkspaceDir source, the indexer should surface at
  // least one repo. Wait up to 30s for the table.
  const tableReady = await page
    .waitForSelector('#tab-repos table.repo-table tbody tr', { timeout: 30_000 })
    .then(() => true)
    .catch(() => false);
  assertTrue(tab, 'repo table has ≥1 row', tableReady);
}

async function assertQuery(page) {
  const tab = 'query';
  await clickTab(page, tab);
  assertTrue(tab, 'tab-query visible', await assertTabVisible(page, tab));
  await page.waitForSelector('#tab-query .query-form', { timeout: 10_000 });
  assertTrue(tab, 'query form rendered',
    !!(await page.$('#tab-query .query-form')));
  assertTrue(tab, 'repo_id input present',
    !!(await page.$('#tab-query #query-repo')));
  assertTrue(tab, 'op select present',
    !!(await page.$('#tab-query #query-op')));
  assertTrue(tab, 'type input present',
    !!(await page.$('#tab-query #query-type')));
  assertTrue(tab, 'Run button present',
    !!(await page.$('#tab-query #query-run')));
  // With a registered repo, the datalist gets populated; verify the
  // option for our fixture's repo_id is present.
  const optionCount = await page.evaluate(() => {
    const dl = document.getElementById('repo-list');
    return dl ? dl.querySelectorAll('option').length : 0;
  });
  assertTrue(tab, 'repo-list datalist populated', optionCount >= 1,
    `optionCount=${optionCount}`);
  assertTrue(tab, 'tab content rendered', await tabHasContent(page, tab));
}

async function assertTools(page) {
  const tab = 'tools';
  await clickTab(page, tab);
  assertTrue(tab, 'tab-tools visible', await assertTabVisible(page, tab));
  // Wait for the tools-list to populate from /mcp tools/list.
  await page.waitForSelector('#tab-tools #tools-list li', { timeout: 20_000 });
  assertTrue(tab, 'tools list populated',
    !!(await page.$('#tab-tools #tools-list li')));
  assertTrue(tab, 'tab content rendered', await tabHasContent(page, tab));

  // Click the first tool so the form (with "Copy as cURL") is rendered.
  await page.click('#tab-tools #tools-list li button');
  await page.waitForSelector('#tab-tools #tool-curl', { timeout: 10_000 });
  const hasCopy = await page.evaluate(() => {
    const btn = document.getElementById('tool-curl');
    return !!btn && /Copy as cURL/i.test(btn.textContent || '');
  });
  assertTrue(tab, '"Copy as cURL" button present', hasCopy);
  const hasCall = await page.evaluate(() => !!document.getElementById('tool-call'));
  assertTrue(tab, '"Call" button present', hasCall);
  const hasForm = await page.evaluate(() => !!document.getElementById('tool-args'));
  assertTrue(tab, 'tool form <form id="tool-args"> present', hasForm);
}

async function assertGraph(page) {
  const tab = 'graph';
  await clickTab(page, tab);
  assertTrue(tab, 'tab-graph visible', await assertTabVisible(page, tab));
  // Wait for the renderer to settle. After the SPA graph upgrade (Tasks
  // 1-5) nodes are <path class="graph-node"> rather than <circle>; the
  // presence of at least one path.graph-node is the proof that
  // drawGraphSvg has produced a real graph.
  await page.waitForFunction(() => {
    const svg = document.getElementById('graph-canvas');
    return svg && svg.querySelectorAll('path.graph-node').length > 0;
  }, { timeout: 15_000 });
  const settled = await page.evaluate(() => {
    const svg = document.getElementById('graph-canvas');
    const empty = document.getElementById('graph-empty');
    if (svg && svg.childElementCount > 0) return { kind: 'graph', nodes: svg.childElementCount };
    const t = (empty && empty.textContent || '').trim();
    return { kind: 'empty', text: t };
  });
  assertTrue(tab, 'graph tab renders canvas or empty message',
    settled.kind === 'graph' || (settled.kind === 'empty' && settled.text.length > 0),
    JSON.stringify(settled));
  // The graph picker (workspace dropdown) should be on screen.
  assertTrue(tab, 'graph workspace picker rendered',
    !!(await page.$('#tab-graph #graph-workspace')));
  assertTrue(tab, 'tab content rendered', await tabHasContent(page, tab));

  // Filter-bar assertions: chip rows are stamped by Task 3's HTML and
  // populated by buildFilterBar (Task 5). The data-* attribute selectors
  // are the contract — no positional queries.
  const filterBar = await page.evaluate(() => {
    const bar = document.querySelector('[data-filter-bar]');
    if (!bar) return null;
    return {
      repoChips: bar.querySelectorAll('[data-filter-row="repos"] .graph-chip').length,
      kindChips: bar.querySelectorAll('[data-filter-row="kinds"] .graph-chip').length,
      toggleChips: bar.querySelectorAll('[data-filter-row="toggles"] .graph-chip').length,
    };
  });
  assertTrue(tab, 'filter bar present', !!filterBar);
  assertTrue(tab, 'at least one repo chip',
    !!filterBar && filterBar.repoChips > 0,
    filterBar ? `repoChips=${filterBar.repoChips}` : 'filter bar missing');
  assertTrue(tab, 'three kind chips (Function/Method/Class)',
    !!filterBar && filterBar.kindChips === 3,
    filterBar ? `kindChips=${filterBar.kindChips}` : 'filter bar missing');
  assertTrue(tab, 'at least two toggle chips (cross-repo-only, labels)',
    !!filterBar && filterBar.toggleChips >= 2,
    filterBar ? `toggleChips=${filterBar.toggleChips}` : 'filter bar missing');

  // Toggle the first repo chip and verify the matching nodes get
  // .is-hidden. Clicking again restores the chip's on-state, so the
  // fixture's nodes return to visible.
  const droppedCount = await page.evaluate(() => {
    const firstRepoChip = document.querySelector('[data-filter-row="repos"] .graph-chip');
    const before = document.querySelectorAll('.graph-node.is-hidden').length;
    firstRepoChip.click();
    const after = document.querySelectorAll('.graph-node.is-hidden').length;
    firstRepoChip.click();           // toggle back on
    return { before, after };
  });
  assertTrue(tab, 'clicking repo chip hides matching nodes',
    droppedCount.after > droppedCount.before,
    `before=${droppedCount.before} after=${droppedCount.after}`);

  // Legend has at least one cell per repo × kind.
  const legendCells = await page.evaluate(() => {
    return document.querySelectorAll('[data-graph-legend] .graph-legend-cell').length;
  });
  assertTrue(tab, 'legend has at least one cell per (repo, kind) pair',
    legendCells >= 3, `legendCells=${legendCells}`);

  // Wheel zoom produces a transform on the inner .graph-viewport <g>.
  await page.evaluate(() => {
    const svg = document.getElementById('graph-canvas');
    svg.dispatchEvent(new WheelEvent('wheel', { deltaY: -100, bubbles: true }));
  });
  await new Promise(r => setTimeout(r, 200));
  const zoomed = await page.evaluate(() => {
    const v = document.querySelector('.graph-viewport');
    return v && v.getAttribute('transform');
  });
  assertTrue(tab, 'wheel event triggers scale transform on .graph-viewport',
    zoomed && /scale\(/.test(zoomed), `transform=${JSON.stringify(zoomed)}`);
}

async function assertSse(baseUrl) {
  const tab = 'sse';
  // Use a short-lived fetch — we just need to confirm the endpoint opens
  // with text/event-stream and serves the synthetic `ready` frame.
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 5_000);
  try {
    const res = await fetch(`${baseUrl}/events`, {
      method: 'GET',
      headers: { Accept: 'text/event-stream' },
      signal: ctrl.signal,
    });
    assertTrue(tab, '/events returns 200', res.status === 200,
      `status=${res.status}`);
    const ct = res.headers.get('content-type') || '';
    assertTrue(tab, '/events Content-Type is text/event-stream',
      ct.includes('text/event-stream'), `got: ${ct}`);
    // Read at most the first chunk to confirm we get data.
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    const { value } = await reader.read();
    const chunk = decoder.decode(value || new Uint8Array());
    assertTrue(tab, '/events emits at least one SSE frame',
      chunk.includes('event:') && chunk.includes('data:'),
      `chunk: ${JSON.stringify(chunk).slice(0, 120)}`);
    try { reader.cancel(); } catch (_) { /* ignore */ }
  } catch (e) {
    if (e.name === 'AbortError') {
      assertTrue(tab, '/events responded within 5s', false, 'timeout');
    } else {
      assertTrue(tab, '/events responds', false, e.message);
    }
  } finally {
    clearTimeout(timer);
  }
}

// ── Main ──────────────────────────────────────────────────────────────────

async function main() {
  const workdir = fs.mkdtempSync(path.join(os.tmpdir(), 'lain-spa-e2e-'));
  const port = process.env.SPA_E2E_PORT
    ? Number(process.env.SPA_E2E_PORT)
    : await findFreePort();
  const baseUrl = `http://127.0.0.1:${port}`;

  console.log(`== lain SPA e2e ==`);
  console.log(`  binary:  ${LAIN_BIN}`);
  console.log(`  port:    ${port}`);
  console.log(`  workdir: ${workdir}`);
  console.log(`  base:    ${baseUrl}`);

  // Pre-flight.
  if (!fs.existsSync(LAIN_BIN)) {
    console.error(`FATAL: lain binary not found at ${LAIN_BIN}`);
    process.exit(2);
  }
  if (!fs.existsSync(CHROMIUM_BIN)) {
    console.error(`FATAL: chromium binary not found at ${CHROMIUM_BIN}`);
    process.exit(2);
  }

  fs.mkdirSync(SCREENSHOT_DIR, { recursive: true });

  console.log(`  building fixture...`);
  buildFixture(workdir);

  console.log(`  starting server...`);
  const serverProc = startServer(workdir, port);

  let exitCode = 0;
  let browser;
  try {
    // Wait for HTTP listener.
    await waitFor(
      `${baseUrl}/health`,
      (r) => r.status === 200,
      30_000,
      '/health=200',
    );
    console.log(`  /health OK, waiting for federation to populate...`);
    // Federation initialization can take a while; poll the federation blob
    // out of /health until it reports ≥1 repo (or we time out).
    try {
      await waitFor(
        `${baseUrl}/health`,
        (r) => r.json().then((j) => j && j.federation &&
          Array.isArray(j.federation.repos) &&
          j.federation.repos.length >= 1).catch(() => false),
        120_000,
        'federation.repos>=1',
      );
    } catch (_) {
      console.log(`  (federation did not report a repo within 120s — SPA will still render)`);
    }

    // SSE check before the browser (cheap, no DOM needed).
    await assertSse(baseUrl);

    console.log(`  launching chromium...`);
    browser = await chromium.launch({
      executablePath: CHROMIUM_BIN,
      args: ['--no-sandbox', '--disable-dev-shm-usage'],
      // Don't try to download a bundled browser.
      headless: true,
    });

    const context = await browser.newContext({
      viewport: { width: 1280, height: 900 },
    });
    const page = await context.newPage();

    // Navigate first — the SPA boots asynchronously.
    await page.goto(baseUrl + '/', { waitUntil: 'load', timeout: 30_000 });

    // SPA chrome.
    await assertChrome(page);

    // Per-tab assertions in the same SPA session.
    await assertOverview(page);
    await page.screenshot({
      path: path.join(SCREENSHOT_DIR, 'tab-overview.png'),
      fullPage: true,
    });

    await assertRepos(page);
    await page.screenshot({
      path: path.join(SCREENSHOT_DIR, 'tab-repos.png'),
      fullPage: true,
    });

    await assertQuery(page);
    await page.screenshot({
      path: path.join(SCREENSHOT_DIR, 'tab-query.png'),
      fullPage: true,
    });

    await assertTools(page);
    await page.screenshot({
      path: path.join(SCREENSHOT_DIR, 'tab-tools.png'),
      fullPage: true,
    });

    await assertGraph(page);
    await page.screenshot({
      path: path.join(SCREENSHOT_DIR, 'tab-graph.png'),
      fullPage: true,
    });

    // Per-tab pass/fail roll-up.
    console.log(`\n== per-tab results ==`);
    for (const t of TAB_NAMES) {
      const sub = results.filter(r => r.tab === t);
      const ok = sub.filter(r => r.ok).length;
      console.log(`  ${t.padEnd(10)} ${ok}/${sub.length}`);
    }

    console.log(`\n== summary ==`);
    console.log(`  total: ${passCount} pass, ${failCount} fail (${results.length} assertions)`);
    console.log(`  screenshots: ${SCREENSHOT_DIR}`);
    exitCode = failCount === 0 ? 0 : 1;
  } catch (e) {
    console.error(`FATAL during test run: ${e.stack || e.message}`);
    exitCode = 2;
  } finally {
    if (browser) {
      try { await browser.close(); } catch (_) { /* ignore */ }
    }
    serverProc.kill('SIGTERM');
    // Give it a moment, then SIGKILL if it's still alive.
    await new Promise(r => setTimeout(r, 500));
    try { serverProc.kill('SIGKILL'); } catch (_) { /* ignore */ }
    if (!process.env.SPA_E2E_KEEP_DIR) {
      try { fs.rmSync(workdir, { recursive: true, force: true }); } catch (_) { /* ignore */ }
    } else {
      console.log(`  (SPA_E2E_KEEP_DIR set, workdir preserved: ${workdir})`);
    }
  }
  process.exit(exitCode);
}

main();
