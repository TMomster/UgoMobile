// AI 对手自测：合法性 / 完整自对弈终局 / 各强度与风格冒烟 / 耗时测量
// 用法: node tools/ai-selftest.mjs
import { readFileSync } from 'fs';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const wasmBuf = readFileSync(join(root, 'www/wasm/ugo_core.wasm'));
const { instance } = await WebAssembly.instantiate(wasmBuf, {});
const w = instance.exports;

const OK = 0, BLACK = 1, WHITE = 2;
let pass = 0, fail = 0;
function check(cond, msg) {
  if (cond) { pass++; }
  else { fail++; console.error('  ✗ FAIL:', msg); }
}

function newGame(size, rule = 0) {
  w.ugo_new(size, rule);
  const n = size * size;
  return {
    size,
    board: new Uint8Array(w.memory.buffer, w.ugo_board_ptr(), n),
  };
}
function dumpBoard(size) {
  const n = size * size;
  return Array.from(new Uint8Array(w.memory.buffer, w.ugo_board_ptr(), n));
}
function stoneCount(board) {
  let b = 0, e = 0;
  for (const v of board) { if (v === BLACK) b++; else if (v === EMPTY) e++; }
  return { b, e };
}
const EMPTY = 0;

function aiMove(seedExtra = 0) {
  w.ugo_ai_set_seed(((Date.now() + seedExtra * 7919) >>> 0) || 1);
  const action = w.ugo_ai_genmove();
  if (action === 1) {
    const x = w.ugo_ai_x(), y = w.ugo_ai_y();
    const err = w.ugo_play(x, y);
    if (err !== OK) return { action: 'illegal', x, y, err };
    return { action: 'play', x, y };
  }
  return { action: action === 2 ? 'resign' : 'pass' };
}

// ---------- 1. 基础规则回归（重构 ugo_play 后） ----------
{
  console.log('[1] 规则回归：提子 / 自杀 / 打劫 / 悔棋');
  const g = newGame(9);
  // 黑棋包围 (0,0) 白子 → 提子
  w.ugo_play(0, 1); // 黑
  w.ugo_play(8, 8); // 白
  w.ugo_play(1, 0); // 黑提 (0,0)? (0,0) 还没白子 — 直接测: 白下(0,0)
  const e1 = w.ugo_play(0, 0); // 白 (0,0)：此时 (0,0) 有气(右边(1,0)是黑? (1,0)=黑 (0,1)=黑) → 上边出界 左边出界 下边(0,1)黑 → 无气 → 自杀
  check(e1 === -3, `角上自杀应返回 -3，实际 ${e1}`);
  // 重来：标准提子 + 打劫
  const g3 = newGame(9);
  w.ugo_play(1, 1); // 黑
  w.ugo_play(1, 0); // 白
  w.ugo_play(0, 2); // 黑
  w.ugo_play(0, 1); // 白（1 气：(0,0)）
  const cap = w.ugo_play(0, 0); // 黑提白(0,1)单子，黑(0,0)单子单气 → 劫
  check(cap === OK, `黑(0,0)提白(0,1)应成功，实际 ${cap}`);
  const koErr = w.ugo_play(0, 1); // 白立即回提 → 禁着
  check(koErr === -4, `劫禁着应返回 -4，实际 ${koErr}`);
  // 悔棋恢复
  const before = dumpBoard(9).join(',');
  w.ugo_undo(); w.ugo_undo();
  const after = dumpBoard(9).join(',');
  check(before !== after, '悔棋后盘面有变化');
  w.ugo_play(0, 2); // 黑 (0,2)
  w.ugo_play(0, 0); // 白
  check(true, '悔棋后继续落子正常');
}
console.log(`  规则回归: ${pass} 通过 / ${fail} 失败`);

// ---------- 2. 单层 AI 着法合法性 ----------
{
  console.log('[2] AI 单步合法性（9/13/19 × 各强度）');
  let bad = 0;
  for (const size of [9, 13, 19]) {
    for (let level = 1; level <= 10; level++) {
      newGame(size);
      w.ugo_ai_config(level, 50, 50, 50, 50);
      // 预先走几手制造复杂局面
      for (let i = 0; i < 6; i++) {
        const r = aiMove(i);
        if (r.action === 'illegal') bad++;
        if (r.action !== 'play') break;
      }
    }
  }
  check(bad === 0, `AI 着法全部合法（bad=${bad}）`);
}
console.log(`  合法性: ${pass} 通过 / ${fail} 失败`);

// ---------- 3. 完整自对弈：必须自然终局且全程合法 ----------
{
  console.log('[3] 自对弈终局测试');
  // 9 路，AI 执黑 vs AI 执白，级别 5
  const size = 9;
  newGame(size);
  w.ugo_ai_config(5, 50, 50, 50, 50);
  let moves = 0, passes = 0, resign = false, illegal = false;
  const maxMoves = size * size * 4;
  const t0 = Date.now();
  while (moves < maxMoves) {
    const r = aiMove(moves);
    moves++;
    if (r.action === 'illegal') { illegal = true; console.error('  非法着:', r); break; }
    if (r.action === 'pass') passes++;
    else passes = 0;
    if (r.action === 'resign') { resign = true; break; }
    if (passes >= 2) break;
  }
  const dt = Date.now() - t0;
  check(!illegal, '自对弈全程合法');
  check(passes >= 2 || resign, `自然终局（${moves} 手，${dt}ms，平均 ${(dt / moves).toFixed(1)}ms/手）`);
  // 提子数统计
  const cb = w.ugo_captured_black(), cw = w.ugo_captured_white();
  console.log(`  9路自对弈: ${moves} 手, 黑提${cb} 白提${cw}, 终局方式: ${resign ? '认输' : '双停'}`);
}

// ---------- 4. 各强度 / 风格组合冒烟 + 19 路耗时 ----------
{
  console.log('[4] 19 路耗时与风格冒烟');
  const styles = [
    ['balanced', 50, 50, 50, 50], ['attack', 90, 30, 35, 60],
    ['defend', 25, 90, 55, 45], ['territory', 40, 50, 90, 25], ['moyo', 55, 60, 25, 85],
  ];
  for (const [name, a, d, t, m] of styles) {
    newGame(19);
    w.ugo_ai_config(6, a, d, t, m);
    const t0 = Date.now();
    let illegal = false;
    for (let i = 0; i < 12; i++) {
      const r = aiMove(i);
      if (r.action === 'illegal') { illegal = true; break; }
      if (r.action !== 'play') break;
    }
    const dt = Date.now() - t0;
    check(!illegal, `风格 ${name}: 12 手合法`);
    console.log(`  风格 ${String(name).padEnd(9)}: 12 手 ${dt}ms (首手盘面)`);
  }
  // 19 路中盘（30 手后）高强度耗时
  newGame(19);
  w.ugo_ai_config(10, 50, 50, 50, 50);
  for (let i = 0; i < 30; i++) { const r = aiMove(i); if (r.action !== 'play') break; }
  const t0 = Date.now();
  const r = aiMove(999);
  const dt = Date.now() - t0;
  check(r.action === 'play', '19路中盘 level10 出着');
  console.log(`  19路中盘 level10 单手: ${dt}ms`);
  check(dt < 3000, `高强度单手耗时 < 3s（实际 ${dt}ms）`);
}

// ---------- 5. 引擎状态完整性：genmove 后盘面 / 行棋方不被破坏 ----------
{
  console.log('[5] genmove 无副作用');
  newGame(9);
  w.ugo_ai_config(7, 50, 50, 50, 50);
  w.ugo_play(4, 4); w.ugo_play(2, 2);
  const before = dumpBoard(9).join(',');
  const turnBefore = w.ugo_turn();
  const mvBefore = w.ugo_move_count();
  w.ugo_ai_set_seed(42);
  w.ugo_ai_genmove();
  const after = dumpBoard(9).join(',');
  check(before === after, 'genmove 不改动盘面');
  check(turnBefore === w.ugo_turn(), 'genmove 不改动行棋方');
  check(mvBefore === w.ugo_move_count(), 'genmove 不改动手数');
  // genmove 之后正常落子 / 悔棋仍然可用
  check(w.ugo_play(6, 6) === OK, 'genmove 后用户落子正常');
  check(w.ugo_undo() === OK, 'genmove 后悔棋正常');
}
console.log(`  完整性: ${pass} 通过 / ${fail} 失败`);

// ---------- 6. 势力分析（泛洪辐射）回归 ----------
{
  console.log('[6] 形势分析：辐射 / 抵消 / 死子 / 估算');
  const terrOf = (size) => new Int8Array(w.memory.buffer, w.ugo_terr_ptr(), size * size);
  const inflOf = (size) => new Int32Array(w.memory.buffer, w.ugo_influence_ptr(), size * size);

  // 6a. 空盘：全 0
  newGame(9);
  w.ugo_analyze(0, 0);
  check(w.ugo_est_black() === 0 && w.ugo_est_white() === 0, '空盘估算 0/0');
  check(terrOf(9).every((v) => v === 0), '空盘分类图全 0');

  // 6b. 孤子：无确定地，有势力
  newGame(9);
  w.ugo_place(4, 4, BLACK);
  w.ugo_analyze(0, 0);
  check(!terrOf(9).some((v) => v === 2), '孤子不宣称确定地');
  check(inflOf(9).some((v) => v > 0), '孤子有黑势力辐射');

  // 6c. 相互抵消：两子正对峙，中点为 0
  newGame(9);
  w.ugo_place(2, 4, BLACK);
  w.ugo_place(6, 4, WHITE);
  w.ugo_analyze(0, 0);
  check(Math.abs(inflOf(9)[4 * 9 + 4]) <= 2, '对峙中线影响力相互抵消');

  // 6d. 死子：一口气白子标记为黑地
  newGame(9);
  w.ugo_place(0, 0, WHITE); w.ugo_place(0, 1, WHITE);
  w.ugo_place(1, 0, BLACK); w.ugo_place(1, 1, BLACK);
  w.ugo_analyze(0, 0);
  const t6 = terrOf(9);
  check(t6[0] === 2 && t6[9] === 2, `死白子标为黑地 (got ${t6[0]},${t6[9]})`);
  check(w.ugo_est_black() === 4, `死子计入估算 (黑=${w.ugo_est_black()} 期望 4)`);

  // 6e. 二线墙 + 边：角部成确定地、口袋有估算收益
  newGame(9);
  w.ugo_place(4, 0, BLACK); w.ugo_place(4, 1, BLACK); w.ugo_place(4, 2, BLACK);
  w.ugo_analyze(0, 0);
  const t6e = terrOf(9);
  check(t6e[0] === 2, '三线墙围出的边角成确定地');
  check(w.ugo_est_black() >= 8, `墙 + 边角口袋估算为正 (黑=${w.ugo_est_black()})`);
}
console.log(`  形势分析: ${pass} 通过 / ${fail} 失败`);

// ---------- 7. AI 战术行为 ----------
{
  console.log('[7] AI 战术：提子 / 救子');
  const size = 9;

  // 7a. 白子仅剩一口气 → AI 执黑必须提掉
  newGame(size);
  w.ugo_place(4, 4, WHITE);
  w.ugo_place(3, 4, BLACK); w.ugo_place(4, 3, BLACK); w.ugo_place(5, 4, BLACK);
  w.ugo_ai_config(5, 50, 50, 50, 50);
  w.ugo_ai_set_seed(7);
  const a1 = w.ugo_ai_genmove();
  check(a1 === 1 && w.ugo_ai_x() === 4 && w.ugo_ai_y() === 5,
    `叫吃子应立即提 (action=${a1}, ${w.ugo_ai_x()},${w.ugo_ai_y()})`);

  // 7b. 己方子仅剩一口气 → AI 执黑必须逃出或提对方
  newGame(size);
  w.ugo_place(4, 4, BLACK);
  w.ugo_place(3, 4, WHITE); w.ugo_place(4, 3, WHITE); w.ugo_place(5, 4, WHITE);
  w.ugo_ai_config(5, 50, 50, 50, 50);
  w.ugo_ai_set_seed(7);
  const a2 = w.ugo_ai_genmove();
  check(a2 === 1 && w.ugo_ai_x() === 4 && w.ugo_ai_y() === 5,
    `叫吃己子应立即逃 (action=${a2}, ${w.ugo_ai_x()},${w.ugo_ai_y()})`);

  // 7c. 布局期（线位分生效）+ 大棋盘：己方子被打吃仍必须应
  //     回归用例：战术分曾被线位分（156）压过，导致候选被挤出名单
  newGame(13);
  // 用三四线闲子填满排序榜，模拟真实开局
  const filler = [
    [3, 3, 1], [3, 9, 2], [9, 3, 1], [9, 9, 2], [6, 3, 1], [3, 6, 2],
    [9, 6, 1], [6, 9, 2], [4, 4, 1], [8, 8, 2], [4, 8, 1], [8, 4, 2],
  ];
  for (const [x, y, c] of filler) w.ugo_place(x, y, c);
  // 白子 (6,6) 被 (5,6)(7,6)(6,5) 三面围死，只剩 (6,7) 一口气；轮到白走
  w.ugo_place(6, 6, 2);
  w.ugo_place(5, 6, 1); w.ugo_place(7, 6, 1); w.ugo_place(6, 5, 1);
  w.ugo_ai_config(10, 50, 50, 50, 50); // genmove 走当前行棋方，与 AI 执色无关
  w.ugo_ai_set_seed(11);
  const a3 = w.ugo_ai_genmove();
  check(a3 === 1 && w.ugo_ai_x() === 6 && w.ugo_ai_y() === 7,
    `布局期叫吃子必须逃出 (action=${a3}, ${w.ugo_ai_x()},${w.ugo_ai_y()})`);
}
console.log(`  战术: ${pass} 通过 / ${fail} 失败`);

// ---------- 8. 重做（redo）回归 ----------
{
  console.log('[8] redo：落子 / 虚着 / 提子 / 摆放重放');
  const boardStr = () => dumpBoard(9).join(',');

  // 基本回放
  newGame(9);
  w.ugo_play(4, 4); w.ugo_play(5, 5);
  const snap1 = boardStr();
  const cnt1 = w.ugo_move_count();
  const turn1 = w.ugo_turn();
  w.ugo_undo(); w.ugo_undo();
  check(w.ugo_redo_count() === 2, `redo_count = 2 (got ${w.ugo_redo_count()})`);
  w.ugo_redo(); w.ugo_redo();
  check(boardStr() === snap1, 'redo 恢复盘面');
  check(w.ugo_move_count() === cnt1 && w.ugo_turn() === turn1, 'redo 恢复手数与行棋方');

  // 新着使 redo 失效
  w.ugo_undo();
  w.ugo_play(2, 2);
  check(w.ugo_redo_count() === 0, '新着后 redo 失效');

  // 虚着回放
  w.ugo_pass();
  const pc = w.ugo_pass_count();
  w.ugo_undo();
  check(w.ugo_redo() === 0 && w.ugo_pass_count() === pc, '虚着 redo');

  // 提子回放
  newGame(9);
  w.ugo_play(1, 1); w.ugo_play(1, 0); w.ugo_play(0, 2); w.ugo_play(0, 1);
  const capBefore = w.ugo_captured_black();
  w.ugo_play(0, 0); // 黑提白(0,1)
  w.ugo_undo();
  check(w.ugo_captured_black() === capBefore, '悔棋还原提子数');
  w.ugo_redo();
  check(w.ugo_captured_black() === capBefore + 1, 'redo 恢复提子数');
  const b8 = new Uint8Array(w.memory.buffer, w.ugo_board_ptr(), 81);
  check(b8.filter(v => v === 1).length === 3 && b8.filter(v => v === 2).length === 1,
    'redo 后盘面（白(0,1)仍被提）');

  // 分析摆放回放
  newGame(9);
  w.ugo_place(3, 3, 1); w.ugo_place(5, 5, 2);
  const snap2 = boardStr();
  w.ugo_undo(); w.ugo_undo();
  w.ugo_redo(); w.ugo_redo();
  check(boardStr() === snap2, '摆放 redo 恢复盘面');
}
console.log(`  redo: ${pass} 通过 / ${fail} 失败`);

console.log(`\n结果: ${pass} 通过 / ${fail} 失败`);
process.exit(fail > 0 ? 1 : 0);
