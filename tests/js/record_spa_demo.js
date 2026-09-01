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
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const LAIN_BIN = process.env.LAIN_BIN
  || path.resolve(__dirname, '..', '..', 'target', 'release', 'lain');
const CHROMIUM_BIN = '/usr/bin/chromium';

// ── CLI args ────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const out = {
    out: '/tmp/lain-spa-demo/raw.webm',
    port: 9931,
    workdir: null,
    // Default matches the workspace name written by the real fixture
    // (`scripts/demo-federation-fixture.sh` → `name: tokio-stack`).
    // Pass `--workspace biller-core` when running against the legacy
    // synthetic fixture (`scripts/legacy/demo-federation-fixture.sh`).
    workspace: 'tokio-stack',
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--out')          out.out       = argv[++i];
    else if (a === '--port')    out.port      = Number(argv[++i]);
    else if (a === '--workdir') out.workdir   = argv[++i];
    else if (a === '--workspace') out.workspace = argv[++i];
    else { console.error(`unknown flag: ${a}`); process.exit(2); }
  }
  return out;
}

// ── Lifecycle helpers ───────────────────────────────────────────────────

function startServer(workdir, port, workspace) {
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

// Federation cross-repo probe.
//
// After `waitForReady` confirms the federation is up, this calls the
// `get_workspace_graph` tool over MCP and asserts that the returned
// `nodes` array spans at least the federation's declared repo set.
// We read the expected repo IDs from `repos.yaml` so the probe is
// keyed to whatever fixture is loaded (real OSS = bytes+tokio,
// synthetic = auth-svc+billing-svc, future fixtures = TBD). The
// unfiltered workspace graph is the most reliable cross-repo probe
// we have — the federation's per-repo ingest cannot resolve `Calls`
// edges across crate boundaries, so `get_cross_repo_blast_radius`
// always returns `by_repo={}` against this fixture and is not usable
// as an upstream gate.
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
              const missing = expectedRepos.filter(r => !repos.has(r));
              if (nodes.length > 0 && missing.length === 0) {
                const repoCounts = {};
                for (const n of nodes) {
                  if (!n || !n.repo_id) continue;
                  repoCounts[n.repo_id] = (repoCounts[n.repo_id] || 0) + 1;
                }
                console.log(`  federation probe OK: workspace_graph nodes=${nodes.length} repos=${Object.keys(repoCounts).sort().join(',')}`);
                return;
              }
              lastErr = new Error(
                `workspace graph missing cross-repo nodes — repos=${[...repos].sort().join(',') || '<none>'} node_count=${nodes.length} missing=${missing.join(',') || '<none>'}`,
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

// Pull the list of repo IDs from the workdir's `repos.yaml`. The probe
// keys off this list so the recording stays fixture-agnostic (real
// OSS vs synthetic vs whatever a future fixture introduces). Falls
// back to an empty list if the file is missing or unparseable; the
// probe's `missing.length === 0` check then trivially passes, which is
// the right behaviour for an "I can't tell" situation.
function readRepoIdsFromConfig(workdir) {
  const configPath = path.join(workdir, 'repos.yaml');
  try {
    const text = fs.readFileSync(configPath, 'utf8');
    const cfg = JSON.parse(text); // tolerate JSON for unit tests
    if (Array.isArray(cfg.repos)) {
      return cfg.repos.map(r => r && r.id).filter(Boolean);
    }
    return [];
  } catch (_) {
    // `repos.yaml` is YAML, not JSON — fall back to a tiny regex pass
    // that grabs `- id: <name>` lines. Good enough for the probe; we
    // don't need full schema fidelity here.
    try {
      const text = fs.readFileSync(configPath, 'utf8');
      const ids = [];
      const re = /^\s*-\s*id:\s*([A-Za-z0-9_.-]+)/gm;
      let m;
      while ((m = re.exec(text)) !== null) ids.push(m[1]);
      return ids;
    } catch (__) {
      return [];
    }
  }
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

  // 2. Repos — wait for the table to populate with both `bytes` and `tokio`
  //    reading `ready`, then sit.
  await clickTab(page, 'repos');
  await page.waitForFunction(() => {
    const rows = document.querySelectorAll('#tab-repos table.repo-table tbody tr');
    if (rows.length < 2) return false;
    const ids = Array.from(rows).map(r => (r.textContent || '').toLowerCase());
    const readyCount = Array.from(rows).filter(r => /ready/.test(r.textContent || '')).length;
    return ids.some(t => t.includes('bytes'))
        && ids.some(t => t.includes('tokio'))
        && readyCount >= 2;
  }, { timeout: 60_000 });
  await new Promise(r => setTimeout(r, 4000));

  // 3. Query — pick tokio (the larger repo), find Function, limit 50, run.
  await clickTab(page, 'query');
  await page.waitForSelector('#tab-query #query-repo', { timeout: 10_000 });
  await page.fill('#query-repo', 'tokio');
  await page.fill('#query-type', 'Function');
  await page.fill('#query-limit', '50');
  await page.click('#query-run');
  await page.waitForFunction(() => {
    const el = document.getElementById('query-output');
    return el && el.textContent && el.textContent.trim().length > 0 &&
           !/…/.test(el.textContent);
  }, { timeout: 15_000 });
  await new Promise(r => setTimeout(r, 6000));

  // 4. Tools — pick the top anchor from the bytes repo at boot, then
  // call the unambiguous cross-repo tool against it. The anchor lookup
  // is repo-scoped (we asked find_anchors for `bytes`), so use
  // `get_cross_repo_blast_radius_for_repo` to bypass federation-wide
  // symbol resolution — that would otherwise error with
  // AmbiguousSymbol when the name lives in both `bytes` and `tokio`
  // (e.g. `Buf`). Fall back to the unscoped tool with literal `Bytes`
  // if find_anchors returned nothing.
  const crossRepoSymbol = await page.evaluate(async () => {
    try {
      const r = await fetch('/mcp', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({
          jsonrpc: '2.0', id: 1, method: 'tools/call',
          params: { name: 'find_anchors', arguments: { repo_id: 'bytes', limit: 10 } },
        }),
      });
      const body = await r.json();
      const text = (body && body.result && body.result.content
        && body.result.content[0] && body.result.content[0].text) || '';
      // find_anchors emits a numbered list, e.g. "1. Bytes\n2. Buf\n...".
      const m = text.match(/^\s*1\.\s+([A-Za-z_][A-Za-z0-9_]*)/m);
      return m ? m[1] : null;
    } catch (_) {
      return null;
    }
  }) || 'Bytes';
  if (crossRepoSymbol === 'Bytes') {
    process.stderr.write('WARN: find_anchors returned no bytes anchor; falling back to literal "Bytes"\n');
  }
  const usesForRepo = crossRepoSymbol !== 'Bytes';
  const toolName = usesForRepo
    ? 'get_cross_repo_blast_radius_for_repo'
    : 'get_cross_repo_blast_radius';

  await clickTab(page, 'tools');
  await page.waitForSelector('#tab-tools #tools-list li button', { timeout: 20_000 });
  // Find the right tool. The list buttons render the tool name as
  // text, so match exactly — `get_cross_repo_blast_radius_for_repo`
  // shares a prefix with the unscoped variant and a substring regex
  // would either click the wrong button or hit whichever is rendered
  // first.
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

  if (usesForRepo) {
    await page.fill('#tab-tools #tool-args input[name="repo_id"]', 'bytes');
  }
  await page.fill('#tab-tools #tool-args input[name="symbol"]', crossRepoSymbol);
  await page.fill('#tab-tools #tool-args input[name="depth"]', '1..3');
  await page.click('#tab-tools #tool-call');
  await page.waitForFunction(() => {
    const el = document.getElementById('tool-result');
    return el && el.textContent && el.textContent.trim().length > 0 &&
           !/…/.test(el.textContent);
  }, { timeout: 30_000 });
  await new Promise(r => setTimeout(r, 6000));

  // 5. Graph — let the D3 layout settle. Upgraded for the new
  // shape-per-kind rendering: nodes are <path class="graph-node"> now,
  // not <circle>.
  await clickTab(page, 'graph');
  try {
    await page.waitForFunction(() => {
      const svg = document.getElementById('graph-canvas');
      return svg && svg.querySelectorAll('path.graph-node').length > 0;
    }, { timeout: 15_000 });
  } catch (_) {
    // Graph may not have data — the empty-state text is acceptable.
  }
  await new Promise(r => setTimeout(r, 6000));   // +0 s vs v1's 10s; small graph
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

  // The orchestrator (`scripts/record-spa-demo.sh`) is responsible for
  // running the fixture script before invoking this driver — either by
  // calling `scripts/demo-federation-fixture.sh` itself or under
  // `--no-clone` against an already-populated workdir. Guard against a
  // caller that forgot to populate the workdir so the failure surfaces
  // here as a clear error instead of a confusing server-start failure
  // ("workspace X not found in workspaces.yaml") downstream.
  const configPath = path.join(workdir, 'repos.yaml');
  if (!fs.existsSync(configPath)) {
    console.error(`FATAL: ${configPath} missing — populate workdir first (orchestrator's fixture step or --no-clone against an existing tree)`);
    process.exit(2);
  }

  console.log(`  starting server...`);
  const serverProc = startServer(workdir, args.port, args.workspace);

  let browser;
  let exitCode = 0;
  try {
    // Bumped from 120_000 → 600_000 (Task 7 fix round 1): cold-cache
    // federation reindex of bytes+tokio routinely takes longer than 2
    // minutes (tokio alone spawned proc-macro servers for ~80 s on the
    // last failed run before the recorder gave up). The cap exists only
    // on the recording path; production server startup is unaffected.
    await waitForReady(baseUrl, 600_000);
    console.log(`  federation ready`);

    // Deterministic gate: confirm the cross-repo workspace-graph
    // payload spans every repo declared in the fixture's
    // `repos.yaml`. Catches a broken fixture before we burn a
    // recording session on it.
    await probeFederationCrossRepoGraph(baseUrl, workdir, 30_000);
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
    if (!process.env.LAIN_RECORD_KEEP_DIR && !args.workdir) {
      try { fs.rmSync(workdir, { recursive: true, force: true }); } catch (_) {}
    } else {
      console.log(`  workdir preserved: ${workdir}`);
    }
  }
  process.exit(exitCode);
}

main().catch(e => { console.error(e); process.exit(1); });
