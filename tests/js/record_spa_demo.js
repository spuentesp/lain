// Records a ~45-second demo of the Command Center SPA against the
// 2-repo federation fixture built by `scripts/demo-federation-fixture.sh`.
//
// Run: node tests/js/record_spa_demo.js --out /tmp/lain-spa-demo/raw.webm
// Env:
//   LAIN_BIN   path to the lain binary (default: ./target/release/lain)
//
// Exits 0 on success, non-zero on any failure. Step timings are pinned
// so two recordings have identical frame timing — anything that breaks
// determinism (extra long-running calls, D3 layout not settling) makes
// the resulting GIF choppy on slower hosts.

'use strict';

const { chromium } = require('playwright');
const { spawn, execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const LAIN_BIN = process.env.LAIN_BIN
  || path.resolve(__dirname, '..', '..', 'target', 'release', 'lain');
const CHROMIUM_BIN = '/usr/bin/chromium';
const FIXTURE_SCRIPT = path.resolve(
  __dirname, '..', '..', 'scripts', 'demo-federation-fixture.sh',
);

// ── CLI args ────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const out = { out: '/tmp/lain-spa-demo/raw.webm', port: 9931, workdir: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--out')     out.out     = argv[++i];
    else if (a === '--port') out.port = Number(argv[++i]);
    else if (a === '--workdir') out.workdir = argv[++i];
    else { console.error(`unknown flag: ${a}`); process.exit(2); }
  }
  return out;
}

// ── Lifecycle helpers ───────────────────────────────────────────────────

function buildFixture(workdir) {
  execFileSync('bash', [FIXTURE_SCRIPT, workdir], { stdio: ['ignore', 'pipe', 'pipe'] });
}

function startServer(workdir, port) {
  const configPath = path.join(workdir, 'repos.yaml');
  // Capture the server's log to a file rather than a Node pipe. With
  // `stdio: ['pipe', 'pipe']` the kernel pipe buffer fills once the SPA
  // fires its first burst of MCP calls (each request logs a line) — and
  // when it does, the server blocks on its next write, which in turn
  // hangs the HTTP handlers. tee'ing to a file keeps the buffer from
  // being the bottleneck.
  const logPath = path.join(workdir, 'server.log');
  const logFd = fs.openSync(logPath, 'w');
  const proc = spawn(
    LAIN_BIN,
    [
      'server',
      '--config', configPath,
      '--workspace', 'biller-core',
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

async function waitForReady(baseUrl, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastErr = null;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${baseUrl}/health`);
      if (res.status === 200) {
        const body = await res.json().catch(() => null);
        if (body && body.federation &&
            Array.isArray(body.federation.repos) &&
            body.federation.repos.length >= 2 &&
            body.federation.repos.every(r => r.health === 'ready' || r.health === 'ok')) {
          return;
        }
      }
      lastErr = new Error(`status=${res.status}`);
    } catch (e) { lastErr = e; }
    await new Promise(r => setTimeout(r, 500));
  }
  throw new Error(`federation not ready within ${timeoutMs}ms: ${lastErr && lastErr.message}`);
}

// Federation cross-repo probe (Task 8 fix-2).
//
// After `waitForReady` confirms the federation is up, this calls the
// `get_workspace_graph` tool over MCP and asserts that the returned
// `nodes` array spans both repos in the federation fixture
// (`auth-svc` + `billing-svc`). The unfiltered workspace graph is the
// Tools-tab call the recording's brief is now keyed on (Option A from
// the Task 8 unblock) — the federation's per-repo ingest cannot resolve
// `Calls` edges across crate boundaries, so `get_cross_repo_blast_radius`
// always returned `by_repo={}` for `verify_token` against this fixture.
// `get_workspace_graph` reads the same backend but is an org-wide graph
// dump, so it visibly returns nodes from both repos on a healthy fixture.
async function probeFederationCrossRepoGraph(baseUrl, timeoutMs) {
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
          params: {
            name: 'get_workspace_graph',
            arguments: {},
          },
          id: 1,
        }),
      });
      if (!res.ok) {
        lastErr = new Error(`mcp HTTP ${res.status}`);
      } else {
        const env = await res.json();
        if (env.error) {
          lastErr = new Error(`jsonrpc error ${env.error.code}: ${env.error.message}`);
        } else if (env.result && env.result.isError) {
          const text = env.result.content && env.result.content[0] && env.result.content[0].text;
          lastErr = new Error(`tool error: ${text}`);
        } else {
          const text = env.result && env.result.content && env.result.content[0] && env.result.content[0].text;
          if (!text) {
            lastErr = new Error('tool returned empty content');
          } else {
            let payload;
            try { payload = JSON.parse(text); }
            catch (e) { lastErr = new Error(`tool payload not JSON: ${e.message}`); }
            if (payload) {
              const nodes = Array.isArray(payload.nodes) ? payload.nodes : [];
              const repos = new Set(nodes.map(n => n && n.repo_id).filter(Boolean));
              if (nodes.length > 0 && repos.has('auth-svc') && repos.has('billing-svc')) {
                const repoCounts = {};
                for (const n of nodes) {
                  if (!n || !n.repo_id) continue;
                  repoCounts[n.repo_id] = (repoCounts[n.repo_id] || 0) + 1;
                }
                console.log(`  federation probe OK: workspace_graph nodes=${nodes.length} repos=${Object.keys(repoCounts).sort().join(',')}`);
                return;
              }
              lastErr = new Error(
                `workspace graph missing cross-repo nodes — repos=${[...repos].sort().join(',') || '<none>'} node_count=${nodes.length}`,
              );
            }
          }
        }
      }
    } catch (e) { lastErr = e; }
    await new Promise(r => setTimeout(r, 500));
  }
  throw new Error(
    `federation probe failed: get_workspace_graph result doesn't span both repos — check fixture (last error: ${lastErr && lastErr.message})`,
  );
}

// ── Tab drive sequence (deterministic timings) ──────────────────────────

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

async function driveSequence(page) {
  // 1. Overview — let the federation health JSON render, then sit on it.
  await clickTab(page, 'overview');
  await page.waitForFunction(() => {
    const el = document.getElementById('tab-overview');
    return el && el.querySelector('pre, p, h3') !== null;
  }, { timeout: 15_000 });
  await new Promise(r => setTimeout(r, 4000));

  // 2. Repos — wait for the table to populate, then sit.
  await clickTab(page, 'repos');
  await page.waitForSelector('#tab-repos table.repo-table tbody tr', { timeout: 30_000 });
  await new Promise(r => setTimeout(r, 3000));

  // 3. Query — pick auth-svc, find Function, limit 50, run.
  await clickTab(page, 'query');
  await page.waitForSelector('#tab-query #query-repo', { timeout: 10_000 });
  await page.fill('#query-repo', 'auth-svc');
  await page.fill('#query-type', 'Function');
  await page.fill('#query-limit', '50');
  await page.click('#query-run');
  await page.waitForFunction(() => {
    const el = document.getElementById('query-output');
    return el && el.textContent && el.textContent.trim().length > 0 &&
           !/…/.test(el.textContent);
  }, { timeout: 15_000 });
  await new Promise(r => setTimeout(r, 4000));

  // 4. Tools — pick get_workspace_graph (Option A from Task 8 unblock).
  // The federation's `get_cross_repo_blast_radius` can't surface
  // cross-repo callers against the demo fixture (per-repo ingest drops
  // cross-crate `Calls` edges), so the recording is now keyed on the
  // workspace graph dump, which visibly returns nodes from both repos.
  await clickTab(page, 'tools');
  await page.waitForSelector('#tab-tools #tools-list li button', { timeout: 20_000 });
  // Find the right tool. The list buttons have the tool name as text.
  await page.evaluate(() => {
    const items = document.querySelectorAll('#tab-tools #tools-list li');
    for (const li of items) {
      const btn = li.querySelector('button');
      if (btn && /get_workspace_graph/.test(btn.textContent || '')) {
        btn.click();
        return;
      }
    }
    throw new Error('get_workspace_graph not in tools list');
  });
  await page.waitForSelector('#tab-tools #tool-args', { timeout: 10_000 });
  // The `filter?` field (note the literal `?` in the schema key — see
  // WORKSPACE_TOOL_DEFS in src/server/mcp/definitions.rs) is optional,
  // so we leave it empty for the unfiltered org-wide graph that the
  // recording's Tools tab is meant to display. The form-skip-empty
  // path in app.js sends `arguments: {}` and the handler reads
  // `args.get("filter")` (without the `?`), so the empty form is
  // equivalent to "no filter".
  await page.click('#tab-tools #tool-call');
  await page.waitForFunction(() => {
    const el = document.getElementById('tool-result');
    return el && el.textContent && el.textContent.trim().length > 0 &&
           !/…/.test(el.textContent);
  }, { timeout: 20_000 });
  await new Promise(r => setTimeout(r, 5000));

  // 5. Graph — let the D3 layout settle.
  await clickTab(page, 'graph');
  try {
    await page.waitForFunction(() => {
      const svg = document.getElementById('graph-canvas');
      return svg && svg.querySelectorAll('circle').length > 0;
    }, { timeout: 15_000 });
  } catch (_) {
    // Graph may not have data for this workspace — the empty-state
    // text is acceptable; recording still finishes.
  }
  await new Promise(r => setTimeout(r, 5000));
}

// ── Main ────────────────────────────────────────────────────────────────

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const workdir = args.workdir || fs.mkdtempSync(path.join(os.tmpdir(), 'lain-record-'));
  const baseUrl = `http://127.0.0.1:${args.port}`;
  const videoDir = path.dirname(args.out);
  fs.mkdirSync(videoDir, { recursive: true });

  console.log(`== lain SPA demo recorder ==`);
  console.log(`  binary:    ${LAIN_BIN}`);
  console.log(`  workdir:   ${workdir}`);
  console.log(`  port:      ${args.port}`);
  console.log(`  out:       ${args.out}`);

  if (!fs.existsSync(LAIN_BIN)) {
    console.error(`FATAL: lain binary not found at ${LAIN_BIN}`);
    process.exit(2);
  }
  if (!fs.existsSync(CHROMIUM_BIN)) {
    console.error(`FATAL: chromium binary not found at ${CHROMIUM_BIN}`);
    process.exit(2);
  }

  console.log(`  building fixture...`);
  buildFixture(workdir);

  console.log(`  starting server...`);
  const serverProc = startServer(workdir, args.port);

  let browser;
  let exitCode = 0;
  try {
    await waitForReady(baseUrl, 120_000);
    console.log(`  federation ready`);

    // Deterministic gate: confirm the cross-repo workspace-graph
    // payload the recording's Tools tab relies on actually spans both
    // repos. Catches a broken fixture (Task 1 → Task 8 bug) before we
    // burn a recording session on it. Now keyed on `get_workspace_graph`
    // after Option A in the Task 8 unblock — the federation cannot
    // surface cross-repo `Calls` edges, so the prior `by_repo`
    // assertion never fired.
    await probeFederationCrossRepoGraph(baseUrl, 30_000);
    console.log(`  federation probe passed`);

    browser = await chromium.launch({
      executablePath: CHROMIUM_BIN,
      args: ['--no-sandbox', '--disable-dev-shm-usage'],
      headless: true,
    });
    const context = await browser.newContext({
      viewport: { width: 1280, height: 800 },
      recordVideo: { dir: videoDir, size: { width: 1280, height: 800 } },
    });
    const page = await context.newPage();

    await page.goto(baseUrl + '/', { waitUntil: 'load', timeout: 30_000 });
    await page.waitForSelector('header.topbar h1', { timeout: 10_000 });

    await driveSequence(page);

    // Close page → video finalises. Then close context so the WebM
    // gets persisted to recordVideo.dir.
    const tempVideoPath = await page.video().path();
    await page.close();
    await context.close();
    await browser.close();
    browser = null;

    // Playwright writes the WebM to a hashed filename inside videoDir.
    // Move it to the requested --out path.
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
    if (serverProc._logPath) {
      console.log(`  server log: ${serverProc._logPath}`);
    }
    if (!process.env.RECORD_KEEP_DIR && !args.workdir) {
      try { fs.rmSync(workdir, { recursive: true, force: true }); } catch (_) {}
    } else {
      console.log(`  workdir preserved: ${workdir}`);
    }
  }
  process.exit(exitCode);
}

main().catch(e => { console.error(e); process.exit(1); });
