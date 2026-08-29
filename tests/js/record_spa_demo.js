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

  // 4. Tools — pick get_cross_repo_blast_radius, run with verify_token.
  await clickTab(page, 'tools');
  await page.waitForSelector('#tab-tools #tools-list li button', { timeout: 20_000 });
  // Find the right tool. The list buttons have the tool name as text.
  await page.evaluate(() => {
    const items = document.querySelectorAll('#tab-tools #tools-list li');
    for (const li of items) {
      const btn = li.querySelector('button');
      if (btn && /get_cross_repo_blast_radius/.test(btn.textContent || '')) {
        btn.click();
        return;
      }
    }
    throw new Error('get_cross_repo_blast_radius not in tools list');
  });
  await page.waitForSelector('#tab-tools #tool-args', { timeout: 10_000 });
  // Fill the args form. `name` attributes mirror the tool's inputSchema keys.
  await page.fill('#tab-tools #tool-args input[name="symbol"]', 'verify_token');
  await page.fill('#tab-tools #tool-args input[name="depth"]', '1..3');
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
