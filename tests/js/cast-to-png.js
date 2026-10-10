// Converts an asciinema v2 cast file to a sequence of PNG frames.
//
// Render path: cast → ANSI parser → character grid → ImageMagick annotate → PNG.
//
// Usage:
//   node tests/js/cast-to-png.js --cast /tmp/session.cast --out /tmp/frames/
//
// Exits 0 on success, non-zero on failure.

'use strict';

const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('path');

// ── CLI args ────────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const out = { cast: null, out: '/tmp/cast-frames', fps: 20 };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--cast')     out.cast = argv[++i];
    else if (a === '--out') out.out  = argv[++i];
    else if (a === '--fps') out.fps  = Number(argv[++i]);
    else { console.error(`unknown flag: ${a}`); process.exit(2); }
  }
  if (!out.cast) { console.error('--cast is required'); process.exit(2); }
  return out;
}

// ── ANSI colour → hex ──────────────────────────────────────────────────────

const ANSI_NAMES = [
  'black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white',
];

const COLOR_MAP = {
  default:       '#cccccc',
  black:         '#555555',
  red:           '#cc4444',
  green:         '#44cc44',
  yellow:        '#cccc44',
  blue:          '#4488cc',
  magenta:       '#cc44cc',
  cyan:          '#44cccc',
  white:         '#cccccc',
  bright_black:  '#888888',
  bright_red:    '#ff6666',
  bright_green:  '#66ff66',
  bright_yellow: '#ffff66',
  bright_blue:   '#66aaff',
  bright_magenta:'#ff66ff',
  bright_cyan:   '#66ffff',
  bright_white:  '#ffffff',
};

function hexColor(name) {
  if (name == null || name === 'default') return '#cccccc';
  if (COLOR_MAP[name]) return COLOR_MAP[name];
  if (name.startsWith('c256_')) {
    const n = parseInt(name.slice(5), 10);
    if (n < 16) return ['#000','#c00','#0c0','#cc0','#00c','#c0c','#0cc','#ccc',
                         '#555','#f55','#5f5','#ff5','#55f','#f5f','#5ff','#fff'][n] || '#ccc';
    if (n < 232) {
      const g = Math.floor(((n - 16) / 36) * 255);
      const r2 = ((n - 16) % 36) % 6;
      const b2 = ((n - 16) % 36) - r2 * 6;
      return `rgb(${Math.round(r2 * 255 / 5)},${Math.round(g)},${Math.round(b2 * 255 / 5)})`;
    }
    const v = Math.round(((n - 232) / 23) * 255);
    return `rgb(${v},${v},${v})`;
  }
  if (name.startsWith('crgb_')) {
    return 'rgb(' + name.slice(5).replace(/_/g, ',') + ')';
  }
  return '#cccccc';
}

// ── Cast parsing ───────────────────────────────────────────────────────────

function parseCast(castPath) {
  const raw = fs.readFileSync(castPath, 'utf8');
  const lines = raw.split('\n');
  const header = JSON.parse(lines[0]);
  const width  = header.width  || 80;
  const height = header.height || 24;
  const frames = [];

  for (let i = 1; i < lines.length; i++) {
    if (!lines[i].trim()) continue;
    try {
      const frame = JSON.parse(lines[i]);
      if (!Array.isArray(frame) || frame.length < 3) continue;
      const [t, type, data] = frame;
      if (type === 'o' && typeof data === 'string') {
        frames.push({ t, data });
      }
    } catch (_) {}
  }

  return { width, height, frames };
}

// ── ANSI → character grid ──────────────────────────────────────────────────

const ESC = '\x1b';
const MAX_ROWS = 2000;

// Known deviations from real terminal semantics, unexercised today
// (every pipeline cast is escape-free): erase-line ignores its parameter,
// so ESC[K/1K/2K all wipe the whole row; ED1 erases rows-above plus
// cursor→EOL where a real terminal erases BOL→cursor on the current row;
// and the OSC skip tests ST as `\`-then-ESC instead of ESC-then-`\`, so
// ST-terminated OSC can swallow text after it.
function buildGrid(frames) {
  // Grid: Array<Array<Cell>>, Cell = { char, fg, bg, bold }
  let grid = [];
  let cursor = { row: 0, col: 0 };
  let fg = 'default', bg = 'default', bold = false;

  function ensureRow(r) {
    while (grid.length <= r) grid.push(new Map());
  }

  function setCell(char) {
    if (char === ' ') char = '\u00a0'; // non-breaking space for blank cells
    ensureRow(cursor.row);
    grid[cursor.row].set(cursor.col, { char, fg, bg, bold });
    cursor.col++;
  }

  function eraseLine(toCol) {
    ensureRow(cursor.row);
    for (const c of grid[cursor.row].keys()) {
      if (toCol === undefined || c >= cursor.col) grid[cursor.row].delete(c);
    }
  }

  for (const { data } of frames) {
    let i = 0;
    while (i < data.length) {
      if (data[i] !== ESC) {
        let j = i;
        while (j < data.length && data[j] !== ESC) j++;
        for (const ch of data.slice(i, j)) {
          if (ch === '\r') { cursor.col = 0; }
          else if (ch === '\n') { cursor.row++; cursor.col = 0; }
          else if (ch === '\t') { cursor.col = Math.ceil((cursor.col + 1) / 8) * 8; }
          else if (ch === '\b') { cursor.col = Math.max(0, cursor.col - 1); }
          else if (ch === '\x07') { /* BEL — skip */ }
          else if (ch < ' ') { /* control char — skip */ }
          else setCell(ch);
        }
        i = j;
        continue;
      }

      if (data[i+1] === '[') {
        let j = i + 2;
        while (j < data.length && /[\x30-\x7e]/.test(data[j])) j++;
        const params = data.slice(i+2, j-1);
        const fin   = data[j-1] || '';
        const parts = params === '' ? [] : params.split(';').map(x => parseInt(x, 10) || 0);

        if (fin === 'm') {
          // SGR
          if (parts.length === 0 || parts[0] === 0) {
            fg = bg = 'default'; bold = false;
          }
          let k = 0;
          while (k < parts.length) {
            const p = parts[k];
            if      (p === 0)  { fg = bg = 'default'; bold = false; }
            else if (p === 1)  bold = true;
            else if (p === 22) bold = false;
            else if (p === 30) fg = ANSI_NAMES[0];
            else if (p === 31) fg = ANSI_NAMES[1];
            else if (p === 32) fg = ANSI_NAMES[2];
            else if (p === 33) fg = ANSI_NAMES[3];
            else if (p === 34) fg = ANSI_NAMES[4];
            else if (p === 35) fg = ANSI_NAMES[5];
            else if (p === 36) fg = ANSI_NAMES[6];
            else if (p === 37) fg = ANSI_NAMES[7];
            else if (p === 39) fg = 'default';
            else if (p === 90) fg = 'bright_' + ANSI_NAMES[0];
            else if (p === 91) fg = 'bright_' + ANSI_NAMES[1];
            else if (p === 92) fg = 'bright_' + ANSI_NAMES[2];
            else if (p === 93) fg = 'bright_' + ANSI_NAMES[3];
            else if (p === 94) fg = 'bright_' + ANSI_NAMES[4];
            else if (p === 95) fg = 'bright_' + ANSI_NAMES[5];
            else if (p === 96) fg = 'bright_' + ANSI_NAMES[6];
            else if (p === 97) fg = 'bright_' + ANSI_NAMES[7];
            else if (p === 38) {
              // Extended fg: 38;5;n or 38;2;r;g;b
              k++;
              const type = parts[k] || 0;
              if (type === 5) { k++; fg = `c256_${parts[k] || 0}`; }
              else if (type === 2) { k++; const r2 = parts[k++]||0, g = parts[k++]||0, b = parts[k++]||0; fg = `crgb_${r2}_${g}_${b}`; }
            }
            k++;
          }
        } else if (fin === 'H' || fin === 'f') {
          cursor.row = Math.max(0, (parts[0] || 1) - 1);
          cursor.col = Math.max(0, (parts[1] || 1) - 1);
        } else if (fin === 'J') {
          const mode = parts[0] || 0;
          if      (mode === 0) { for (let r = cursor.row + 1; r < grid.length; r++) grid[r] = new Map(); }
          else if (mode === 1) { for (let r = 0; r < cursor.row; r++) grid[r] = new Map(); eraseLine(0); }
          else if (mode >= 2)  { grid = []; }
        } else if (fin === 'K') {
          eraseLine();
        } else if (fin === 'A') { cursor.row = Math.max(0, cursor.row - (parts[0] || 1)); }
        else if (fin === 'B')   { cursor.row += (parts[0] || 1); }
        else if (fin === 'C')   { cursor.col += (parts[0] || 1); }
        else if (fin === 'D')   { cursor.col = Math.max(0, cursor.col - (parts[0] || 1)); }
        else if (fin === 's')   { /* save cursor — skip */ }
        else if (fin === 'u')   { /* restore cursor — skip */ }
        i = j;
      } else if (data[i+1] === ']') {
        // OSC — skip to BEL or ST
        let j = i + 2;
        while (j < data.length && data[j] !== '\x07' && !(data[j] === '\\' && data[j+1] === ESC)) j++;
        i = j + 1;
      } else {
        i++;
      }
    }
  }
  return grid;
}

// ── Render grid → PNG via ImageMagick annotate ──────────────────────────────

const FONT      = '/usr/share/fonts/truetype/LiberationMono-Regular.ttf';
const FONT_BOLD = '/usr/share/fonts/truetype/LiberationMono-Bold.ttf';
const CELL_W    = 9;   // pixel width per cell
const CELL_H    = 18;  // pixel height per line
const PAD_X     = 16;  // left padding
const PAD_Y     = 16;  // top padding
const BG_COLOR  = '#0d1117'; // dark terminal bg — no quotes for IM

function runMagick(args) {
  return new Promise((resolve, reject) => {
    const proc = spawn('magick', args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stderr = '';
    proc.stderr.on('data', d => { stderr += d; });
    proc.on('close', (code) => {
      if (code !== 0) reject(new Error(`magick exit ${code}: ${stderr.slice(0, 200)}`));
      else resolve();
    });
    proc.on('error', err => reject(err));
  });
}

function escapeIm(s) {
  // No shell is involved (spawn not shell), so no escaping needed.
  // Just trim whitespace for cleanliness.
  return s;
}

async function renderGrid(grid, outPath, castWidth, castHeight) {
  const rows = grid.length || 0;
  const W = castWidth * CELL_W + PAD_X * 2;
  const H = Math.max(rows, castHeight) * CELL_H + PAD_Y * 2 + 4;
  const baseline = PAD_Y + CELL_H - 4; // baseline for first line

  // Build magick arguments: background + per-line color-run annotations
  const args = ['-size', `${W}x${H}`, `xc:${BG_COLOR}`];

  for (let r = 0; r < rows; r++) {
    const row = grid[r];
    if (row.size === 0) continue;

    const y = baseline + r * CELL_H;
    let c = 0;
    while (c < castWidth) {
      // Check if there's a cell at this position
      const cell = row.get(c) || null;
      if (!cell) { c++; continue; }

      // Scan forward for run of same style
      let runStart = c;
      const style = { fg: cell.fg, bold: cell.bold };
      while (c + 1 < castWidth) {
        const nextCell = row.get(c + 1);
        if (!nextCell || nextCell.fg !== style.fg || nextCell.bold !== style.bold) break;
        c++;
      }
      const runEnd = c; // inclusive

      // Collect characters for this run
      const chars = [];
      for (let ci = runStart; ci <= runEnd; ci++) {
        const c2 = row.get(ci);
        chars.push(c2 ? c2.char.replace(/\u00a0/g, ' ') : ' ');
      }
      const text = chars.join('').replace(/\s+$/, ''); // rtrim spaces
      if (text.length > 0) {
        const x = PAD_X + runStart * CELL_W;
        const fg_col = hexColor(style.fg);
        const font  = style.bold ? FONT_BOLD : FONT;
        args.push('-fill', fg_col, '-font', font, '-pointsize', '14',
                  '-gravity', 'NorthWest', '-annotate',
                  `+${x}+${y}`, text);
      }
      c++;
    }
  }

  args.push(outPath);
  await runMagick(args);
}

// ── Key-frame selection ─────────────────────────────────────────────────────
//
// Emit frames at a fixed interval (1/fps) across the FULL cast duration,
// including trailing silence after the last output event. Each sample
// renders the terminal state accumulated from the output events elapsed at
// that sample's time, so early frames show early output and late frames the
// full session. Every interval gets a frame (no deduplication) so the MP4
// duration tracks the cast.
//
// This replaces the old output-event-driven approach which produced very few
// frames for fast-executing commands.

const TRAILING_SECONDS = 3; // how long to hold the final state on screen

function selectKeyFrames(frames, fps) {
  if (frames.length === 0) return [];
  const interval = 1 / fps;

  // Determine the last event timestamp; add trailing silence so the video
  // holds the final state for a few seconds.
  const lastEventT = frames[frames.length - 1].t;
  const endT = lastEventT + TRAILING_SECONDS;

  const result = [];
  for (let t = 0; t <= endT + interval / 2; t += interval) {
    let k = 0;
    while (k < frames.length && frames[k].t <= t) k++;
    result.push({ t, grid: buildGrid(frames.slice(0, k)) });
  }

  return result;
}

// ── Main ──────────────────────────────────────────────────────────────────

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const outDir = args.out;

  if (!fs.existsSync(args.cast)) {
    console.error(`FATAL: cast file not found: ${args.cast}`); process.exit(1);
  }

  fs.mkdirSync(outDir, { recursive: true });
  console.log(`== cast-to-png ==`);
  console.log(`  cast: ${args.cast}`);
  console.log(`  out:  ${outDir}`);

  const cast = parseCast(args.cast);
  console.log(`  cast: ${cast.frames.length} frames, ${cast.width}x${cast.height}`);

  const keyFrames = selectKeyFrames(cast.frames, args.fps);
  console.log(`  key frames: ${keyFrames.length}`);

  let ok = 0, fail = 0;
  for (let i = 0; i < keyFrames.length; i++) {
    const { t, grid } = keyFrames[i];
    process.stdout.write(`  frame ${i+1}/${keyFrames.length} (t=${t.toFixed(2)}s) … `);
    const outPath = path.join(outDir, `frame_${String(i+1).padStart(6,'0')}.png`);
    try {
      await renderGrid(grid, outPath, cast.width, cast.height);
      const sz = fs.statSync(outPath).size;
      process.stdout.write(`ok (${sz}B)\n`);
      ok++;
    } catch (e) {
      process.stdout.write(`FAIL: ${e.message}\n`);
      fail++;
    }
  }

  console.log(`\n  rendered: ${ok} ok, ${fail} failed`);
  const pngs = fs.readdirSync(outDir).filter(f => f.endsWith('.png')).sort();
  console.log(`  PNGs in output: ${pngs.length}`);
  if (pngs.length === 0) { console.error('FATAL: no PNG frames produced'); process.exit(1); }
  if (fail > ok) { console.error('FATAL: too many render failures'); process.exit(1); }
  process.exit(0);
}

if (typeof module !== 'undefined' && module.exports) {
  module.exports = { parseArgs, parseCast, buildGrid, selectKeyFrames, renderGrid };
}

if (require.main === module) {
  main().catch(e => { console.error(e); process.exit(1); });
}
