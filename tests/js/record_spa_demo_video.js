// Records a ~60-second demo of the Command Center SPA with video capture.
// Built for the make-demo-video.sh pipeline.
//
// Run: node tests/js/record_spa_demo_video.js --out /tmp/demo/spa.webm
// Env:
//   LAIN_BIN   path to the lain binary
//
// Exits 0 on success, non-zero on any failure.

'use strict';

const { chromium } = require('playwright');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const LAIN_BIN = process.env.LAIN_BIN
  || path.resolve(__dirname, '..', '..', 'target', 'release', 'lain');
const CHROMIUM_BIN = process.env.CHROMIUM_BIN
  || require('playwright').chromium.executablePath()
  || '/home/spuentesp/.cache/ms-playwright/chromium-1234/chrome-linux64/chrome';

// ── CLI args ────────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const out = {
    out: '/tmp/lain-spa-demo-video/spa.webm',
    port: 9931,
    workdir: null,
    workspace: 'tokio-stack',
    ready_timeout_ms: 600_000,
    server_pid: null,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--out')          out.out       = argv[++i];
    else if (a === '--port')    out.port      = Number(argv[++i]);
    else if (a === '--workdir') out.workdir   = argv[++i];
    else if (a === '--workspace') out.workspace = argv[++i];
    else if (a === '--ready-timeout-ms') out.ready_timeout_ms = Number(argv[++i]);
    else if (a === '--server-pid') out.server_pid = Number(argv[++i]);
    else { console.error(`unknown flag: ${a}`); process.exit(2); }
  }
  return out;
}

// ── Lifecycle helpers ───────────────────────────────────────────────────────

function startServer(workdir, port, workspace) {
  const configPath = path.join(workdir, 'repos.yaml');
  const logPath = path.join(workdir, 'server.log');
  const logFd = fs.openSync(logPath, 'w');
  const proc = spawn(
    LAIN_BIN,
    [
      'server',
      '--config', configPath,
      '--workspace', workspace,
      '--transport', 'http',
      '--port', String(port),
      '--log-level', 'warn',
    ],
    {
      cwd: workdir,
      env: { ...process.env, LAIN_API_KEYS: '' },
      stdio: ['ignore', logFd, logFd],
    },
  );
  proc._logPath = logPath;
  return proc;
}

function isProcessAlive(pid) {
  try { return process.kill(pid, 0); } catch (_) { return false; }
}

// Pure predicate: does this /health payload describe a federation with at
// least `minRepos` repos, all ready? Shared by waitForReady and the server
// reuse check so the two cannot disagree.
function federationIsReady(body, minRepos) {
  return !!(body && body.federation &&
    Array.isArray(body.federation.repos) &&
    body.federation.repos.length >= minRepos &&
    body.federation.repos.every(r => r.health === 'ready' || r.health === 'ok'));
}

async function federationReadyWithin(baseUrl, minRepos, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${baseUrl}/health`);
      if (res.status === 200 && federationIsReady(await res.json().catch(() => null), minRepos)) {
        return true;
      }
    } catch (_) { /* not up yet */ }
    await new Promise(r => setTimeout(r, 300));
  }
  return false;
}

// Returns a serverProc object. If serverPid is given AND that server is
// actually serving the expected federation, returns a no-op proc (kill is a
// no-op) so the finally block is safe. Liveness alone is not enough: a
// leftover server from an earlier run can be perfectly alive while serving
// zero repos — it was started against a config that has since been fixed —
// and reusing it wedges waitForReady for the full timeout.
async function makeServerProc(workdir, port, workspace, serverPid, minRepos) {
  if (serverPid !== null) {
    console.log(`  server-pid=${serverPid} — checking liveness...`);
    if (isProcessAlive(serverPid)) {
      const baseUrl = `http://127.0.0.1:${port}`;
      if (await federationReadyWithin(baseUrl, minRepos, 5_000)) {
        console.log(`  reusing existing server (pid ${serverPid})`);
        return { kill: () => {}, _logPath: null, _reused: true };
      }
      console.log(`  server-pid ${serverPid} is alive but not serving ${minRepos} ready repos — restarting it`);
      try { process.kill(serverPid, 'SIGTERM'); } catch (_) { /* already gone */ }
      await new Promise(r => setTimeout(r, 750));
    } else {
      console.log(`  server-pid ${serverPid} is not alive — will start our own`);
    }
  }
  return startServer(workdir, port, workspace);
}

async function waitForReady(baseUrl, timeoutMs, minRepos = 2) {
  const deadline = Date.now() + timeoutMs;
  let lastErr = null;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${baseUrl}/health`);
      if (res.status === 200) {
        if (federationIsReady(await res.json().catch(() => null), minRepos)) {
          return;
        }
      }
      lastErr = new Error(`status=${res.status}`);
    } catch (e) { lastErr = e; }
    await new Promise(r => setTimeout(r, 500));
  }
  throw new Error(`federation not ready within ${timeoutMs}ms: ${lastErr && lastErr.message}`);
}

async function probeFederationCrossRepoGraph(baseUrl, workdir, timeoutMs) {
  const expectedRepos = readRepoIdsFromConfig(workdir);
  const deadline = Date.now() + timeoutMs;
  let lastErr = null;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${baseUrl}/mcp`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          jsonrpc: '2.0',
          method: 'tools/call',
          params: { name: 'get_workspace_graph', arguments: {} },
          id: 1,
        }),
      });
      if (!res.ok) { lastErr = new Error(`mcp HTTP ${res.status}`); }
      else {
        const env = await res.json();
        if (env.error) { lastErr = new Error(`jsonrpc error ${env.error.code}: ${env.error.message}`); }
        else if (env.result && env.result.isError) {
          const text = env.result.content && env.result.content[0] && env.result.content[0].text;
          lastErr = new Error(`tool error: ${text}`);
        } else {
          const text = env.result && env.result.content && env.result.content[0] && env.result.content[0].text;
          if (!text) { lastErr = new Error('tool returned empty content'); }
          else {
            let payload;
            try { payload = JSON.parse(text); }
            catch (e) { lastErr = new Error(`tool payload not JSON: ${e.message}`); }
            if (payload) {
              const nodes = Array.isArray(payload.nodes) ? payload.nodes : [];
              const repos = new Set(nodes.map(n => n && n.repo_id).filter(Boolean));
              const missing = expectedRepos.filter(r => !repos.has(r));
              if (nodes.length > 0 && missing.length === 0) {
                console.log(`  federation probe OK: workspace_graph nodes=${nodes.length} repos=${Object.keys(repos).sort().join(',')}`);
                return;
              }
              lastErr = new Error(`workspace graph missing cross-repo nodes — repos=${[...repos].sort().join(',') || '<none>'} node_count=${nodes.length} missing=${missing.join(',') || '<none>'}`);
            }
          }
        }
      }
    } catch (e) { lastErr = e; }
    await new Promise(r => setTimeout(r, 500));
  }
  throw new Error(`federation probe failed: ${lastErr && lastErr.message}`);
}

function readRepoIdsFromConfig(workdir) {
  const configPath = path.join(workdir, 'repos.yaml');
  try {
    const text = fs.readFileSync(configPath, 'utf8');
    try {
      const cfg = JSON.parse(text);
      if (Array.isArray(cfg.repos)) return cfg.repos.map(r => r && r.id).filter(Boolean);
    } catch (_) {}
    // YAML fallback: match `- id: <name>`
    const ids = [];
    const re = /^\s*-\s*id:\s*([A-Za-z0-9_.-]+)/gm;
    let m;
    while ((m = re.exec(text)) !== null) ids.push(m[1]);
    return ids;
  } catch (_) { return []; }
}

// ── Tab helpers ─────────────────────────────────────────────────────────────

async function clickTab(page, name) {
  await page.click(`nav.tabs button[data-tab="${name}"]`);
  await page.waitForFunction(
    (t) => {
      const el = document.getElementById('tab-' + t);
      return el && window.getComputedStyle(el).display !== 'none';
    },
    name,
    { timeout: 10_000 },
  );
}

// ── Demo drive sequence ─────────────────────────────────────────────────────

async function driveSequence(page, baseUrl) {
  // ── Beat 1: Overview ──────────────────────────────────────────────────
  await clickTab(page, 'overview');
  await page.waitForFunction(() => {
    const el = document.getElementById('tab-overview');
    return el && el.querySelector('pre, p, h3') !== null;
  }, { timeout: 15_000 });
  console.log('  [overview] visible');
  await new Promise(r => setTimeout(r, 3000));

  // ── Beat 2: Repos ────────────────────────────────────────────────────
  await clickTab(page, 'repos');
  await page.waitForFunction(() => {
    const rows = document.querySelectorAll('#tab-repos table.repo-table tbody tr');
    if (rows.length < 2) return false;
    const ids = Array.from(rows).map(r => (r.textContent || '').toLowerCase());
    const readyCount = Array.from(rows).filter(r => /ready/.test(r.textContent || '')).length;
    // For synthetic fixture: auth-svc + billing-svc; for real: bytes + tokio
    return ids.some(t => t.includes('auth') || t.includes('bytes'))
        && ids.some(t => t.includes('billing') || t.includes('tokio'))
        && readyCount >= 2;
  }, { timeout: 60_000 });
  console.log('  [repos] table populated');
  await new Promise(r => setTimeout(r, 3000));

  // ── Beat 3: Query tab ───────────────────────────────────────────────
  await clickTab(page, 'query');
  await page.waitForSelector('#tab-query #query-repo', { timeout: 10_000 });

  // Determine which repo to query
  const repoId = await page.evaluate(async (base) => {
    const r = await fetch(`${base}/mcp`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        jsonrpc: '2.0', id: 1, method: 'tools/call',
        params: { name: 'list_repos', arguments: {} },
      }),
    });
    const body = await r.json();
    const text = (body && body.result && body.result.content
      && body.result.content[0] && body.result.content[0].text) || '[]';
    try {
      const repos = JSON.parse(text);
      if (Array.isArray(repos) && repos.length > 0) return repos[0].id;
    } catch (_) {}
    return 'auth-svc';
  }, baseUrl);

  await page.fill('#query-repo', repoId);
  await page.fill('#query-type', 'Function');
  await page.fill('#query-limit', '20');
  await page.click('#query-run');
  await page.waitForFunction(() => {
    const el = document.getElementById('query-output');
    return el && el.textContent && el.textContent.trim().length > 0;
  }, { timeout: 15_000 });
  console.log(`  [query] ran query on ${repoId}`);
  await new Promise(r => setTimeout(r, 4000));

  // ── Beat 4: Tools tab ───────────────────────────────────────────────
  await clickTab(page, 'tools');
  await page.waitForSelector('#tab-tools #tools-list li button', { timeout: 20_000 });

  // Use get_workspace_graph as a reliable tool (always available)
  const toolName = 'get_workspace_graph';
  await page.evaluate((name) => {
    const items = document.querySelectorAll('#tab-tools #tools-list li');
    for (const li of items) {
      const btn = li.querySelector('button');
      if (btn && btn.textContent && btn.textContent.trim() === name) {
        btn.click();
        return;
      }
    }
    throw new Error(`${name} not in tools list`);
  }, toolName);
  await page.waitForSelector('#tab-tools #tool-args', { timeout: 10_000 });
  await page.click('#tab-tools #tool-call');
  await page.waitForFunction(() => {
    const el = document.getElementById('tool-result');
    return el && el.textContent && el.textContent.trim().length > 0;
  }, { timeout: 30_000 });
  console.log('  [tools] called get_workspace_graph');
  await new Promise(r => setTimeout(r, 5000));

  // ── Beat 5: Graph tab (CRITICAL — nodes must render) ─────────────────
  await clickTab(page, 'graph');
  await page.waitForFunction(
    () => !!document.querySelector('[data-graph-search]'),
    { timeout: 10_000 },
  );

  // Wait for anchor-mode graph data to load (D3 nodes must appear)
  let nodesAppeared = false;
  try {
    await page.waitForFunction(() => {
      const svg = document.getElementById('graph-canvas');
      return svg && svg.querySelectorAll('path.graph-node').length > 0;
    }, { timeout: 20_000 });
    nodesAppeared = true;
    console.log('  [graph] anchor nodes appeared');
  } catch (_) {
    console.log('  [graph] no anchor nodes — will try focal search');
  }

  // Wait for anchor loading to complete
  await page.waitForFunction(
    () => {
      const meta = document.getElementById('graph-meta');
      return meta && !/loading anchors/i.test(meta.textContent || '');
    },
    { timeout: 60_000 },
  );
  await new Promise(r => setTimeout(r, 2000));

  // Try focal search for a more interesting graph
  if (nodesAppeared) {
    // Find a symbol from anchors
    const focalSymbol = await page.evaluate(async () => {
      try {
        const r = await fetch('/mcp', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            jsonrpc: '2.0', id: 1, method: 'tools/call',
            params: { name: 'find_anchors', arguments: { limit: 5 } },
          }),
        });
        const body = await r.json();
        const text = (body && body.result && body.result.content
          && body.result.content[0] && body.result.content[0].text) || '';
        const m = text.match(/^\s*1\.\s+([A-Za-z_][A-Za-z0-9_]*)/m);
        return m ? m[1] : null;
      } catch (_) { return null; }
    });

    if (focalSymbol) {
      console.log(`  [graph] focal search: ${focalSymbol}`);
      await page.fill('[data-graph-search]', focalSymbol);
      await page.evaluate(() => {
        const input = document.querySelector('[data-graph-search]');
        if (input) input.dispatchEvent(new Event('input', { bubbles: true }));
      });
      await new Promise(r => setTimeout(r, 2000));

      // Check for focal candidate buttons
      try {
        await page.waitForSelector('[data-focal-candidate]', { timeout: 3000 })
          .then(b => b.click());
        console.log('  [graph] clicked focal candidate');
      } catch (_) {}
    }

    // Wait for D3 layout to settle
    await new Promise(r => setTimeout(r, 8000));
  } else {
    // No anchor nodes — sit on whatever the empty state is
    await new Promise(r => setTimeout(r, 5000));
  }

  console.log('  [graph] tab complete');
}

// ── Main ──────────────────────────────────────────────────────────────────

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const workdir = args.workdir || fs.mkdtempSync(path.join(os.tmpdir(), 'lain-spa-video-'));
  const baseUrl = `http://127.0.0.1:${args.port}`;
  const videoDir = path.dirname(args.out);
  fs.mkdirSync(videoDir, { recursive: true });

  console.log('== lain SPA video recorder ==');
  console.log(`  binary:    ${LAIN_BIN}`);
  console.log(`  workdir:   ${workdir}`);
  console.log(`  port:      ${args.port}`);
  console.log(`  workspace: ${args.workspace}`);
  console.log(`  out:       ${args.out}`);

  if (!fs.existsSync(LAIN_BIN)) {
    console.error(`FATAL: lain binary not found at ${LAIN_BIN}`);
    process.exit(2);
  }
  if (!fs.existsSync(CHROMIUM_BIN)) {
    console.error(`FATAL: chromium binary not found at ${CHROMIUM_BIN}`);
    process.exit(2);
  }

  const configPath = path.join(workdir, 'repos.yaml');
  if (!fs.existsSync(configPath)) {
    console.error(`FATAL: ${configPath} missing — populate workdir first`);
    process.exit(2);
  }

  console.log('  starting server...');
  const expectedRepos = readRepoIdsFromConfig(workdir).length;
  const serverProc = await makeServerProc(
    workdir, args.port, args.workspace, args.server_pid, Math.max(expectedRepos, 1),
  );

  let browser;
  let exitCode = 0;
  try {
    await waitForReady(baseUrl, args.ready_timeout_ms, Math.max(expectedRepos, 1));
    console.log('  federation ready');

    await probeFederationCrossRepoGraph(baseUrl, workdir, 180_000);
    console.log('  federation probe passed');

    // Launch with video recording
    browser = await chromium.launch({
      executablePath: CHROMIUM_BIN,
      args: ['--no-sandbox', '--disable-dev-shm-usage'],
      headless: true,
    });

    const context = await browser.newContext({
      viewport: { width: 1600, height: 900 },
      recordVideo: { dir: videoDir, size: { width: 1600, height: 900 } },
    });
    const page = await context.newPage();
    page.on('console', (msg) => {
      const text = msg.text();
      if (/SPA-FOCAL|SPA-RGT|SPA-WGC|console\.error|node not found/.test(text)) {
        process.stderr.write(`  [browser ${msg.type()}] ${text}\n`);
      }
    });
    page.on('pageerror', (err) => process.stderr.write(`  [browser pageerror] ${err.message}\n`));

    await page.goto(baseUrl + '/', { waitUntil: 'load', timeout: 30_000 });
    await page.waitForSelector('header.topbar h1', { timeout: 10_000 });

    // Screenshot at load time
    await page.screenshot({ path: path.join(videoDir, 'spa-load.png'), fullPage: true });

    await driveSequence(page, baseUrl);

    // Final screenshot
    await page.screenshot({ path: path.join(videoDir, 'spa-final.png'), fullPage: true });

    // Close page → video finalises
    const tempVideoPath = await page.video().path();
    await page.close();
    await context.close();
    await browser.close();
    browser = null;

    fs.renameSync(tempVideoPath, args.out);
    console.log(`  video written: ${args.out}`);
  } catch (e) {
    console.error(`FATAL: ${e.stack || e.message}`);
    exitCode = 1;
  } finally {
    if (browser) { try { await browser.close(); } catch (_) {} }
    serverProc.kill('SIGTERM');
    await new Promise(r => setTimeout(r, 500));
    try { serverProc.kill('SIGKILL'); } catch (_) {}
    console.log(`  server log: ${serverProc._logPath}`);
    if (!process.env.LAIN_RECORD_KEEP_DIR && !args.workdir) {
      try { fs.rmSync(workdir, { recursive: true, force: true }); } catch (_) {}
    } else {
      console.log(`  workdir preserved: ${workdir}`);
    }
  }
  process.exit(exitCode);
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = { federationIsReady, federationReadyWithin, parseArgs };
}

if (require.main === module) {
  main().catch(e => { console.error('FATAL:', e.message); process.exit(1); });
}
