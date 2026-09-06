/* UGo Mobile 原型逻辑
 * Rust 引擎（ugo_core.wasm）负责规则：落子 / 提子 / 打劫 / 自杀 / 计分。
 * 前端只做渲染与交互，通过导出的 C ABI 直接读写引擎内存。
 */
'use strict';

const EMPTY = 0, BLACK = 1, WHITE = 2;
const ERR_MSG = {
  [-1]: '此处已有棋子',
  [-3]: '禁着点：不能自杀',
  [-4]: '打劫：需先在别处落子',
};

const state = {
  size: 19,
  rule: 0, // 0 中国 1 日本 2 韩国
  labels: true,
  moveDisplay: 'numbers', // numbers | circle | off
  finished: false,
  mode: 'play',           // play | analysis（分析=试下）
  placeColor: 'alt',      // 分析摆放：1 黑 | 2 白 | alt 交替
  nextPlace: BLACK,       // 交替摆放的下一色
  showInfluence: false,   // 形势显示
  ai: { on: false, color: WHITE, level: 5, style: 'balanced' },
  aiThinking: false,
};

// 行棋风格 → 引擎权重（攻杀 / 防守 / 实地 / 外势，0..100）
const AI_STYLES = {
  balanced:  { atk: 50, def: 50, ter: 50, moyo: 50 },
  attack:    { atk: 90, def: 30, ter: 35, moyo: 60 },
  defend:    { atk: 25, def: 90, ter: 55, moyo: 45 },
  territory: { atk: 40, def: 50, ter: 90, moyo: 25 },
  moyo:      { atk: 55, def: 60, ter: 25, moyo: 85 },
};
const AI_LEVEL_NAMES = ['入门', '入门', '初级', '初级', '中级', '中级', '进阶', '高级', '高手', '专家'];

let wasm = null;
let boardView = null;   // Uint8Array：棋盘
let moveNoView = null;  // Int32Array：每点手数（摆放子为负数）
let inflView = null;    // Int32Array：净影响力（黑正白负）
let terrView = null;    // Int8Array：分类图 ±2 确定地 / ±1 势力 / 0 中立

const $ = (id) => document.getElementById(id);

/* ---------- 引擎加载 ---------- */
// ?v= 用于更新引擎后绕过 WebView 缓存
async function loadEngine() {
  const res = await fetch('wasm/ugo_core.wasm?v=11');
  const { instance } = await WebAssembly.instantiateStreaming(res);
  wasm = instance.exports;
}

function applyAiConfig() {
  const w = AI_STYLES[state.ai.style] || AI_STYLES.balanced;
  wasm.ugo_ai_config(state.ai.on ? state.ai.level : 0, w.atk, w.def, w.ter, w.moyo);
}

function newGame(silent) {
  // 若在分析模式中，先丢弃试下恢复原局
  if (state.mode === 'analysis') {
    wasm.ugo_snapshot_pop();
    state.mode = 'play';
    $('analysis-bar').classList.add('hidden');
    const btn = $('btn-mode');
    btn.textContent = '分析';
    btn.classList.remove('active');
  }
  wasm.ugo_new(state.size, state.rule);
  const n = state.size * state.size;
  boardView = new Uint8Array(wasm.memory.buffer, wasm.ugo_board_ptr(), n);
  moveNoView = new Int32Array(wasm.memory.buffer, wasm.ugo_moveno_ptr(), n);
  inflView = new Int32Array(wasm.memory.buffer, wasm.ugo_influence_ptr(), n);
  terrView = new Int8Array(wasm.memory.buffer, wasm.ugo_terr_ptr(), n);
  state.finished = false;
  state.nextPlace = BLACK;
  markers.clear();
  state.aiThinking = false;
  aiStats.total = 0;
  aiStats.count = 0;
  clearTimeout(aiTimer);
  applyAiConfig();
  refresh();
  if (!silent) toast(`新对局：${state.size} 路`);
  aiMaybeMove();
}

/// 盘面有变后统一刷新：按需重算形势 → 重绘 → 更新状态栏 / 形势条
function refresh() {
  if (state.showInfluence) wasm.ugo_analyze(0, 0);
  render();
  updateStatus();
  updateEstBar();
}

/* ---------- 形势估算条 ---------- */
function updateEstBar() {
  const bar = $('est-bar');
  if (!state.showInfluence) {
    bar.classList.add('hidden');
    return;
  }
  bar.classList.remove('hidden');
  const eb = wasm.ugo_est_black(), ew = wasm.ugo_est_white();
  const komi = state.rule === 0 ? 7.5 : 6.5;
  const lead = eb - ew - komi; // 黑视角
  $('est-b').textContent = eb;
  $('est-w').textContent = ew;
  const total = Math.max(1, eb + ew);
  $('est-fill').style.width = Math.min(100, Math.max(0, eb / total * 100)) + '%';
  const fmt = (n) => (Math.abs(n) + 0.001).toFixed(1).replace(/\.0$/, '');
  $('est-lead').textContent = state.finished ? '对局结束'
    : lead >= 0 ? `黑领先 ${fmt(lead)} 目` : `白领先 ${fmt(-lead)} 目`;
}

/* ---------- 棋盘绘制 ---------- */
const canvas = $('board');
const ctx = canvas.getContext('2d');
let geom = { cell: 0, margin: 0, px: 0 };

function layoutCanvas() {
  const wrap = $('board-wrap');
  const avail = Math.min(wrap.clientWidth, wrap.clientHeight) - 16;
  const dpr = window.devicePixelRatio || 1;
  const px = Math.max(240, Math.floor(avail));
  canvas.style.width = px + 'px';
  canvas.style.height = px + 'px';
  canvas.width = px * dpr;
  canvas.height = px * dpr;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

  // 先按估算边距求出格距，再保证边距至少能容纳整颗棋子（否则边上的子会被裁切）
  let margin = px * (state.labels ? 0.06 : 0.05);
  margin = Math.max(margin, ((px - margin * 2) / (state.size - 1)) * 0.47 * 1.12);
  const cell = (px - margin * 2) / (state.size - 1);
  geom = { cell, margin, px };
}

function gridX(i) { return geom.margin + i * geom.cell; }

function starPoints(size) {
  if (size === 9) return [[2, 2], [6, 2], [4, 4], [2, 6], [6, 6]];
  if (size === 13) return [[3, 3], [9, 3], [6, 6], [3, 9], [9, 9]];
  // 19 路：9 个标准星位
  const p = [3, 9, 15];
  const pts = [];
  for (const x of p) for (const y of p) pts.push([x, y]);
  return pts;
}

function drawStone(x, y, color, radius) {
  const cx = gridX(x), cy = gridX(y);
  const g = ctx.createRadialGradient(cx - radius * 0.35, cy - radius * 0.35, radius * 0.1, cx, cy, radius);
  ctx.save();
  ctx.shadowColor = 'rgba(45, 22, 5, 0.4)';
  ctx.shadowBlur = radius * 0.3;
  ctx.shadowOffsetY = radius * 0.14;
  if (color === BLACK) {
    g.addColorStop(0, '#5a5a5a'); g.addColorStop(1, '#000');
  } else {
    g.addColorStop(0, '#ffffff'); g.addColorStop(1, '#b8b8b8');
  }
  ctx.beginPath();
  ctx.arc(cx, cy, radius, 0, Math.PI * 2);
  ctx.fillStyle = g;
  ctx.fill();
  ctx.restore();
}

function render() {
  layoutCanvas();
  const { cell, margin, px } = geom;
  const s = state.size;
  const stoneR = cell * 0.47;

  // 棋盘木纹底（纹理用固定种子，重绘保持稳定）
  const bg = ctx.createLinearGradient(0, 0, px * 0.25, px);
  bg.addColorStop(0, '#e2b66c');
  bg.addColorStop(0.55, '#d6a75c');
  bg.addColorStop(1, '#c7954b');
  ctx.fillStyle = bg;
  ctx.fillRect(0, 0, px, px);

  // 木纹：垂直微波纹路
  let grainSeed = 20240918;
  const rand = () => {
    grainSeed = (grainSeed * 1664525 + 1013904223) >>> 0;
    return grainSeed / 4294967296;
  };
  ctx.lineWidth = 1;
  for (let i = 0; i < 34; i++) {
    const gx = rand() * px;
    const amp = 2 + rand() * 5;
    const period = 60 + rand() * 130;
    const phase = rand() * Math.PI * 2;
    ctx.strokeStyle = `rgba(140, 96, 40, ${0.05 + rand() * 0.07})`;
    ctx.beginPath();
    for (let yy = 0; yy <= px; yy += 8) {
      const xx = gx + Math.sin((yy / period) * Math.PI * 2 + phase) * amp;
      if (yy === 0) ctx.moveTo(xx, yy); else ctx.lineTo(xx, yy);
    }
    ctx.stroke();
  }
  // 边缘暗角
  const vig = ctx.createRadialGradient(px / 2, px / 2, px * 0.35, px / 2, px / 2, px * 0.78);
  vig.addColorStop(0, 'rgba(60, 35, 10, 0)');
  vig.addColorStop(1, 'rgba(60, 35, 10, 0.20)');
  ctx.fillStyle = vig;
  ctx.fillRect(0, 0, px, px);

  // 边框 + 网格
  ctx.strokeStyle = '#4a3820';
  ctx.lineWidth = 1;
  const lo = gridX(0), hi = gridX(s - 1);
  ctx.strokeRect(lo, lo, hi - lo, hi - lo);
  ctx.beginPath();
  for (let i = 1; i < s - 1; i++) {
    const p = gridX(i);
    ctx.moveTo(lo, p); ctx.lineTo(hi, p);
    ctx.moveTo(p, lo); ctx.lineTo(p, hi);
  }
  ctx.stroke();

  // 星位
  ctx.fillStyle = '#4a3820';
  for (const [x, y] of starPoints(s)) {
    ctx.beginPath();
    ctx.arc(gridX(x), gridX(y), Math.max(2, cell * 0.09), 0, Math.PI * 2);
    ctx.fill();
  }

  // 边缘标号（列 A-T 跳过 I，行号自下而上）
  if (state.labels) {
    ctx.fillStyle = 'rgba(74, 56, 32, 0.8)';
    ctx.font = `${Math.max(9, cell * 0.42)}px sans-serif`;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    const letters = 'ABCDEFGHJKLMNOPQRSTUVWXYZ';
    for (let i = 0; i < s; i++) {
      const p = gridX(i);
      ctx.fillText(letters[i], p, px - margin * 0.42);
      ctx.fillText(String(s - i), margin * 0.42, p);
    }
  }

  // 棋子 + 落子显示
  const lastX = wasm.ugo_last_x(), lastY = wasm.ugo_last_y();
  const labelFont = Math.max(9, cell * 0.46);
  for (let y = 0; y < s; y++) {
    for (let x = 0; x < s; x++) {
      const idx = y * state.size + x;
      const v = boardView[idx];
      if (v === EMPTY) continue;
      drawStone(x, y, v, stoneR);
      if (state.moveDisplay === 'numbers') {
        const n = moveNoView[idx];
        if (n !== 0) {
          ctx.fillStyle = v === BLACK ? '#fff' : '#222';
          ctx.font = `600 ${labelFont}px sans-serif`;
          ctx.textAlign = 'center';
          ctx.textBaseline = 'middle';
          ctx.fillText(String(Math.abs(n)), gridX(x), gridX(y) + 0.5);
        }
      }
    }
  }

  // 形势分析（泛洪辐射）：势力晕染 → 确定地圆点（画在棋子下层），
  // 死棋子上方标对方领地点
  if (state.showInfluence && terrView) {
    const dotR = Math.max(2, cell * 0.17);
    const washR = cell * 0.5;
    // 第一层：势力/模样晕染（±1）——半透明圆角方块，成片铺出形势感
    for (let y = 0; y < s; y++) {
      for (let x = 0; x < s; x++) {
        const idx = y * s + x;
        if (boardView[idx] !== EMPTY) continue;
        const t = terrView[idx];
        if (t === 1 || t === -1) {
          ctx.beginPath();
          if (ctx.roundRect) {
            ctx.roundRect(gridX(x) - washR, gridX(y) - washR, washR * 2, washR * 2, washR * 0.3);
          } else {
            ctx.arc(gridX(x), gridX(y), washR * 0.85, 0, Math.PI * 2);
          }
          ctx.fillStyle = t === 1 ? 'rgba(25, 20, 12, 0.13)' : 'rgba(255, 252, 244, 0.22)';
          ctx.fill();
        }
      }
    }
    // 第二层：确定地圆点（±2）
    for (let y = 0; y < s; y++) {
      for (let x = 0; x < s; x++) {
        const idx = y * s + x;
        if (boardView[idx] !== EMPTY) continue;
        const t = terrView[idx];
        if (t !== 2 && t !== -2) continue;
        ctx.beginPath();
        ctx.arc(gridX(x), gridX(y), dotR, 0, Math.PI * 2);
        if (t === 2) {
          ctx.fillStyle = 'rgba(15,15,15,0.85)';
          ctx.fill();
        } else {
          ctx.fillStyle = 'rgba(255,255,255,0.95)';
          ctx.fill();
          ctx.strokeStyle = 'rgba(90,70,40,0.55)';
          ctx.lineWidth = 1;
          ctx.stroke();
        }
      }
    }
    // 第三层：死棋子标记（棋子势力与子色相反 → 该子被判死）
    for (let y = 0; y < s; y++) {
      for (let x = 0; x < s; x++) {
        const idx = y * s + x;
        const stone = boardView[idx];
        if (stone === EMPTY) continue;
        const t = terrView[idx];
        const dead = (stone === BLACK && t === -2) || (stone === WHITE && t === 2);
        if (!dead) continue;
        ctx.beginPath();
        ctx.arc(gridX(x), gridX(y), dotR, 0, Math.PI * 2);
        if (stone === BLACK) {
          ctx.fillStyle = 'rgba(255,255,255,0.95)';
          ctx.fill();
          ctx.strokeStyle = 'rgba(90,70,40,0.55)';
          ctx.lineWidth = 1;
          ctx.stroke();
        } else {
          ctx.fillStyle = 'rgba(15,15,15,0.85)';
          ctx.fill();
        }
      }
    }
  }

  // 分析记号（红圈，最上层）
  for (const idx of markers) {
    const mx = idx % s, my = (idx / s) | 0;
    ctx.beginPath();
    ctx.arc(gridX(mx), gridX(my), cell * 0.38, 0, Math.PI * 2);
    ctx.strokeStyle = '#c8503c';
    ctx.lineWidth = Math.max(1.5, cell * 0.09);
    ctx.stroke();
  }

  // 最后一手的圆圈标记
  if (state.moveDisplay === 'circle' && lastX >= 0) {
    const v = boardView[lastY * state.size + lastX];
    ctx.beginPath();
    ctx.arc(gridX(lastX), gridX(lastY), stoneR * 0.55, 0, Math.PI * 2);
    ctx.strokeStyle = v === BLACK ? '#fff' : '#c8503c';
    ctx.lineWidth = Math.max(1.5, cell * 0.09);
    ctx.stroke();
  }
}

/* ---------- 状态栏 ---------- */
function updateStatus() {
  const turn = wasm.ugo_turn();
  $('turn-stone').className = 'stone-icon ' + (turn === BLACK ? 'black' : 'white');
  const aiTurn = state.ai.on && turn === state.ai.color && !state.finished && state.mode === 'play';
  const who = (turn === BLACK ? '黑方' : '白方') + (aiTurn ? '(AI)' : '');
  $('turn-text').textContent = state.finished ? '对局结束'
    : state.mode === 'analysis' ? '试下·' + who
    : state.aiThinking ? 'AI 思考中…'
    : who + '行棋';
  $('move-info').textContent = `第 ${wasm.ugo_move_count()} 手 · 黑提${wasm.ugo_captured_black()} 白提${wasm.ugo_captured_white()}`;
}

/* ---------- AI 对手 ---------- */
let aiTimer = null;
const aiStats = { total: 0, count: 0 };

// 播报条可见性：开启机器对手时显示
function updateAiInfo(visible, text) {
  const el = $('ai-info');
  el.classList.toggle('hidden', !visible);
  if (text !== undefined) el.innerHTML = text;
}

// 轮到 AI 时调度落子（延迟一点让"AI 思考中…"先渲染，引擎搜索是同步阻塞的）
function aiMaybeMove() {
  clearTimeout(aiTimer);
  aiTimer = null;
  state.aiThinking = false;
  updateAiInfo(state.ai.on, state.ai.on ? '等待对局进行…' : undefined);
  if (!state.ai.on || state.finished || state.mode !== 'play') return;
  if (wasm.ugo_turn() !== state.ai.color) return;
  state.aiThinking = true;
  updateStatus();
  aiTimer = setTimeout(aiDoMove, 420);
}

function aiDoMove() {
  aiTimer = null;
  state.aiThinking = false;
  if (!state.ai.on || state.finished || state.mode !== 'play') { updateStatus(); return; }
  if (wasm.ugo_turn() !== state.ai.color) { updateStatus(); return; }
  wasm.ugo_ai_set_seed(((Date.now() ^ (wasm.ugo_move_count() * 2654435761)) >>> 0) || 1);
  const t0 = performance.now();
  const action = wasm.ugo_ai_genmove();
  const elapsed = (performance.now() - t0) / 1000;
  if (action !== 2) {
    aiStats.total += elapsed;
    aiStats.count += 1;
    const avg = aiStats.total / aiStats.count;
    updateAiInfo(true,
      `AI本手落子用时<b>${elapsed.toFixed(2)}</b>秒，平均耗时<b>${avg.toFixed(2)}</b>秒`);
  }
  if (action === 2) {
    state.finished = true;
    const line = assessmentText('中盘胜，');
    updateStatus();
    updateEstBar();
    showDialog('对局结束', line);
    return;
  }
  if (action === 1) {
    const x = wasm.ugo_ai_x(), y = wasm.ugo_ai_y();
    if (wasm.ugo_play(x, y) !== 0) wasm.ugo_pass();
  } else {
    wasm.ugo_pass();
    if (!state.finished) toast('AI 虚着');
  }
  refresh();
  if (wasm.ugo_pass_count() >= 2) finishGame();
}

/* ---------- 落子交互与手势 ---------- */
const markers = new Set(); // 分析模式记号（红圈）
// 棋盘外手势区：双击空白 = 虚着，长按空白 = 悔棋。
// 棋盘内只负责落子，避免单/双击歧义（落子即时生效）。
const boardWrap = $('board-wrap');
let lastWrapTap = 0;
const LONGPRESS_MS = 600;
let lpTimer = null, lpStart = null;

function inBoardRect(x, y) {
  const r = canvas.getBoundingClientRect();
  return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
}

function cancelLongPress() {
  if (lpTimer) { clearTimeout(lpTimer); lpTimer = null; }
  lpStart = null;
}

function placeOnBoard(x, y) {
  if (state.finished) { toast('对局已结束，请开新对局'); return; }
  if (state.aiThinking) { toast('AI 思考中，请稍候'); return; }
  if (state.ai.on && wasm.ugo_turn() === state.ai.color) { toast('轮到 AI 行棋'); return; }
  const err = wasm.ugo_play(x, y);
  if (err !== 0) {
    toast(ERR_MSG[err] || '不能落子');
    return;
  }
  aiMaybeMove();
  refresh();
}

canvas.addEventListener('pointerdown', (e) => {
  const rect = canvas.getBoundingClientRect();
  const i = (e.clientX - rect.left - geom.margin) / geom.cell;
  const j = (e.clientY - rect.top - geom.margin) / geom.cell;
  const x = Math.round(i), y = Math.round(j);
  if (x < 0 || y < 0 || x >= state.size || y >= state.size) return;
  const dx = i - x, dy = j - y;
  if (Math.hypot(dx, dy) > 0.48) return;

  // 分析模式：记号 / 摆放（立即生效）
  if (state.mode === 'analysis') {
    if (state.placeColor === 'marker') {
      const idx = y * state.size + x;
      if (markers.has(idx)) markers.delete(idx); else markers.add(idx);
      render();
      return;
    }
    const color = state.placeColor === 'alt'
      ? state.nextPlace
      : parseInt(state.placeColor, 10);
    const err = wasm.ugo_place(x, y, color);
    if (err !== 0) {
      toast(ERR_MSG[err] || '不能落子');
      return;
    }
    if (state.placeColor === 'alt') state.nextPlace = 3 - state.nextPlace;
    refresh();
    return;
  }

  // 对弈模式：立即落子
  placeOnBoard(x, y);
});

// 棋盘外手势（pointerdown 统一入口）：双击 → 虚着；按住不动 600ms → 悔棋
boardWrap.addEventListener('pointerdown', (e) => {
  if (inBoardRect(e.clientX, e.clientY)) return; // 棋盘内交给落子
  const now = performance.now();
  if (now - lastWrapTap < 350 && state.mode === 'play') {
    lastWrapTap = 0;
    doPass();
    return;
  }
  lastWrapTap = now;
  // 长按悔棋
  lpStart = { x: e.clientX, y: e.clientY };
  lpTimer = setTimeout(() => {
    lpTimer = null;
    lastWrapTap = 0;
    try { if (navigator.vibrate) navigator.vibrate(30); } catch (_) { /* 不支持则忽略 */ }
    doUndo();
  }, LONGPRESS_MS);
});
boardWrap.addEventListener('pointermove', (e) => {
  if (lpStart && Math.hypot(e.clientX - lpStart.x, e.clientY - lpStart.y) > 12) {
    cancelLongPress();
  }
});
boardWrap.addEventListener('pointerup', cancelLongPress);
boardWrap.addEventListener('pointercancel', cancelLongPress);
boardWrap.addEventListener('pointerleave', cancelLongPress);
// 长按不弹系统菜单
boardWrap.addEventListener('contextmenu', (e) => e.preventDefault());

/* ---------- 悔棋 ---------- */
function doUndo() {
  if (state.aiThinking) { toast('AI 思考中，请稍候'); return; }
  clearTimeout(aiTimer);
  state.aiThinking = false;
  if (wasm.ugo_undo() !== 0) {
    toast('没有可以悔的棋');
    return;
  }
  // 人机对弈：连撤两手回到自己行棋（AI 已应答的情形）
  if (state.ai.on && state.mode === 'play'
      && wasm.ugo_turn() === state.ai.color && wasm.ugo_move_count() > 0) {
    wasm.ugo_undo();
  }
  refresh();
  // 若撤到了 AI 先手开局（AI 执黑第 0 手），让 AI 重新行棋
  aiMaybeMove();
}

$('btn-undo').addEventListener('click', doUndo);

/* ---------- 分析 / 对弈模式切换 ---------- */
$('btn-mode').addEventListener('click', () => {
  if (state.mode === 'play') {
    wasm.ugo_snapshot_push();
    state.mode = 'analysis';
    $('analysis-bar').classList.remove('hidden');
    $('btn-mode').textContent = '对弈';
    $('btn-mode').classList.add('active');
    toast('分析模式：试下开始，可随时重置盘面');
  } else {
    wasm.ugo_snapshot_pop();
    state.mode = 'play';
    markers.clear();
    $('analysis-bar').classList.add('hidden');
    $('btn-mode').textContent = '分析';
    $('btn-mode').classList.remove('active');
    toast('已返回对弈，试下盘面已还原');
  }
  refresh();
  aiMaybeMove();
});

/* ---------- 分析导航：退5 / 退1 / 重置 / 进1 / 进5 ---------- */
function analysisUndo(k) {
  for (let i = 0; i < k; i++) if (wasm.ugo_undo() !== 0) break;
  refresh();
}
function analysisRedo(k) {
  for (let i = 0; i < k; i++) if (wasm.ugo_redo() !== 0) break;
  refresh();
}
$('an-back5').addEventListener('click', () => analysisUndo(5));
$('an-back1').addEventListener('click', () => analysisUndo(1));
$('an-reset').addEventListener('click', () => {
  wasm.ugo_snapshot_pop();
  wasm.ugo_snapshot_push();
  markers.clear();
  refresh();
  toast('已重置到试下开始时的盘面');
});
$('an-fwd1').addEventListener('click', () => analysisRedo(1));
$('an-fwd5').addEventListener('click', () => analysisRedo(5));

/* ---------- 摆放颜色 ---------- */
bindSegmented('opt-place-color', (v) => {
  state.placeColor = v;
  if (v === '1') state.nextPlace = BLACK;
  if (v === '2') state.nextPlace = WHITE;
});

/* ---------- 形势显示（对弈 / 分析模式均可用） ---------- */
$('btn-influence').addEventListener('click', () => {
  state.showInfluence = !state.showInfluence;
  $('btn-influence').classList.toggle('active', state.showInfluence);
  refresh();
});

/* ---------- 菜单 ---------- */
function openMenu(open) {
  $('menu-sheet').classList.toggle('hidden', !open);
  $('menu-mask').classList.toggle('hidden', !open);
}
$('menu-tab').addEventListener('click', () => openMenu(true));
$('menu-mask').addEventListener('click', () => openMenu(false));

$('opt-labels').addEventListener('change', (e) => {
  state.labels = e.target.checked;
  render();
  savePrefs();
});

function bindSegmented(id, onPick) {
  $(id).addEventListener('click', (e) => {
    const btn = e.target.closest('button');
    if (!btn || btn.classList.contains('active')) return;
    $(id).querySelectorAll('button').forEach(b => b.classList.remove('active'));
    btn.classList.add('active');
    onPick(btn.dataset.value);
    savePrefs();
  });
}

bindSegmented('opt-move-display', (v) => { state.moveDisplay = v; render(); });

bindSegmented('opt-board-size', (v) => {
  state.size = parseInt(v, 10);
  newGame(true);
  toast(`已切换到 ${state.size} 路棋盘`);
});

bindSegmented('opt-rule', (v) => {
  state.rule = parseInt(v, 10);
  wasm.ugo_set_rule(state.rule);
  toast(['中国规则（数子 · 贴7.5）', '日本规则（数目 · 贴6.5）', '韩国规则（数目 · 贴6.5）'][state.rule]);
});

/* ---------- 人机对弈设置 ---------- */
bindSegmented('opt-ai', (v) => {
  clearTimeout(aiTimer);
  state.aiThinking = false;
  if (v === 'off') {
    state.ai.on = false;
    toast('已关闭人机对弈');
  } else {
    state.ai.on = true;
    state.ai.color = parseInt(v, 10);
    toast(`AI 执${state.ai.color === BLACK ? '黑' : '白'} · 强度${state.ai.level} · 已应用`);
  }
  applyAiConfig();
  refresh();
  updateStatus();
  aiMaybeMove();
});

$('opt-ai-level').addEventListener('input', (e) => {
  state.ai.level = parseInt(e.target.value, 10);
  $('ai-level-label').textContent = `${AI_LEVEL_NAMES[state.ai.level - 1]} · ${state.ai.level}`;
  applyAiConfig();
  savePrefs();
});

bindSegmented('opt-ai-style', (v) => {
  if (AI_STYLES[v]) state.ai.style = v;
  applyAiConfig();
  toast({ balanced: '风格：均衡', attack: '风格：激进（好攻杀）', defend: '风格：稳健（重防守）', territory: '风格：取地（重实地）', moyo: '风格：厚势（重外势）' }[state.ai.style]);
});

/* ---------- 虚着 / 认输 / 新对局 ---------- */
function doPass() {
  if (state.finished) { toast('对局已结束'); return; }
  if (state.aiThinking) { toast('AI 思考中，请稍候'); return; }
  if (state.ai.on && state.mode === 'play' && wasm.ugo_turn() === state.ai.color) {
    toast('轮到 AI 行棋');
    return;
  }
  wasm.ugo_pass();
  refresh();
  if (wasm.ugo_pass_count() >= 2) {
    finishGame();
  } else {
    toast('虚着，请对方选择');
    aiMaybeMove();
  }
}

$('btn-pass').addEventListener('click', () => {
  openMenu(false);
  doPass();
});

$('btn-pass-top').addEventListener('click', doPass);

$('btn-resign').addEventListener('click', () => {
  openMenu(false);
  if (state.finished) { toast('对局已结束'); return; }
  if (state.aiThinking) { toast('AI 思考中，请稍候'); return; }
  state.finished = true;
  const line = assessmentText('中盘胜，');
  updateStatus();
  updateEstBar();
  showDialog('对局结束', line);
});

$('btn-new').addEventListener('click', () => { openMenu(false); newGame(); });

/* ---------- 终局计分 ---------- */
// 形势明细行（终局 / 中盘认输通用）：形势分析含死子读子
function assessmentText(prefix) {
  wasm.ugo_analyze(0, 0);
  const b = wasm.ugo_est_black(), wt = wasm.ugo_est_white();
  const cb = wasm.ugo_captured_black(), cw = wasm.ugo_captured_white();
  const komi = state.rule === 0 ? 7.5 : 6.5;
  const lead = b - wt - komi;
  const winner = lead > 0 ? '黑' : '白';
  return `${prefix}黑${b}目 提${cb}目，白${wt}目 提${cw}目，${winner}胜${fmtMargin(Math.abs(lead))}目`;
}

function fmtMargin(n) { return Number.isInteger(n) ? String(n) : n.toFixed(1); }

function finishGame() {
  state.finished = true;
  const line = assessmentText('终局，');
  updateStatus();
  updateEstBar();
  showDialog('对局结束', line);
}

/* ---------- 弹窗 / Toast ---------- */
function showDialog(title, html) {
  $('dialog-title').textContent = title;
  $('dialog-body').innerHTML = html;
  $('dialog-mask').classList.remove('hidden');
}
$('dialog-ok').addEventListener('click', () => $('dialog-mask').classList.add('hidden'));

let toastTimer = null;
function toast(msg) {
  const el = $('toast');
  el.textContent = msg;
  el.classList.remove('hidden');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.add('hidden'), 1600);
}

/* ---------- 偏好持久化 ---------- */
// 供自动化测试 / 调试读取引擎状态
window.__ugo = {
  state: state,
  board: () => Array.from(boardView.slice(0, state.size * state.size)),
  influence: () => Array.from(inflView.slice(0, state.size * state.size)),
  terr: () => Array.from(terrView.slice(0, state.size * state.size)),
  estimate: () => { wasm.ugo_analyze(0, 0); return [wasm.ugo_est_black(), wasm.ugo_est_white()]; },
  turn: () => wasm.ugo_turn(),
  moves: () => wasm.ugo_move_count(),
  api: () => wasm,
  aiMove: aiMaybeMove,
};

function savePrefs() {
  localStorage.setItem('ugo-prefs', JSON.stringify({
    size: state.size, rule: state.rule, labels: state.labels, moveDisplay: state.moveDisplay,
    ai: state.ai,
  }));
}
function loadPrefs() {
  try {
    const p = JSON.parse(localStorage.getItem('ugo-prefs'));
    if (!p) return;
    state.size = [9, 13, 19].includes(p.size) ? p.size : 19;
    state.rule = [0, 1, 2].includes(p.rule) ? p.rule : 0;
    state.labels = p.labels !== false;
    if (['numbers', 'circle', 'off'].includes(p.moveDisplay)) state.moveDisplay = p.moveDisplay;
    if (p.ai && typeof p.ai === 'object') {
      state.ai.on = !!p.ai.on;
      state.ai.color = p.ai.color === BLACK ? BLACK : WHITE;
      state.ai.level = Number.isInteger(p.ai.level) ? Math.min(10, Math.max(1, p.ai.level)) : 5;
      if (AI_STYLES[p.ai.style]) state.ai.style = p.ai.style;
    }
  } catch (_) { /* 忽略损坏的偏好数据 */ }
}

function syncControls() {
  $('opt-labels').checked = state.labels;
  const pick = (id, value) => {
    $(id).querySelectorAll('button').forEach(b =>
      b.classList.toggle('active', b.dataset.value === String(value)));
  };
  pick('opt-move-display', state.moveDisplay);
  pick('opt-board-size', state.size);
  pick('opt-rule', state.rule);
  pick('opt-ai', state.ai.on ? String(state.ai.color) : 'off');
  pick('opt-ai-style', state.ai.style);
  $('opt-ai-level').value = state.ai.level;
  $('ai-level-label').textContent = `${AI_LEVEL_NAMES[state.ai.level - 1]} · ${state.ai.level}`;
}

/* ---------- 启动流程 ---------- */
window.addEventListener('resize', () => { if (wasm) render(); });

(async function boot() {
  loadPrefs();
  await loadEngine();
  newGame(true);
  syncControls();

  // 闪屏展示约 1.8 秒后淡出进入主界面
  setTimeout(() => {
    $('splash').classList.add('fading');
    $('app').classList.remove('hidden');
    render();
    setTimeout(() => $('splash').remove(), 650);
  }, 1800);
})();
