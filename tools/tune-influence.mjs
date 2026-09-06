// 势力分析调参辅助：打印典型局面的 ASCII 势力图，人工核对泛洪辐射算法
// 用法: node tools/tune-influence.mjs
import { readFileSync } from 'fs';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const wasmBuf = readFileSync(join(root, 'www/wasm/ugo_core.wasm'));
const { instance } = await WebAssembly.instantiate(wasmBuf, {});
const w = instance.exports;
const BLACK = 1, WHITE = 2;

function newGame(size, rule = 0) {
  w.ugo_new(size, rule);
  return new Uint8Array(w.memory.buffer, w.ugo_board_ptr(), size * size);
}

// 摆子：'b' 黑 'w' 白，坐标 (x,y) 0 起左上
function put(board, size, color, ...pts) {
  for (const [x, y] of pts) {
    const err = w.ugo_place(x, y, color);
    if (err !== 0) throw new Error(`place(${x},${y},${color}) err=${err}`);
  }
}

function dump(size, title) {
  w.ugo_analyze(0, 0);
  const terr = new Int8Array(w.memory.buffer, w.ugo_terr_ptr(), size * size);
  const infl = new Int32Array(w.memory.buffer, w.ugo_influence_ptr(), size * size);
  const board = new Uint8Array(w.memory.buffer, w.ugo_board_ptr(), size * size);
  const lines = [];
  for (let y = 0; y < size; y++) {
    let row = '';
    for (let x = 0; x < size; x++) {
      const i = y * size + x;
      const st = board[i];
      if (st === BLACK) row += terr[i] === -2 ? 'X' : '@';
      else if (st === WHITE) row += terr[i] === 2 ? 'O' : '=';
      else if (terr[i] === 2) row += '#';
      else if (terr[i] === -2) row += 'o';
      else if (terr[i] === 1) row += '+';
      else if (terr[i] === -1) row += '-';
      else row += '.';
    }
    lines.push(row);
  }
  const estB = w.ugo_est_black(), estW = w.ugo_est_white();
  console.log(`\n== ${title} ==  黑地估算 ${estB} / 白地估算 ${estW}`);
  console.log(lines.map((l, i) => String(i).padStart(2) + ' ' + l).join('\n'));
  return { terr, infl, estB, estW };
}

// ---- 1. 空盘 ----
{
  const s = 9;
  const b = newGame(s);
  dump(s, '1. 空盘（应全为 . 且 0/0）');
}

// ---- 2. 孤子（4-4 一颗黑子）----
{
  const s = 9;
  const b = newGame(s);
  put(b, s, BLACK, [4, 4]);
  const r = dump(s, '2. 9路 孤子黑(4,4)（周围应无 #，只有少量 +）');
  let terrCount = 0;
  for (const v of r.terr) if (v === 2) terrCount++;
  console.log(`   黑确定地点数 = ${terrCount}（期望 0）`);
}

// ---- 3. 三线一子墙 + 边（半封闭口袋）----
{
  const s = 9;
  const b = newGame(s);
  put(b, s, BLACK, [4, 0], [4, 1], [4, 2]);
  dump(s, '3. 黑墙 (4,0)-(4,2)（左侧口袋应多数为 #）');
}

// ---- 4. 双方对峙 → 抵消 ----
{
  const s = 9;
  const b = newGame(s);
  put(b, s, BLACK, [2, 4]);
  put(b, s, WHITE, [6, 4]);
  const r = dump(s, '4. 黑(2,4) vs 白(6,4)（中线附近应互相抵消为 .）');
  const mid = r.terr[4 * s + 4];
  console.log(`   中点(4,4) 分类 = ${mid}（期望 0）`);
}

// ---- 5. 死子：白二子被围（一口气）----
{
  const s = 9;
  const b = newGame(s);
  put(b, s, WHITE, [0, 0], [0, 1]);
  put(b, s, BLACK, [1, 0], [1, 1]);
  dump(s, '5. 白(0,0)(0,1) 一口气 → 应标 X（死子）且该处算黑地');
}

// ---- 6. 征子级吃子：白二子两气无眼（黑可征吃）----
{
  const s = 9;
  const b = newGame(s);
  put(b, s, WHITE, [3, 3], [4, 3]);
  put(b, s, BLACK, [2, 3], [3, 2], [4, 2], [5, 3]);
  dump(s, '6. 白(3,3)(4,3) 两气无眼接触 → 读子应判死（X O）');
}

// ---- 7. 白角二子被封死（黑可吃）→ 读子应判死 ----
{
  const s = 9;
  const b = newGame(s);
  put(b, s, WHITE, [0, 0], [1, 0]);
  put(b, s, BLACK, [2, 0], [2, 1], [0, 2]);
  // 白 (0,0)(1,0) 两气无眼：黑 (1,1) 后白无论怎么走都被吃 → 读子判死
  dump(s, '7. 白角二子被封死 → 应标 O O（死子）');
}

// ---- 8. 19路 黑布局 vs 白布局 ----
{
  const s = 19;
  const b = newGame(s);
  put(b, s, BLACK, [3, 3], [9, 3], [15, 3], [3, 9]);
  put(b, s, WHITE, [15, 15], [9, 15], [3, 15], [15, 9]);
  dump(s, '8. 19路 四星位对峙');
}

// ---- 9. 中国规则 9路 半盘黑地估算 ----
{
  const s = 9;
  const b = newGame(s);
  // 黑围左下大块
  put(b, s, BLACK, [0, 5], [1, 4], [2, 3], [3, 2], [4, 1], [5, 0]);
  put(b, s, WHITE, [8, 8], [8, 7], [7, 8]);
  const r = dump(s, '9. 黑斜线围左下（黑地估算应显著为正）');
  console.log(`   lead(黑-白) = ${r.estB - r.estW}`);
}
