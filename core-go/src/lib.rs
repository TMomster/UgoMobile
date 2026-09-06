//! UGO 围棋引擎核心（no_std，编译为 wasm32 后由前端加载）。
//!
//! 对外采用纯 C ABI 导出，前端直接读写 wasm 线性内存中的棋盘，
//! 无需 wasm-bindgen 及额外的 JS 胶水代码生成步骤。

#![no_std]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {}
}

const MAX: usize = 19;

const EMPTY: u8 = 0;
const BLACK: u8 = 1;
const WHITE: u8 = 2;

// 返回值错误码
pub const OK: i32 = 0;
pub const ERR_OCCUPIED: i32 = -1;
pub const ERR_OFFBOARD: i32 = -2;
pub const ERR_SUICIDE: i32 = -3;
pub const ERR_KO: i32 = -4;

// 规则
pub const RULE_CHINESE: u8 = 0;
pub const RULE_JAPANESE: u8 = 1;
pub const RULE_KOREAN: u8 = 2;

static mut SIZE: usize = 19;
static mut BOARD: [u8; MAX * MAX] = [EMPTY; MAX * MAX];
static mut TURN: u8 = BLACK;
static mut MOVE_COUNT: i32 = 0;
static mut LAST_X: i32 = -1;
static mut LAST_Y: i32 = -1;
static mut KO_X: i32 = -1;
static mut KO_Y: i32 = -1;
static mut KO_ACTIVE: bool = false;
static mut CAPTURED_BY_BLACK: i32 = 0;
static mut CAPTURED_BY_WHITE: i32 = 0;
static mut PASS_COUNT: i32 = 0;
static mut RULE: u8 = RULE_CHINESE;
/// 每个交叉点上的落子手数（0 表示无子或该子未记录）
static mut MOVE_NO: [i32; MAX * MAX] = [0; MAX * MAX];

/// 悔棋历史：每手棋一条记录（落子 / 停一手 / 分析摆放）
const MAX_HIST: usize = 1024;
const CAP_MAX: usize = 4096;

#[derive(Clone, Copy)]
struct MoveRec {
    kind: u8, // 0=落子 1=停一手 2=分析摆放
    x: i32,
    y: i32,
    color: u8,
    cap_start: usize,
    n_cap: usize,
    prev_turn: u8,
    prev_move_count: i32,
    prev_last_x: i32,
    prev_last_y: i32,
    prev_ko_active: bool,
    prev_ko_x: i32,
    prev_ko_y: i32,
    prev_cap_b: i32,
    prev_cap_w: i32,
    prev_pass: i32,
    /// kind=2 时存放该点原先的手数，其余 kind 恒 0
    prev_moveno: i32,
}

const REC0: MoveRec = MoveRec {
    kind: 0, x: 0, y: 0, color: 0, cap_start: 0, n_cap: 0,
    prev_turn: BLACK, prev_move_count: 0, prev_last_x: -1, prev_last_y: -1,
    prev_ko_active: false, prev_ko_x: -1, prev_ko_y: -1,
    prev_cap_b: 0, prev_cap_w: 0, prev_pass: 0, prev_moveno: 0,
};

static mut HIST: [MoveRec; MAX_HIST] = [REC0; MAX_HIST];
static mut HIST_LEN: usize = 0;
/// 被提棋子位置的扁平栈，悔棋时按记录区间复活
static mut CAP_BUF: [u16; CAP_MAX] = [0; CAP_MAX];
/// 与 CAP_BUF 平行：被提子原先的手数，复活时一并还原
static mut CAP_NO_BUF: [i32; CAP_MAX] = [0; CAP_MAX];
static mut CAP_TOP: usize = 0;
/// 重做水位线：HIST[HIST_LEN..REDO_TOP] 为已悔棋可重放的着法；
/// 新着法入历史时即失效（REDOTOP = HIST_LEN）
static mut REDO_TOP: usize = 0;

/// 缓冲写满时丢弃全部悔棋历史（极端长会话才会触发，原型可接受）
fn push_rec(rec: MoveRec) {
    unsafe {
        if HIST_LEN >= MAX_HIST || CAP_TOP + 400 > CAP_MAX {
            HIST_LEN = 0;
            CAP_TOP = 0;
            REDO_TOP = 0;
        }
        HIST[HIST_LEN] = rec;
        HIST_LEN += 1;
        REDO_TOP = HIST_LEN; // 新着使重做区失效
    }
}

/// 试下起点快照：进入分析模式时保存，可随时整体恢复
struct Snapshot {
    saved: bool,
    size: usize,
    board: [u8; MAX * MAX],
    moveno: [i32; MAX * MAX],
    turn: u8,
    move_count: i32,
    last_x: i32,
    last_y: i32,
    ko_active: bool,
    ko_x: i32,
    ko_y: i32,
    cap_b: i32,
    cap_w: i32,
    pass_count: i32,
    hist_len: usize,
    cap_top: usize,
    redo_top: usize,
}

const SNAP0: Snapshot = Snapshot {
    saved: false, size: 19, board: [0; MAX * MAX], moveno: [0; MAX * MAX],
    turn: BLACK, move_count: 0, last_x: -1, last_y: -1,
    ko_active: false, ko_x: -1, ko_y: -1,
    cap_b: 0, cap_w: 0, pass_count: 0, hist_len: 0, cap_top: 0, redo_top: 0,
};

static mut SNAP: Snapshot = SNAP0;

/// 形势分析（Bouzy 膨胀/腐蚀）结果，紧凑 size*size 布局
static mut INFLU: [i32; MAX * MAX] = [0; MAX * MAX];
static mut INF_TMP: [i32; MAX * MAX] = [0; MAX * MAX];

const DX: [i32; 4] = [1, -1, 0, 0];
const DY: [i32; 4] = [0, 0, 1, -1];

fn size() -> usize {
    unsafe { SIZE }
}

fn at(x: i32, y: i32) -> u8 {
    let s = size();
    unsafe { BOARD[y as usize * s + x as usize] }
}

fn set(x: i32, y: i32, v: u8) {
    let s = size();
    unsafe { BOARD[y as usize * s + x as usize] = v };
}

fn in_board(x: i32, y: i32) -> bool {
    let s = size() as i32;
    x >= 0 && y >= 0 && x < s && y < s
}

/// 迭代式 flood fill 所需的固定缓冲，避免递归与动态分配。
struct Filler {
    stack: [(i32, i32); MAX * MAX],
    top: usize,
    visited: [[bool; MAX]; MAX],
}

impl Filler {
    fn new(x: i32, y: i32) -> Self {
        let mut f = Filler {
            stack: [(0, 0); MAX * MAX],
            top: 1,
            visited: [[false; MAX]; MAX],
        };
        f.visited[y as usize][x as usize] = true;
        f.stack[0] = (x, y);
        f
    }

    /// 遍历 (起点所属) 连通块。返回 (是否有气, 成员数)。
    /// 若传入 out_group（容量 >= size*size），同时把成员坐标写进去。
    fn run(&mut self, mut out_group: Option<&mut [(i32, i32)]>) -> (bool, usize) {
        let mut has_lib = false;
        let mut count = 0usize;
        while self.top > 0 {
            self.top -= 1;
            let (cx, cy) = self.stack[self.top];
            if let Some(g) = out_group.as_deref_mut() {
                g[count] = (cx, cy);
            }
            count += 1;
            for i in 0..4 {
                let nx = cx + DX[i];
                let ny = cy + DY[i];
                if !in_board(nx, ny) || self.visited[ny as usize][nx as usize] {
                    continue;
                }
                match at(nx, ny) {
                    EMPTY => has_lib = true,
                    v if v == at(cx, cy) => {
                        self.visited[ny as usize][nx as usize] = true;
                        self.stack[self.top] = (nx, ny);
                        self.top += 1;
                    }
                    _ => {}
                }
            }
        }
        (has_lib, count)
    }
}

static mut GROUP_BUF: [(i32, i32); MAX * MAX] = [(0, 0); MAX * MAX];

/// 提掉 (x,y) 处无气的棋块，返回提子数；有气返回 0。
/// 同时把被提子的位置与原手数压入 CAP 栈，供悔棋复活。
fn remove_dead_group(x: i32, y: i32) -> i32 {
    let color = at(x, y);
    if color == EMPTY {
        return 0;
    }
    unsafe {
        let mut filler = Filler::new(x, y);
        let (has_lib, count) = filler.run(Some(unsafe { &mut GROUP_BUF[..] }));
        if has_lib {
            return 0;
        }
        for i in 0..count {
            let (gx, gy) = GROUP_BUF[i];
            let idx = gy as usize * SIZE + gx as usize;
            let mno = MOVE_NO[idx];
            set(gx, gy, EMPTY);
            MOVE_NO[idx] = 0;
            if CAP_TOP < CAP_MAX {
                CAP_BUF[CAP_TOP] = idx as u16;
                CAP_NO_BUF[CAP_TOP] = mno;
                CAP_TOP += 1;
            }
        }
        count as i32
    }
}

/// 试下恢复数据：try_play 之后调用 untry_play 可完整还原盘面与状态，
/// 不触及悔棋历史（AI 搜索专用）。
struct Undo {
    turn: u8,
    move_count: i32,
    last_x: i32,
    last_y: i32,
    ko_active: bool,
    ko_x: i32,
    ko_y: i32,
    cap_b: i32,
    cap_w: i32,
    pass: i32,
    cap_top: usize,
}

/// 试下：与 ugo_play 完全相同的规则校验与提子/劫逻辑，但不写悔棋历史。
/// 返回 (错误码, 恢复数据)；错误码为 OK 时须调用 untry_play 还原。
fn try_play(x: i32, y: i32) -> (i32, Undo) {
    let u = unsafe {
        Undo {
            turn: TURN,
            move_count: MOVE_COUNT,
            last_x: LAST_X,
            last_y: LAST_Y,
            ko_active: KO_ACTIVE,
            ko_x: KO_X,
            ko_y: KO_Y,
            cap_b: CAPTURED_BY_BLACK,
            cap_w: CAPTURED_BY_WHITE,
            pass: PASS_COUNT,
            cap_top: CAP_TOP,
        }
    };
    if !in_board(x, y) {
        return (ERR_OFFBOARD, u);
    }
    unsafe {
        if at(x, y) != EMPTY {
            return (ERR_OCCUPIED, u);
        }
        if KO_ACTIVE && x == KO_X && y == KO_Y {
            return (ERR_KO, u);
        }
        let me = TURN;
        let opp = if me == BLACK { WHITE } else { BLACK };

        set(x, y, me);

        // 先提对方相邻的无气块；记录单子被提位置用于简单劫判定
        let mut captured = 0i32;
        let mut ko_point = (x, y);
        for i in 0..4 {
            let nx = x + DX[i];
            let ny = y + DY[i];
            if in_board(nx, ny) && at(nx, ny) == opp {
                let c = remove_dead_group(nx, ny);
                if c == 1 {
                    ko_point = (nx, ny);
                }
                captured += c;
            }
        }

        // 自杀检查（未提子且自身无气）
        let mut filler = Filler::new(x, y);
        let (has_lib, _) = filler.run(None);
        if !has_lib {
            set(x, y, EMPTY);
            while CAP_TOP > u.cap_top {
                CAP_TOP -= 1;
                let idx = CAP_BUF[CAP_TOP] as usize;
                BOARD[idx] = me ^ 3;
                MOVE_NO[idx] = CAP_NO_BUF[CAP_TOP];
            }
            return (ERR_SUICIDE, u);
        }

        // 简单劫：恰好提一子，且己方落子成单一子、单一气 → 禁入被提点
        if captured == 1 {
            let mut filler2 = Filler::new(x, y);
            let (_, cnt) = filler2.run(None);
            let mut libs = 0;
            for i in 0..4 {
                let nx = x + DX[i];
                let ny = y + DY[i];
                if in_board(nx, ny) && at(nx, ny) == EMPTY {
                    libs += 1;
                }
            }
            KO_ACTIVE = cnt == 1 && libs == 1;
            KO_X = ko_point.0;
            KO_Y = ko_point.1;
        } else {
            KO_ACTIVE = false;
        }

        if me == BLACK {
            CAPTURED_BY_BLACK += captured;
        } else {
            CAPTURED_BY_WHITE += captured;
        }

        LAST_X = x;
        LAST_Y = y;
        MOVE_COUNT += 1;
        MOVE_NO[y as usize * SIZE + x as usize] = MOVE_COUNT;
        PASS_COUNT = 0;
        TURN = opp;
        (OK, u)
    }
}

/// 撤销 try_play 的一切盘面与状态变更。
fn untry_play(x: i32, y: i32, u: Undo) {
    unsafe {
        set(x, y, EMPTY);
        MOVE_NO[y as usize * SIZE + x as usize] = 0;
        while CAP_TOP > u.cap_top {
            CAP_TOP -= 1;
            let idx = CAP_BUF[CAP_TOP] as usize;
            BOARD[idx] = u.turn ^ 3; // 被提的是对方子
            MOVE_NO[idx] = CAP_NO_BUF[CAP_TOP];
        }
        TURN = u.turn;
        MOVE_COUNT = u.move_count;
        LAST_X = u.last_x;
        LAST_Y = u.last_y;
        KO_ACTIVE = u.ko_active;
        KO_X = u.ko_x;
        KO_Y = u.ko_y;
        CAPTURED_BY_BLACK = u.cap_b;
        CAPTURED_BY_WHITE = u.cap_w;
        PASS_COUNT = u.pass;
    }
}

/// 尝试在 (x,y) 落子（当前执子方），返回 0 或负数错误码。
#[no_mangle]
pub extern "C" fn ugo_play(x: i32, y: i32) -> i32 {
    let (r, u) = try_play(x, y);
    if r != OK {
        return r;
    }
    unsafe {
        push_rec(MoveRec {
            kind: 0,
            x, y,
            color: u.turn,
            cap_start: u.cap_top,
            n_cap: CAP_TOP - u.cap_top,
            prev_turn: u.turn,
            prev_move_count: u.move_count,
            prev_last_x: u.last_x, prev_last_y: u.last_y,
            prev_ko_active: u.ko_active, prev_ko_x: u.ko_x, prev_ko_y: u.ko_y,
            prev_cap_b: u.cap_b, prev_cap_w: u.cap_w,
            prev_pass: 0, prev_moveno: 0,
        });
    }
    OK
}

#[no_mangle]
pub extern "C" fn ugo_pass() {
    unsafe {
        let p_turn = TURN;
        let p_move_count = MOVE_COUNT;
        let p_last_x = LAST_X;
        let p_last_y = LAST_Y;
        let p_ko_active = KO_ACTIVE;
        let p_ko_x = KO_X;
        let p_ko_y = KO_Y;
        let p_pass = PASS_COUNT;

        PASS_COUNT += 1;
        KO_ACTIVE = false;
        LAST_X = -1;
        LAST_Y = -1;
        MOVE_COUNT += 1;
        TURN = if TURN == BLACK { WHITE } else { BLACK };

        push_rec(MoveRec {
            kind: 1, x: -1, y: -1, color: 0,
            cap_start: CAP_TOP, n_cap: 0,
            prev_turn: p_turn,
            prev_move_count: p_move_count,
            prev_last_x: p_last_x, prev_last_y: p_last_y,
            prev_ko_active: p_ko_active, prev_ko_x: p_ko_x, prev_ko_y: p_ko_y,
            prev_cap_b: CAPTURED_BY_BLACK, prev_cap_w: CAPTURED_BY_WHITE,
            prev_pass: p_pass, prev_moveno: 0,
        });
    }
}

/// 悔一手（普通落子 / 停一手 / 分析摆放均可撤销），无可撤时返回 ERR_OFFBOARD。
#[no_mangle]
pub extern "C" fn ugo_undo() -> i32 {
    unsafe {
        if HIST_LEN == 0 {
            return ERR_OFFBOARD;
        }
        HIST_LEN -= 1;
        let rec = HIST[HIST_LEN];

        match rec.kind {
            0 | 2 => {
                // 撤销落子 / 摆放：清点、复活被提子、还原手数
                set(rec.x, rec.y, EMPTY);
                MOVE_NO[rec.y as usize * SIZE + rec.x as usize] = 0;
                if rec.n_cap > 0 {
                    let mut i = rec.cap_start + rec.n_cap;
                    while i > rec.cap_start {
                        i -= 1;
                        let idx = CAP_BUF[i] as usize;
                        BOARD[idx] = rec.color ^ 3; // 被提的是对方子
                        MOVE_NO[idx] = CAP_NO_BUF[i];
                    }
                    CAP_TOP = rec.cap_start;
                }
            }
            _ => {}
        }

        TURN = rec.prev_turn;
        MOVE_COUNT = rec.prev_move_count;
        LAST_X = rec.prev_last_x;
        LAST_Y = rec.prev_last_y;
        KO_ACTIVE = rec.prev_ko_active;
        KO_X = rec.prev_ko_x;
        KO_Y = rec.prev_ko_y;
        CAPTURED_BY_BLACK = rec.prev_cap_b;
        CAPTURED_BY_WHITE = rec.prev_cap_w;
        PASS_COUNT = rec.prev_pass;
        OK
    }
}

/// 重做一手：回放 HIST[HIST_LEN] 处已悔掉的着法（落子 / 虚着 / 摆放）。
/// 回放是确定性的：提子、劫、手数与原局一致；CAP 栈按顺序推进。
/// 无可重放时返回 ERR_OFFBOARD。不新增历史记录（记录已在 HIST 中）。
#[no_mangle]
pub extern "C" fn ugo_redo() -> i32 {
    unsafe {
        if HIST_LEN >= REDO_TOP || HIST_LEN >= MAX_HIST {
            return ERR_OFFBOARD;
        }
        let rec = HIST[HIST_LEN];
        match rec.kind {
            0 => {
                // 落子：以原色重放（try_play 不写历史）
                TURN = rec.color;
                let (r, _) = try_play(rec.x, rec.y);
                if r != OK {
                    // 理论不可达（顺序回放与原局一致）；防御性返回
                    return ERR_OCCUPIED;
                }
            }
            1 => {
                // 虚着：与 ugo_pass 相同的状态变化，但不写记录
                PASS_COUNT += 1;
                KO_ACTIVE = false;
                LAST_X = -1;
                LAST_Y = -1;
                MOVE_COUNT += 1;
                TURN = if TURN == BLACK { WHITE } else { BLACK };
            }
            _ => {
                // 分析摆放（kind=2）：与 ugo_place 相同的状态变化，但不写记录
                let me = rec.color;
                let opp = if me == BLACK { WHITE } else { BLACK };
                set(rec.x, rec.y, me);
                for i in 0..4 {
                    let nx = rec.x + DX[i];
                    let ny = rec.y + DY[i];
                    if in_board(nx, ny) && at(nx, ny) == opp {
                        remove_dead_group(nx, ny);
                    }
                }
                let mut filler = Filler::new(rec.x, rec.y);
                let (has_lib, _) = filler.run(None);
                if !has_lib {
                    return ERR_SUICIDE;
                }
                MOVE_COUNT += 1;
                MOVE_NO[rec.y as usize * SIZE + rec.x as usize] = -MOVE_COUNT;
                KO_ACTIVE = false;
                LAST_X = rec.x;
                LAST_Y = rec.y;
            }
        }
        HIST_LEN += 1;
        OK
    }
}

/// 当前可重做的手数（前端可用于按钮可用性）
#[no_mangle]
pub extern "C" fn ugo_redo_count() -> i32 {
    unsafe { (REDO_TOP - HIST_LEN.min(REDO_TOP)) as i32 }
}

/// 分析模式摆放：颜色由前端指定（1 黑 / 2 白），不遵守 alternation，
/// 但仍校验占位与自杀；可提子、可悔棋（kind=2 记录）。
#[no_mangle]
pub extern "C" fn ugo_place(x: i32, y: i32, color: i32) -> i32 {
    if !in_board(x, y) {
        return ERR_OFFBOARD;
    }
    if color != BLACK as i32 && color != WHITE as i32 {
        return ERR_OFFBOARD;
    }
    unsafe {
        if at(x, y) != EMPTY {
            return ERR_OCCUPIED;
        }
        let me = color as u8;
        let opp = if me == BLACK { WHITE } else { BLACK };

        let rec_cap_start = CAP_TOP;
        let p_moveno = MOVE_NO[y as usize * SIZE + x as usize];

        set(x, y, me);

        let mut captured = 0i32;
        for i in 0..4 {
            let nx = x + DX[i];
            let ny = y + DY[i];
            if in_board(nx, ny) && at(nx, ny) == opp {
                captured += remove_dead_group(nx, ny);
            }
        }

        let mut filler = Filler::new(x, y);
        let (has_lib, _) = filler.run(None);
        if !has_lib {
            set(x, y, EMPTY);
            return ERR_SUICIDE;
        }

        // 手数标记为负数（摆放子），用于与正常落子区分显示
        MOVE_COUNT += 1;
        MOVE_NO[y as usize * SIZE + x as usize] = -MOVE_COUNT;
        KO_ACTIVE = false;
        LAST_X = x;
        LAST_Y = y;

        push_rec(MoveRec {
            kind: 2, x, y, color: me,
            cap_start: rec_cap_start,
            n_cap: CAP_TOP - rec_cap_start,
            prev_turn: TURN,
            prev_move_count: MOVE_COUNT - 1,
            prev_last_x: LAST_X, prev_last_y: LAST_Y,
            prev_ko_active: KO_ACTIVE, prev_ko_x: KO_X, prev_ko_y: KO_Y,
            prev_cap_b: CAPTURED_BY_BLACK, prev_cap_w: CAPTURED_BY_WHITE,
            prev_pass: PASS_COUNT, prev_moveno: p_moveno,
        });
        OK
    }
}

/// 试下：把当前盘面压入快照栈（原型单层）。
#[no_mangle]
pub extern "C" fn ugo_snapshot_push() {
    unsafe {
        SNAP = Snapshot {
            saved: true,
            size: SIZE,
            board: BOARD,
            moveno: MOVE_NO,
            turn: TURN,
            move_count: MOVE_COUNT,
            last_x: LAST_X,
            last_y: LAST_Y,
            ko_active: KO_ACTIVE,
            ko_x: KO_X,
            ko_y: KO_Y,
            cap_b: CAPTURED_BY_BLACK,
            cap_w: CAPTURED_BY_WHITE,
            pass_count: PASS_COUNT,
            hist_len: HIST_LEN,
            cap_top: CAP_TOP,
            redo_top: REDO_TOP,
        };
    }
}

/// 重置到试下开始时的盘面（快照之后的一切变更全部丢弃）。
#[no_mangle]
pub extern "C" fn ugo_snapshot_pop() -> i32 {
    unsafe {
        if !SNAP.saved {
            return ERR_OFFBOARD;
        }
        SNAP.saved = false;
        SIZE = SNAP.size;
        BOARD = SNAP.board;
        MOVE_NO = SNAP.moveno;
        TURN = SNAP.turn;
        MOVE_COUNT = SNAP.move_count;
        LAST_X = SNAP.last_x;
        LAST_Y = SNAP.last_y;
        KO_ACTIVE = SNAP.ko_active;
        KO_X = SNAP.ko_x;
        KO_Y = SNAP.ko_y;
        CAPTURED_BY_BLACK = SNAP.cap_b;
        CAPTURED_BY_WHITE = SNAP.cap_w;
        PASS_COUNT = SNAP.pass_count;
        HIST_LEN = SNAP.hist_len;
        CAP_TOP = SNAP.cap_top;
        REDO_TOP = SNAP.redo_top;
        OK
    }
}

#[no_mangle]
pub extern "C" fn ugo_new(board_size: i32, rule: u8) {
    unsafe {
        SIZE = board_size.clamp(5, MAX as i32) as usize;
        BOARD = [EMPTY; MAX * MAX];
        MOVE_NO = [0; MAX * MAX];
        TURN = BLACK;
        MOVE_COUNT = 0;
        LAST_X = -1;
        LAST_Y = -1;
        KO_ACTIVE = false;
        CAPTURED_BY_BLACK = 0;
        CAPTURED_BY_WHITE = 0;
        PASS_COUNT = 0;
        RULE = rule;
        HIST_LEN = 0;
        CAP_TOP = 0;
        REDO_TOP = 0;
        SNAP.saved = false;
        INFLU = [0; MAX * MAX];
    }
}

#[no_mangle]
pub extern "C" fn ugo_set_rule(rule: u8) {
    unsafe { RULE = rule }
}

#[no_mangle]
pub extern "C" fn ugo_get(x: i32, y: i32) -> i32 {
    if !in_board(x, y) {
        return ERR_OFFBOARD;
    }
    at(x, y) as i32
}

#[no_mangle]
pub extern "C" fn ugo_turn() -> i32 {
    unsafe { TURN as i32 }
}

#[no_mangle]
pub extern "C" fn ugo_move_count() -> i32 {
    unsafe { MOVE_COUNT }
}

#[no_mangle]
pub extern "C" fn ugo_last_x() -> i32 {
    unsafe { LAST_X }
}

#[no_mangle]
pub extern "C" fn ugo_last_y() -> i32 {
    unsafe { LAST_Y }
}

#[no_mangle]
pub extern "C" fn ugo_captured_black() -> i32 {
    unsafe { CAPTURED_BY_BLACK }
}

#[no_mangle]
pub extern "C" fn ugo_captured_white() -> i32 {
    unsafe { CAPTURED_BY_WHITE }
}

#[no_mangle]
pub extern "C" fn ugo_pass_count() -> i32 {
    unsafe { PASS_COUNT }
}

/// 棋盘数据指针，前端用 Uint8Array(memory.buffer, ptr, size*size) 读取。
#[no_mangle]
pub extern "C" fn ugo_board_ptr() -> *const u8 {
    unsafe { BOARD.as_ptr() }
}

/// 手数数组指针（i32），前端用 Int32Array(memory.buffer, ptr, size*size) 读取。
#[no_mangle]
pub extern "C" fn ugo_moveno_ptr() -> *const i32 {
    unsafe { MOVE_NO.as_ptr() }
}

/// 死子自动判定超出原型范围；双方连续停一手后按全子存活估算得分。
/// 黑白得分分别查询；贴目小数（7.5 / 6.5）由前端补齐。
#[no_mangle]
pub extern "C" fn ugo_score_black() -> i32 {
    score_both().0
}

#[no_mangle]
pub extern "C" fn ugo_score_white() -> i32 {
    score_both().1
}

fn territory() -> (i32, i32) {
    let s = size();
    let mut visited = [[false; MAX]; MAX];
    let mut tb = 0i32;
    let mut tw = 0i32;
    for y in 0..s {
        for x in 0..s {
            if visited[y][x] || at(x as i32, y as i32) != EMPTY {
                continue;
            }
            let mut filler = Filler::new(x as i32, y as i32);
            let (_, cnt) = filler.run(Some(unsafe { &mut GROUP_BUF[..] }));
            let region: [(i32, i32); MAX * MAX] = unsafe { GROUP_BUF };
            for i in 0..cnt {
                let (gx, gy) = region[i];
                visited[gy as usize][gx as usize] = true;
            }
            let mut touch_b = false;
            let mut touch_w = false;
            for i in 0..cnt {
                let (gx, gy) = region[i];
                for d in 0..4 {
                    let nx = gx + DX[d];
                    let ny = gy + DY[d];
                    if !in_board(nx, ny) {
                        continue;
                    }
                    match at(nx, ny) {
                        BLACK => touch_b = true,
                        WHITE => touch_w = true,
                        _ => {}
                    }
                }
            }
            if touch_b && !touch_w {
                tb += cnt as i32;
            } else if touch_w && !touch_b {
                tw += cnt as i32;
            }
        }
    }
    (tb, tw)
}

fn score_both() -> (i32, i32) {
    let s = size() as i32;
    let rule = unsafe { RULE };
    let (tb, tw) = territory();

    if rule == RULE_CHINESE {
        // 数子法：活子 + 围空（贴 7.5 目，0.5 由前端补）
        let mut sb = 0i32;
        let mut sw = 0i32;
        for y in 0..s {
            for x in 0..s {
                match at(x, y) {
                    BLACK => sb += 1,
                    WHITE => sw += 1,
                    _ => {}
                }
            }
        }
        (sb + tb, sw + tw + 7)
    } else {
        // 日韩数空法：围空 + 提子（贴 6.5 目）
        let cb = unsafe { CAPTURED_BY_BLACK };
        let cw = unsafe { CAPTURED_BY_WHITE };
        (tb + cw, tw + cb + 6)
    }
}

// ==================== 形势分析（泛洪辐射影响力模型） ====================
//
// 参考的经典研究：
// - Zobrist (1970) 与 GNU Go 的「辐射影响力」模型：每颗棋子向四周泛洪辐射
//   影响力，随距离衰减，被对方棋子阻挡、可穿过己方棋子；黑白影响力在同一点
//   代数相加、相互抵消（正 = 黑势力，负 = 白势力）。
// - Bouzy (1995)《Mathematical Morphology Applied to Computer Go》：在辐射图上
//   平台化后做腐蚀——只有被棋子/盘缘围出足够厚度的地方才确认为「确定地」，
//   孤子与朝开阔方向宣称的地盘会被腐蚀掉，只保留「势力/模样」标签。
// - 死子判定采用 GNU Go reading.c 式 attack/defend 深度受限吃子搜索，
//   征子（梯子）等长程吃子自动被读到。
//
// 输出：
//   INFLU  净影响力 R（黑正白负）——势力/模样显示与 AI 评估共用
//   ERODED 形态学确认后的地盘图
//   TERR   分类图（i8）：+2/-2 确定地，+1/-1 势力，0 中立；死子格标为对方地
//   EST_B / EST_W  按当前规则的形势估算点数（不含贴目）

const RAY_D: i32 = 8;
/// 辐射衰减表：与棋子曼哈顿距离 d 处的单子贡献（随距离快速递减）
const RAY_W: [i32; 8] = [12, 10, 8, 7, 5, 4, 2, 1];
/// 腐蚀图的「满强度」平台值
const CAP_PLATEAU: i32 = 16;
/// 净影响力达到该值才有资格成为确定地（平台化门槛）
const T_PLATEAU: i32 = 12;
/// 腐蚀轮数（需足够吃掉朝开阔面宣称的地盘；封闭口袋不受影响）
const EROSION_ROUNDS: i32 = 8;
/// 腐蚀后判定确定地的阈值
const T_TERR: i32 = 10;
/// 净影响力判定势力/模样的阈值
const T_INFL: i32 = 5;

static mut ERODED: [i32; MAX * MAX] = [0; MAX * MAX];
static mut TERR: [i8; MAX * MAX] = [0; MAX * MAX];
/// 每点所属棋块的辐射强度（八分比，带辐射符号；死子已翻转为对方符号）
static mut GSTRENGTH: [i32; MAX * MAX] = [0; MAX * MAX];
/// 每点所属棋块是否被判死
static mut GDEAD: [bool; MAX * MAX] = [false; MAX * MAX];
/// 每点所属棋块的气数 / 子数（AI 候选评估用）
static mut GLIB: [i32; MAX * MAX] = [0; MAX * MAX];
static mut GSZ: [i32; MAX * MAX] = [0; MAX * MAX];
/// 每点所属棋块是否接触敌子
static mut GCONTACT: [bool; MAX * MAX] = [false; MAX * MAX];
/// 己方处于叫吃状态的棋子标记
static mut ATARI: [[bool; MAX]; MAX] = [[false; MAX]; MAX];
static mut EST_B: i32 = 0;
static mut EST_W: i32 = 0;

/// 逐子泛洪 BFS 的队列与访问戳
static mut BFS_Q: [(i32, i32, i32); MAX * MAX] = [(0, 0, 0); MAX * MAX];
static mut BFS_SEEN: [u32; MAX * MAX] = [0; MAX * MAX];
static mut BFS_GEN: u32 = 0;

const DIAG: [(i32, i32); 4] = [(-1, -1), (1, -1), (-1, 1), (1, 1)];

// ---- AI / 分析共用的棋块普查缓冲 ----
static mut GEN: u32 = 0;
static mut VIS_GEN: [[u32; MAX]; MAX] = [[0; MAX]; MAX];
/// 逐组扫描的已访问标记（每次外层扫描整体清除一次）
static mut SEEN: [[bool; MAX]; MAX] = [[false; MAX]; MAX];
static mut GBUF2: [(i32, i32); MAX * MAX] = [(0, 0); MAX * MAX];
static mut LIB_PTS: [(i32, i32); MAX * MAX] = [(0, 0); MAX * MAX];
static mut READ_OPS: i32 = 0;
static mut READ_BUDGET: i32 = 600;

/// 泛洪收集 (x,y) 所属棋块：成员写入 GBUF2，气点写入 LIB_PTS。
/// 返回 (成员数, 气数, 是否接触敌子)。
fn group_info(x: i32, y: i32) -> (usize, i32, bool) {
    unsafe {
        GEN = GEN.wrapping_add(1);
        let gen = GEN;
        let s = size();
        let mut stack = [(0i32, 0i32); MAX * MAX];
        let mut top = 1usize;
        stack[0] = (x, y);
        VIS_GEN[y as usize][x as usize] = gen;
        let color = at(x, y);
        let mut cnt = 0usize;
        let mut libs = 0i32;
        let mut contact = false;
        while top > 0 {
            top -= 1;
            let (cx, cy) = stack[top];
            GBUF2[cnt] = (cx, cy);
            cnt += 1;
            for i in 0..4 {
                let nx = cx + DX[i];
                let ny = cy + DY[i];
                if !in_board(nx, ny) {
                    continue;
                }
                let v = at(nx, ny);
                if v == EMPTY {
                    if VIS_GEN[ny as usize][nx as usize] != gen {
                        VIS_GEN[ny as usize][nx as usize] = gen;
                        if (libs as usize) < MAX * MAX {
                            LIB_PTS[libs as usize] = (nx, ny);
                        }
                        libs += 1;
                    }
                } else if v == color {
                    if VIS_GEN[ny as usize][nx as usize] != gen {
                        VIS_GEN[ny as usize][nx as usize] = gen;
                        stack[top] = (nx, ny);
                        top += 1;
                    }
                } else {
                    contact = true;
                }
            }
        }
        (cnt, libs, contact)
    }
}

/// 简单眼位判定（启发式）：四邻全为己方或盘外，且
/// 边角点斜角全为己方/盘外、中央点至少三个斜角为己方/盘外。
fn is_eye_for(x: i32, y: i32, color: u8) -> bool {
    unsafe {
        for d in 0..4 {
            let nx = x + DX[d];
            let ny = y + DY[d];
            if in_board(nx, ny) && at(nx, ny) != color {
                return false;
            }
        }
        let s = size() as i32;
        let mut own = 0i32;
        for d in 0..4 {
            let (dx, dy) = DIAG[d];
            let nx = x + dx;
            let ny = y + dy;
            if !in_board(nx, ny) || at(nx, ny) == color {
                own += 1;
            }
        }
        if x == 0 || y == 0 || x == s - 1 || y == s - 1 {
            own == 4
        } else {
            own >= 3
        }
    }
}

/// 深度受限吃子搜索（GNU Go attack/defend 风格）：
/// 判断以攻方身份能否在 depth 手内提掉 (tx,ty) 处棋块；att_moves = 攻方先走。
/// 预算（READ_OPS）耗尽时按「提不掉」处理（对死活判定保守）。
fn read_cap(tx: i32, ty: i32, depth: i32, att_moves: bool) -> bool {
    if depth <= 0 {
        return false;
    }
    unsafe {
        READ_OPS += 1;
        if READ_OPS > READ_BUDGET {
            return false;
        }
        let target = at(tx, ty);
        if target == EMPTY {
            return true;
        }
        let att = if target == BLACK { WHITE } else { BLACK };
        let saved_turn = TURN;
        let (cnt, libs, _) = group_info(tx, ty);
        if libs == 0 {
            TURN = saved_turn;
            return true;
        }
        let mut result = false;
        if att_moves {
            // 攻方走：只剩一口气直接提；否则逐口紧气
            if libs == 1 {
                let (lx, ly) = LIB_PTS[0];
                TURN = att;
                let (r, u) = try_play(lx, ly);
                if r == OK {
                    result = at(tx, ty) != target;
                    untry_play(lx, ly, u);
                }
                TURN = saved_turn;
                return result;
            }
            // 复制前几口气（递归会改写 LIB_PTS）
            let mut libs_local = [(0i32, 0i32); 4];
            let tryn = libs.min(4) as usize;
            for i in 0..tryn {
                libs_local[i] = LIB_PTS[i];
            }
            for i in 0..tryn {
                let (lx, ly) = libs_local[i];
                TURN = att;
                let (r, u) = try_play(lx, ly);
                if r == OK {
                    if at(tx, ty) != target {
                        result = true;
                    } else {
                        result = read_cap(tx, ty, depth - 1, false);
                    }
                    untry_play(lx, ly, u);
                }
                TURN = saved_turn;
                if result {
                    break;
                }
            }
        } else {
            // 防守方先走
            if libs >= 3 {
                TURN = saved_turn;
                return false;
            }
            if libs == 2 {
                // 有真眼的两气块近似视为活
                let mut eye = false;
                for i in 0..libs as usize {
                    if is_eye_for(LIB_PTS[i].0, LIB_PTS[i].1, target) {
                        eye = true;
                    }
                }
                if eye {
                    TURN = saved_turn;
                    return false;
                }
            }
            // 防守着点：自己的气 + 提掉相邻的叫吃攻块
            let mut dps = [(0i32, 0i32); 8];
            let mut nd = 0usize;
            for i in 0..libs as usize {
                if nd < 8 {
                    dps[nd] = LIB_PTS[i];
                    nd += 1;
                }
            }
            // 先复制相邻攻方棋子（group_info 会改写 GBUF2）
            let mut adj_att = [(0i32, 0i32); 8];
            let mut na = 0usize;
            for ci in 0..cnt {
                let (gx, gy) = GBUF2[ci];
                for d in 0..4 {
                    let nx = gx + DX[d];
                    let ny = gy + DY[d];
                    if !in_board(nx, ny) || at(nx, ny) != att {
                        continue;
                    }
                    let mut dup = false;
                    for k in 0..na {
                        if adj_att[k] == (nx, ny) {
                            dup = true;
                        }
                    }
                    if !dup && na < 8 {
                        adj_att[na] = (nx, ny);
                        na += 1;
                    }
                }
            }
            for ai in 0..na {
                let (ax, ay) = adj_att[ai];
                let (_, alibs, _) = group_info(ax, ay);
                if alibs == 1 {
                    let (px, py) = LIB_PTS[0];
                    let mut dup = false;
                    for k in 0..nd {
                        if dps[k] == (px, py) {
                            dup = true;
                        }
                    }
                    if !dup && nd < 8 {
                        dps[nd] = (px, py);
                        nd += 1;
                    }
                }
            }
            // 逐一尝试防守；全部失败才算被吃
            let mut all_fail = true;
            for di in 0..nd {
                let (dx2, dy2) = dps[di];
                TURN = target;
                let (r, u) = try_play(dx2, dy2);
                if r == OK {
                    let (_, libs2, _) = group_info(tx, ty);
                    let caps = (CAP_TOP - u.cap_top) as i32;
                    let escaped = libs2 >= 3
                        || (caps > 0 && libs2 >= 2)
                        || !read_cap(tx, ty, depth - 1, true);
                    untry_play(dx2, dy2, u);
                    if escaped {
                        all_fail = false;
                        break;
                    }
                }
                TURN = saved_turn;
            }
            TURN = saved_turn;
            result = all_fail;
        }
        result
    }
}

/// 全盘棋块普查：气数 / 真眼数 / 是否接触敌子 / 死活判定 / 辐射强度。
/// read_depth > 0 时对两气无眼的接触块做吃子搜索确认死活（read_budget 限制读子工作量）。
/// 同时填充 GLIB / GSZ / ATARI 供 AI 使用。
fn scan_groups(read_depth: i32, read_budget: i32) {
    unsafe {
        let s = size();
        for i in 0..s * s {
            GSTRENGTH[i] = 0;
            GDEAD[i] = false;
            GLIB[i] = 0;
            GSZ[i] = 0;
            GCONTACT[i] = false;
        }
        for yy in 0..s {
            for xx in 0..s {
                SEEN[yy][xx] = false;
                ATARI[yy][xx] = false;
            }
        }
        READ_OPS = 0;
        READ_BUDGET = read_budget;
        for y in 0..s {
            for x in 0..s {
                if SEEN[y][x] {
                    continue;
                }
                let color = at(x as i32, y as i32);
                if color == EMPTY {
                    continue;
                }
                let (cnt, libs, contact) = group_info(x as i32, y as i32);
                for i in 0..cnt {
                    let (gx, gy) = GBUF2[i];
                    SEEN[gy as usize][gx as usize] = true;
                    let idx = gy as usize * s + gx as usize;
                    GLIB[idx] = libs;
                    GSZ[idx] = cnt as i32;
                    GCONTACT[idx] = contact;
                    if libs == 1 {
                        ATARI[gy as usize][gx as usize] = true;
                    }
                }
                // 真眼数
                let mut eyes = 0i32;
                for li in 0..libs as usize {
                    if is_eye_for(LIB_PTS[li].0, LIB_PTS[li].1, color) {
                        eyes += 1;
                    }
                }
                // 一口气且最后的眼填不掉 → 对方填入即自杀，无法提块
                let mut unfillable = false;
                if libs == 1 && is_eye_for(LIB_PTS[0].0, LIB_PTS[0].1, color) {
                    let (lx, ly) = LIB_PTS[0];
                    let mut all_own = true;
                    for d in 0..4 {
                        let (ddx, ddy) = DIAG[d];
                        let nx = lx + ddx;
                        let ny = ly + ddy;
                        if in_board(nx, ny) && at(nx, ny) != color {
                            all_own = false;
                        }
                    }
                    unfillable = all_own;
                }
                // 死活判定：0 气 / 接触敌子且一口气 / 两气无眼（读子确认）
                let mut dead = libs == 0 || (contact && libs == 1 && !unfillable);
                if !dead && read_depth > 0 && contact && libs == 2 && eyes == 0 {
                    dead = read_cap(x as i32, y as i32, read_depth, true);
                }
                // 辐射强度（八分比）：随气数连续变化（避免 2↔3 气的跳变放大噪声），
                // 死子以对方身份全额辐射
                let base: i32 = if color == BLACK { 1 } else { -1 };
                let mut str8 = (4 + libs).min(9); // 1气→5, 2→6, 3→7, 4→8, 5+→9
                if libs == 1 {
                    str8 = 3; // 仅剩一口气（含填不掉的眼）几乎不辐射
                }
                if eyes >= 2 && libs >= 4 {
                    str8 = 9;
                }
                let radiate = if dead { -base * 8 } else { base * str8 };
                for i in 0..cnt {
                    let (gx, gy) = GBUF2[i];
                    let idx = gy as usize * s + gx as usize;
                    GSTRENGTH[idx] = radiate;
                    GDEAD[idx] = dead;
                }
            }
        }
    }
}

/// 逐子泛洪辐射：每颗棋子向外辐射随距离衰减的影响力，
/// 被对方棋子阻挡、可穿过己方棋子；同点黑白贡献代数相加（相互抵消）。
fn radiation() {
    unsafe {
        let s = size();
        for i in 0..s * s {
            INFLU[i] = 0;
        }
        for y in 0..s {
            for x in 0..s {
                let idx = y * s + x;
                let str8 = GSTRENGTH[idx];
                if str8 == 0 {
                    continue;
                }
                let radiate_color = if str8 > 0 { BLACK } else { WHITE };
                BFS_GEN = BFS_GEN.wrapping_add(1);
                let gen = BFS_GEN;
                let mut head = 0usize;
                let mut tail = 1usize;
                BFS_Q[0] = (x as i32, y as i32, 0);
                BFS_SEEN[idx] = gen;
                while head < tail {
                    let (cx, cy, d) = BFS_Q[head];
                    head += 1;
                    if d > 0 && at(cx, cy) == EMPTY {
                        INFLU[cy as usize * s + cx as usize] += RAY_W[(d - 1) as usize] * str8 / 8;
                    }
                    if d >= RAY_D {
                        continue;
                    }
                    for dd in 0..4 {
                        let nx = cx + DX[dd];
                        let ny = cy + DY[dd];
                        if !in_board(nx, ny) {
                            continue;
                        }
                        let nidx = ny as usize * s + nx as usize;
                        if BFS_SEEN[nidx] == gen {
                            continue;
                        }
                        let nv = at(nx, ny);
                        if nv == EMPTY || nv == radiate_color {
                            BFS_SEEN[nidx] = gen;
                            BFS_Q[tail] = (nx, ny, d + 1);
                            tail += 1;
                        }
                    }
                }
            }
        }
    }
}

/// 形态学确认（Bouzy 式腐蚀）：平台化后做 EROSION_ROUNDS 轮腐蚀。
/// 棋子与盘缘视为支撑；四周没有足够支撑的平台会被逐步腐蚀掉——
/// 孤子与朝开阔面宣称的「地盘」只剩势力标签，被围出的口袋得以保留。
fn morphology() {
    unsafe {
        let s = size();
        for i in 0..s * s {
            let v = INFLU[i];
            let mut p = if v >= T_PLATEAU {
                CAP_PLATEAU
            } else if v <= -T_PLATEAU {
                -CAP_PLATEAU
            } else {
                v
            };
            if BOARD[i] != EMPTY {
                // 棋子格：按辐射符号给满强度（死子已翻转为对方符号）
                p = if GSTRENGTH[i] > 0 {
                    CAP_PLATEAU
                } else if GSTRENGTH[i] < 0 {
                    -CAP_PLATEAU
                } else {
                    0
                };
            }
            ERODED[i] = p;
        }
        for _ in 0..EROSION_ROUNDS {
            for y in 0..s {
                for x in 0..s {
                    let i = y * s + x;
                    let v = ERODED[i];
                    if BOARD[i] != EMPTY || v == 0 {
                        INF_TMP[i] = v;
                        continue;
                    }
                    let mag = v.abs();
                    let sgn = v.signum();
                    let mut m = 0i32;
                    for d in 0..4 {
                        let nx = x as i32 + DX[d];
                        let ny = y as i32 + DY[d];
                        if !in_board(nx, ny) {
                            continue; // 盘缘 = 支撑
                        }
                        let qv = ERODED[ny as usize * s + nx as usize];
                        if qv == 0 || qv.signum() != sgn || qv.abs() < mag {
                            m += 1;
                        }
                    }
                    let nv = mag - m;
                    INF_TMP[i] = if nv <= 0 { 0 } else { sgn * nv };
                }
            }
            ERODED = INF_TMP;
        }
    }
}

/// 分类 + 形势估算（需先 scan_groups → radiation → morphology）。
/// 返回 (黑地, 白地, 黑势力点, 白势力点)，不含贴目。
/// 中国规则：地 = 确定地点数 + 活子数；日韩规则：地 = 确定地 + 提子 + 对方死子数。
fn classify_and_est() -> (i32, i32, i32, i32) {
    unsafe {
        let s = size();
        let mut terr_b = 0i32;
        let mut terr_w = 0i32;
        let mut infl_b = 0i32;
        let mut infl_w = 0i32;
        let mut dead_b = 0i32;
        let mut dead_w = 0i32;
        let mut stone_b = 0i32;
        let mut stone_w = 0i32;
        for i in 0..s * s {
            TERR[i] = 0;
            let stone = BOARD[i];
            if stone != EMPTY {
                if GDEAD[i] {
                    // 死子的占用点归对方所有（TERR 正 = 黑地）
                    if stone == BLACK {
                        dead_b += 1;
                        terr_w += 1;
                        TERR[i] = -2;
                    } else {
                        dead_w += 1;
                        terr_b += 1;
                        TERR[i] = 2;
                    }
                } else if stone == BLACK {
                    stone_b += 1;
                } else {
                    stone_w += 1;
                }
                continue;
            }
            let e = ERODED[i];
            let r = INFLU[i];
            let mut t = 0i8;
            if e >= T_TERR {
                t = 2;
            } else if e <= -T_TERR {
                t = -2;
            } else if r >= T_INFL {
                t = 1;
            } else if r <= -T_INFL {
                t = -1;
            }
            // 守卫（仅空点）：紧贴活棋的点不可能是对方的确定地，降级为势力
            if (t == 2 || t == -2) && stone == EMPTY {
                let owner = if t == 2 { BLACK } else { WHITE };
                let x = (i % s) as i32;
                let y = (i / s) as i32;
                for d in 0..4 {
                    let nx = x + DX[d];
                    let ny = y + DY[d];
                    if in_board(nx, ny) {
                        let nidx = ny as usize * s + nx as usize;
                        if BOARD[nidx] == 3 - owner && !GDEAD[nidx] {
                            t /= 2;
                            break;
                        }
                    }
                }
            }
            TERR[i] = t;
            match t {
                2 => terr_b += 1,
                -2 => terr_w += 1,
                1 => infl_b += 1,
                -1 => infl_w += 1,
                _ => {}
            }
        }
        let (est_b, est_w) = if RULE == RULE_CHINESE {
            (terr_b + stone_b, terr_w + stone_w)
        } else {
            (
                terr_b + CAPTURED_BY_BLACK + dead_w,
                terr_w + CAPTURED_BY_WHITE + dead_b,
            )
        };
        EST_B = est_b;
        EST_W = est_w;
        (est_b, est_w, infl_b, infl_w)
    }
}

/// 形势分析（泛洪辐射 + 形态学确认 + 读子死活）。参数仅为兼容旧接口，不再使用。
#[no_mangle]
pub extern "C" fn ugo_analyze(_d: i32, _e: i32) {
    unsafe {
        scan_groups(8, 600);
        radiation();
        morphology();
        classify_and_est();
    }
}

/// 分类图指针（i8，size*size）：±2 确定地 / ±1 势力 / 0 中立；死子格 = 对方地。
#[no_mangle]
pub extern "C" fn ugo_terr_ptr() -> *const i8 {
    unsafe { TERR.as_ptr() }
}

/// 形势缓冲指针（i32，size*size，净影响力，黑正白负）。
#[no_mangle]
pub extern "C" fn ugo_influence_ptr() -> *const i32 {
    unsafe { INFLU.as_ptr() }
}

/// 形势估算（需先调用 ugo_analyze）：黑方点数（不含贴目）。
#[no_mangle]
pub extern "C" fn ugo_est_black() -> i32 {
    unsafe { EST_B }
}

/// 形势估算（需先调用 ugo_analyze）：白方点数（不含贴目）。
#[no_mangle]
pub extern "C" fn ugo_est_white() -> i32 {
    unsafe { EST_W }
}

// ==================== AI 对手 ====================
//
// 架构（参考经典计算机围棋研究）：
//   1. 全盘棋块普查（气 / 眼 / 死活启发 + 征子级吃子搜索）
//   2. 泛洪辐射影响力评估（与「势力」显示同一模型）
//   3. 候选生成：战术急所（提子 / 救子 / 叫吃）+ 辐射前沿扩张点 + 布局线位
//   4. alpha-beta 负极大搜索（深度 / 宽度随强度 1-10 递增），叶子做全盘评估
//   5. 低强度加噪声；停一手 / 认输策略基于同一形势估算
//
// 评估单位：点 × 8（贴 7.5 → 60，贴 6.5 → 52）。

static mut AI_LEVEL: i32 = 0; // 0 = 关闭
static mut AI_W_ATK: i32 = 50;
static mut AI_W_DEF: i32 = 50;
static mut AI_W_TER: i32 = 50;
static mut AI_W_MOYO: i32 = 50;
static mut AI_SEED: u32 = 0x9E37_79B9;
static mut AI_ACTION: i32 = 0; // 0=停一手 1=落子 2=认输
static mut AI_X: i32 = -1;
static mut AI_Y: i32 = -1;

/// 扩张潜力图（辐射前沿的空缺程度）
static mut EXP_MAP: [i32; MAX * MAX] = [0; MAX * MAX];
/// 候选缓冲
static mut C_X: [i32; MAX * MAX] = [0; MAX * MAX];
static mut C_Y: [i32; MAX * MAX] = [0; MAX * MAX];
static mut C_SC: [i32; MAX * MAX] = [0; MAX * MAX];
/// 根节点候选的独立副本（搜索递归会复用 C_X/C_Y/C_SC）
static mut RC_X: [i32; MAX * MAX] = [0; MAX * MAX];
static mut RC_Y: [i32; MAX * MAX] = [0; MAX * MAX];
static mut RC_SC: [i32; MAX * MAX] = [0; MAX * MAX];
static mut RC_N: usize = 0;
/// 调试/决策：每个根候选的搜索值
static mut RC_V: [i32; MAX * MAX] = [0; MAX * MAX];
/// 最近一次完整深度层的候选值（近平局决胜用，避免用到不完整层的数据）
static mut RC_VB: [i32; MAX * MAX] = [0; MAX * MAX];
/// 搜索各深度层的候选副本（按 (depth, ext) 索引，互不干扰：
/// 延拓节点的 depth 会与栈上祖先重叠，必须带上延拓层数区分）
static mut SC_X: [[[i32; MAX * MAX]; 4]; 10] = [[[0; MAX * MAX]; 4]; 10];
static mut SC_Y: [[[i32; MAX * MAX]; 4]; 10] = [[[0; MAX * MAX]; 4]; 10];
/// 叫吃延拓的最大层数（打吃/提子局面在叶子上自动延长搜索）。
/// 实测延拓与叶子评估存在不对称失真（有战术的一方多拿深度，价值漂移可达数点），
/// 暂置 0 停用；机制保留，待评估函数更稳后再启用。
const MAX_EXT: i32 = 0;
/// 搜索参数（根节点按强度设定）
static mut CUR_DEPTH: i32 = 0;
static mut CUR_W: [usize; 4] = [0; 4];
static mut NODES: i32 = 0;
static mut NODE_BUDGET: i32 = 0;
static mut OPEN_TOTAL: i32 = 1;
/// 开局形状指导分（布局期候选加权：空角 / 守角 / 挂角 / 夹击）
static mut OPEN_B: [i32; MAX * MAX] = [0; MAX * MAX];

/// xorshift32 伪随机数（种子由前端每次 genmove 前注入）
fn rng_next() -> u32 {
    unsafe {
        let mut x = AI_SEED;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        AI_SEED = x;
        x
    }
}

/// 0..range 均匀随机整数
fn rng_below(range: i32) -> i32 {
    ((rng_next() >> 16) as i32).rem_euclid(range.max(1))
}

/// 角部坐标归一化：把 (x,y) 变换到以左上角为基准的 (a,b)（镜像变换，自反）
fn corner_norm(corner: u8, x: i32, y: i32, s: i32) -> (i32, i32) {
    let nx = if corner & 1 != 0 { s - 1 - x } else { x };
    let ny = if corner & 2 != 0 { s - 1 - y } else { y };
    (nx, ny)
}

/// 向 OPEN_B 记一笔开局形状分（只记空点，取较高者）
fn guide_add(corner: u8, a: i32, b: i32, pts: i32, s_i: i32) {
    unsafe {
        if a < 2 || b < 2 || a >= s_i || b >= s_i {
            return;
        }
        let (px, py) = corner_norm(corner, a, b, s_i);
        let idx = py as usize * s_i as usize + px as usize;
        if BOARD[idx] == EMPTY && OPEN_B[idx] < pts {
            OPEN_B[idx] = pts;
        }
    }
}

/// 开局形状指导（布局期）：逐角归一化分析，生成教科书式的开局形状分——
///   空角大场（4-4 / 3-4 / 3-3）、小飞/大飞守角、小飞/大飞挂角、一间夹。
/// 全部用相对几何（骑马步关系）表达，不依赖定式字典，任何路数通用。
fn compute_opening_guide(me: u8, factor: i32) {
    unsafe {
        let s = size();
        let s_i = s as i32;
        for i in 0..s * s {
            OPEN_B[i] = 0;
        }
        if factor <= 0 {
            return;
        }
        let opp = if me == BLACK { WHITE } else { BLACK };
        for corner in 0..4u8 {
            // 收集角部（归一化后 8×8 内）棋子
            let mut sa = [0i32; 40];
            let mut sb = [0i32; 40];
            let mut sc = [0u8; 40];
            let mut n = 0usize;
            'scan: for yy in 0..s {
                for xx in 0..s {
                    let v = at(xx as i32, yy as i32);
                    if v == EMPTY {
                        continue;
                    }
                    let (a, b) = corner_norm(corner, xx as i32, yy as i32, s_i);
                    if a < 8 && b < 8 {
                        if n >= 40 {
                            break 'scan;
                        }
                        sa[n] = a;
                        sb[n] = b;
                        sc[n] = v;
                        n += 1;
                    }
                }
            }
            if n == 0 {
                // 空角大场：4-4 / 两向 3-4 / 3-3
                guide_add(corner, 3, 3, 300, s_i);
                guide_add(corner, 2, 3, 290, s_i);
                guide_add(corner, 3, 2, 290, s_i);
                guide_add(corner, 2, 2, 260, s_i);
                continue;
            }
            for i in 0..n {
                let (a, b, v) = (sa[i], sb[i], sc[i]);
                // 角部核心子：3-3 / 3-4 / 4-4
                let is_core = (a == 2 || a == 3) && (b == 2 || b == 3);
                if !is_core {
                    continue;
                }
                // 与该核心子最近的对方子（切比雪夫距离）
                let mut opp_dist = 99i32;
                let mut opp_j = -1i32;
                for j in 0..n {
                    if sc[j] == opp {
                        let d = (sa[j] - a).abs().max((sb[j] - b).abs());
                        if d < opp_dist {
                            opp_dist = d;
                            opp_j = j as i32;
                        }
                    }
                }
                // 小飞/大飞外向点（守角与挂角共用形状集）
                let knights = [(2i32, 1i32), (1i32, 2i32), (3i32, 1i32), (1i32, 3i32)];
                if v == me {
                    if opp_dist > 3 {
                        // 守角
                        for &(da, db) in &knights {
                            guide_add(corner, a + da, b + db, 240, s_i);
                        }
                    } else {
                        // 应挂：转守另一侧 + 对来子一间夹
                        for &(da, db) in &knights {
                            guide_add(corner, a + da, b + db, 220, s_i);
                        }
                        if opp_j >= 0 {
                            let j = opp_j as usize;
                            let (dxa, dyb) = (sa[j] - a, sb[j] - b);
                            let knight = (dxa.abs() == 2 && dyb.abs() == 1)
                                || (dxa.abs() == 1 && dyb.abs() == 2)
                                || (dxa.abs() == 3 && dyb.abs() == 1)
                                || (dxa.abs() == 1 && dyb.abs() == 3);
                            if knight {
                                guide_add(corner, a + 2 * dxa, b + 2 * dyb, 200, s_i);
                            }
                        }
                    }
                } else {
                    // 对方角核：我方附近无子 → 挂角
                    let mut own_near = false;
                    for j in 0..n {
                        if sc[j] == me {
                            let d = (sa[j] - a).abs().max((sb[j] - b).abs());
                            if d <= 3 {
                                own_near = true;
                            }
                        }
                    }
                    if !own_near && opp_dist > 3 {
                        for &(da, db) in &knights {
                            guide_add(corner, a + da, b + db, 240, s_i);
                        }
                    }
                }
            }
        }
    }
}

/// 扩张潜力图：以每点为中心、半径 3 的菱形内，统计双方均未确认为地盘的
/// 净影响力空缺（越接近确定地阈值收益越高，双方已成地或对方成地处为 0）。
fn build_exp() {
    unsafe {
        let s = size();
        for i in 0..s * s {
            EXP_MAP[i] = 0;
        }
        for y in 0..s {
            for x in 0..s {
                let mut acc = 0i32;
                for dy in -3i32..=3 {
                    for dx in -3i32..=3 {
                        let d = dx.abs() + dy.abs();
                        if d == 0 || d > 3 {
                            continue;
                        }
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if !in_board(nx, ny) {
                            continue;
                        }
                        let v = INFLU[ny as usize * s + nx as usize];
                        let gain = (T_TERR + 2 - v.abs()).max(0);
                        acc += gain * (4 - d);
                    }
                }
                EXP_MAP[y * s + x] = acc;
            }
        }
    }
}

/// 生成并评估候选着点（用当前节点的普查 / 辐射数据），前 w 个按分数降序。
/// noise > 0 时给根节点候选加随机扰动（低强度用）。
/// tactical_only = true 时只保留带战术内容的着点（叫吃延拓节点用）。
/// 返回候选总数；C_X/C_Y/C_SC 前 w 项已排序。
fn gen_candidates(me: u8, w: usize, noise: i32, tactical_only: bool) -> usize {
    unsafe {
        let s = size();
        let opp = if me == BLACK { WHITE } else { BLACK };
        let mut n = 0usize;
        for y in 0..s {
            for x in 0..s {
                if at(x as i32, y as i32) != EMPTY {
                    continue;
                }
                if is_eye_for(x as i32, y as i32, me) {
                    continue;
                }
                let mut sc = 0i32;
                let mut cap_sz = 0i32; // 相邻叫吃敌块总子数
                let mut chase_sz = 0i32; // 相邻两气敌块
                let mut own_atari = false;
                let mut own_low = 0i32; // 相邻两气己块总子数
                let mut empty_nb = 0i32;
                let mut touch_enemy = false;
                for d in 0..4 {
                    let nx = x as i32 + DX[d];
                    let ny = y as i32 + DY[d];
                    if !in_board(nx, ny) {
                        continue;
                    }
                    let nidx = ny as usize * s + nx as usize;
                    let nv = at(nx, ny);
                    if nv == EMPTY {
                        empty_nb += 1;
                    } else if nv == opp {
                        touch_enemy = true;
                        if GLIB[nidx] == 1 {
                            cap_sz += GSZ[nidx];
                        } else if GLIB[nidx] == 2 {
                            chase_sz += GSZ[nidx];
                        }
                    } else {
                        if GLIB[nidx] == 1 {
                            own_atari = true;
                        } else if GLIB[nidx] == 2 {
                            own_low += GSZ[nidx];
                        }
                    }
                }
                if cap_sz > 0 {
                    // 立即提子：战术分必须压过布局线位分（~156），否则候选会被
                    // 排序挤出名单，出现「被打吃不应」的呆着
                    sc += (400 + 60 * cap_sz.min(12)) * (50 + AI_W_ATK) / 50;
                }
                if chase_sz > 0 {
                    sc += (40 + 10 * chase_sz.min(12)) * (30 + AI_W_ATK) / 80;
                }
                // 扩张 / 削减 / 线位：仅在常规节点参与排序（延拓节点只看战术）
                if !tactical_only {
                    // 扩张潜力（辐射前沿）
                    let eidx = y * s + x;
                    sc += EXP_MAP[eidx] * (AI_W_TER + AI_W_MOYO) / 3800;
                    // 削减价值：落子点对方势力越强，侵消/打入的价值越高
                    // （能否成活由搜索评估；估值让 AI 落后时知道去抢对方的地盘）
                    {
                        let sv = INFLU[eidx] * if me == BLACK { 1 } else { -1 };
                        if sv < 0 {
                            sc += (-sv).min(60) * (30 + AI_W_ATK) / 60;
                        }
                    }
                    // 布局线位修正（布局期）
                    let left = (OPEN_TOTAL - MOVE_COUNT).max(0);
                    if left > 0 {
                        // 开局形状指导（空角/守角/挂角/夹击）：
                        // 布局窗口前半程全额，后半程减半衰减
                        let factor = (200 * left / OPEN_TOTAL.max(1)).min(100);
                        compute_opening_guide(me, factor);
                        sc += OPEN_B[y * s + x] * factor / 100;
                        let xi = x as i32;
                        let yi = y as i32;
                        let l = xi.min(yi).min(s as i32 - 1 - xi).min(s as i32 - 1 - yi);
                        let lb = if l == 1 {
                            -12
                        } else if l == 2 || l == 3 {
                            26
                        } else if l == 4 {
                            10
                        } else {
                            0
                        };
                        sc += lb * 8 * left / OPEN_TOTAL.max(1);
                    }
                }
                // 试下精查：可能提子 / 救子 / 自叫吃风险
                if cap_sz > 0 || own_atari || own_low > 0
                    || (touch_enemy && empty_nb < 2) || empty_nb == 0 {
                    let (r, u) = try_play(x as i32, y as i32);
                    if r != OK {
                        continue; // 劫禁着等非法点
                    }
                    let caps = (CAP_TOP - u.cap_top) as i32;
                    let (gcnt, libs2, _) = group_info(x as i32, y as i32);
                    if libs2 == 1 && caps == 0 {
                        // 自叫吃：重罚（同样要压过线位分）
                        sc -= 200 + 30 * gcnt as i32;
                    } else if libs2 == 1 {
                        sc -= 60;
                    }
                    if (own_atari || own_low > 0) && libs2 >= 2 {
                        // 落子后连回的己方叫吃 / 两气块：救子分同样主导量级
                        let mut saved1 = 0i32;
                        let mut saved2 = 0i32;
                        for i in 0..gcnt {
                            let (gx, gy) = GBUF2[i];
                            let gi = gy as usize * s + gx as usize;
                            if ATARI[gy as usize][gx as usize] {
                                saved1 += GSZ[gi];
                            } else if GLIB[gi] == 2 {
                                saved2 += GSZ[gi];
                            }
                        }
                        if saved1 > 0 {
                            sc += (300 + 50 * saved1.min(12)) * (50 + AI_W_DEF) / 50;
                            if libs2 == 2 {
                                sc -= 16; // 救出但仍仅两气
                            }
                        } else if saved2 > 0 && libs2 >= 3 {
                            // 给被追的两气块续气成功
                            sc += (120 + 30 * saved2.min(12)) * (50 + AI_W_DEF) / 50;
                        }
                    }
                    untry_play(x as i32, y as i32, u);
                }
                if noise > 0 {
                    sc += rng_below(noise * 2 + 1) - noise;
                }
                // 延拓节点：只保留带战术内容的着点
                if tactical_only && sc <= 0 {
                    continue;
                }
                C_X[n] = x as i32;
                C_Y[n] = y as i32;
                C_SC[n] = sc;
                n += 1;
            }
        }
        // 部分选择排序：前 w 个最大
        let k = w.min(n);
        for i in 0..k {
            let mut bi = i;
            for j in (i + 1)..n {
                if C_SC[j] > C_SC[bi] {
                    bi = j;
                }
            }
            C_X.swap(i, bi);
            C_Y.swap(i, bi);
            C_SC.swap(i, bi);
        }
        n
    }
}

/// 叶子全盘评估（行棋方视角，点 × 8）。自行做带读子的棋块普查。
fn eval_leaf() -> i32 {
    unsafe {
        scan_groups(4, 150);
        eval_from_scan()
    }
}

/// 叶子评估的评估段（前提：scan_groups 已完成）。
fn eval_from_scan() -> i32 {
    unsafe {
        let mover = TURN;
        let s = size();
        radiation();
        morphology();
        let (est_b, est_w, infl_b, infl_w) = classify_and_est();
        // 区域估算 + 模样潜力（双方同函数，风格只改权重，保持零和）
        let mut raw = (est_b - est_w) * 8 + (infl_b - infl_w) * 8 * AI_W_MOYO / 600;
        raw -= if RULE == RULE_CHINESE { 60 } else { 52 };
        // 时序修正：轮到谁走，谁的叫吃子就危险 / 对方的叫吃子可提。
        // 注意死子组（GDEAD）也要进入本循环：行棋方的启发死子仍有一手机会
        let mut tempo = 0i32;
        for i in 0..s * s {
            let stone = BOARD[i];
            if stone == EMPTY {
                continue;
            }
            if GLIB[i] == 1 {
                let sz = GSZ[i].min(12);
                if GDEAD[i] {
                    // 启发式死子已按对方计点；死子方若正是行棋方，
                    // 下一手可能逃出/反提，返还大部分损失
                    if stone == mover {
                        tempo += 16 * sz;
                    }
                } else if stone == mover {
                    tempo -= 24 * sz; // 叫吃子下一手多半被提，按 3 点/子计损
                } else {
                    tempo += 16 * sz;
                }
            } else if GLIB[i] == 2 && GCONTACT[i] && !GDEAD[i] && stone == mover {
                // 行棋方的两气接触块处境危险（对方可继续追杀）
                tempo -= 6 * GSZ[i].min(12);
            }
        }
        raw + if mover == BLACK { tempo } else { -tempo }
    }
}

/// 负极大 + alpha-beta 深度受限搜索（含叫吃延拓）。返回行棋方视角评估（点 × 8）。
fn search(depth: i32, mut alpha: i32, beta: i32, ext: i32) -> i32 {
    unsafe {
        NODES += 1;
        if NODES > NODE_BUDGET {
            return eval_leaf();
        }
        if depth <= 0 {
            // 叫吃延拓：叶子上若存在叫吃（任一方），把战术序列搜完再评估，
            // 消除「下两手才见分晓的接触战」的视野盲区
            if ext >= MAX_EXT {
                return eval_leaf();
            }
            scan_groups(4, 150);
            let s = size();
            let mut any_atari = false;
            'outer: for y in 0..s {
                for x in 0..s {
                    if ATARI[y][x] {
                        any_atari = true;
                        break 'outer;
                    }
                }
            }
            if !any_atari {
                return eval_from_scan();
            }
            return search_body(1, alpha, beta, ext + 1, true, true);
        }
        search_body(depth, alpha, beta, ext, false, false)
    }
}

/// 搜索节点主体（候选生成 + 子节点循环）。
fn search_body(
    depth: i32,
    mut alpha: i32,
    beta: i32,
    ext: i32,
    tactical_only: bool,
    scan_done: bool,
) -> i32 {
    unsafe {
        let me = TURN;
        if !scan_done {
            scan_groups(0, 0);
        }
        radiation();
        build_exp();
        let wi = (CUR_DEPTH - depth).max(0) as usize;
        let width = if wi < 4 { CUR_W[wi] } else { 6 };
        if width == 0 {
            return if scan_done { eval_from_scan() } else { eval_leaf() };
        }
        let n = gen_candidates(me, width, 0, tactical_only);
        if n == 0 {
            return if scan_done { eval_from_scan() } else { eval_leaf() };
        }
        // 本层候选复制到 (depth, ext) 私有缓冲（子节点会改写 C_X/C_Y）
        let k = width.min(n);
        let di = depth as usize;
        let de = ext as usize;
        for i in 0..k {
            SC_X[di][de][i] = C_X[i];
            SC_Y[di][de][i] = C_Y[i];
        }
        let mut best = -1_000_000i32;
        for i in 0..k {
            let (r, u) = try_play(SC_X[di][de][i], SC_Y[di][de][i]);
            if r != OK {
                continue;
            }
            let v = -search(depth - 1, -beta, -alpha, ext);
            untry_play(SC_X[di][de][i], SC_Y[di][de][i], u);
            if v > best {
                best = v;
                if v > alpha {
                    alpha = v;
                }
                if alpha >= beta {
                    break;
                }
            }
        }
        best
    }
}

/// 配置 AI：level 0=关闭 1..10=强度；其余为风格权重 0..100。
#[no_mangle]
pub extern "C" fn ugo_ai_config(level: i32, atk: i32, def: i32, ter: i32, moyo: i32) {
    unsafe {
        AI_LEVEL = level.clamp(0, 10);
        AI_W_ATK = atk.clamp(0, 100);
        AI_W_DEF = def.clamp(0, 100);
        AI_W_TER = ter.clamp(0, 100);
        AI_W_MOYO = moyo.clamp(0, 100);
    }
}

/// 每次生成着法前由前端注入随机种子（时间戳），保证同一盘面不重复走棋。
#[no_mangle]
pub extern "C" fn ugo_ai_set_seed(seed: u32) {
    unsafe {
        AI_SEED = if seed == 0 { 0x9E37_79B9 } else { seed };
    }
}

#[no_mangle]
pub extern "C" fn ugo_ai_action() -> i32 {
    unsafe { AI_ACTION }
}

#[no_mangle]
pub extern "C" fn ugo_ai_x() -> i32 {
    unsafe { AI_X }
}

#[no_mangle]
pub extern "C" fn ugo_ai_y() -> i32 {
    unsafe { AI_Y }
}

/// 调试：根候选数 / 第 i 个候选 (x*100+y)*100000+排序分 / 搜索值
#[no_mangle]
pub extern "C" fn ugo_dbg_n() -> i32 {
    unsafe { RC_N as i32 }
}
#[no_mangle]
pub extern "C" fn ugo_dbg_xy(i: i32) -> i32 {
    unsafe {
        if (i as usize) < RC_N {
            (RC_X[i as usize] * 100 + RC_Y[i as usize]) * 100000 + RC_SC[i as usize]
        } else {
            -1
        }
    }
}
#[no_mangle]
pub extern "C" fn ugo_dbg_v(i: i32) -> i32 {
    unsafe {
        if (i as usize) < RC_N {
            RC_VB[i as usize]
        } else {
            -1
        }
    }
}

/// 调试：根节点指定坐标的排序分与搜索值（x*100+y）
#[no_mangle]
pub extern "C" fn ugo_dbg_probe(x: i32, y: i32) -> i32 {
    unsafe {
        for i in 0..RC_N {
            if RC_X[i as usize] == x && RC_Y[i as usize] == y {
                return RC_VB[i as usize] * 100000 + RC_SC[i as usize];
            }
        }
        -1
    }
}

/// 调试：最近一次 genmove 的搜索节点数
#[no_mangle]
pub extern "C" fn ugo_dbg_nodes() -> i32 {
    unsafe { NODES }
}

/// 调试：当前盘面的叶子评估（行棋方视角，点×8）
#[no_mangle]
pub extern "C" fn ugo_dbg_eval() -> i32 {
    eval_leaf()
}

/// 生成 AI 着法（不落子，只给出建议；前端用 ugo_play/ugo_pass 执行）。
/// 返回动作：0=停一手 1=落子（坐标读 ugo_ai_x/ugo_ai_y）2=认输。
#[no_mangle]
pub extern "C" fn ugo_ai_genmove() -> i32 {
    unsafe {
        AI_ACTION = 0;
        AI_X = -1;
        AI_Y = -1;
        let s = size();
        let s_i = s as i32;
        if AI_LEVEL < 1 || s < 2 {
            return AI_ACTION;
        }
        let me = TURN;
        let persp: i32 = if me == BLACK { 1 } else { -1 };

        // 强度参数表：
        // (搜索深度, 根宽, 二层宽, 三层宽, 四层宽, 根噪声(点×8), 读子深度, 节点预算)
        // 高强度借助迭代加深与叫吃延拓，实际战术深度高于名义深度
        let (depth, w0, w1, w2, w3, noise, read_d, budget) = match AI_LEVEL {
            1 => (1, 10usize, 0usize, 0, 0, 160i32, 0i32, 300i32),
            2 => (1, 14, 0, 0, 0, 96, 2, 400),
            3 => (2, 12, 8, 0, 0, 64, 4, 700),
            4 => (2, 14, 9, 0, 0, 40, 5, 900),
            5 => (3, 14, 10, 8, 0, 24, 6, 1600),
            6 => (3, 18, 12, 9, 0, 12, 7, 2400),
            7 => (4, 14, 11, 8, 0, 0, 8, 3600),
            8 => (4, 18, 13, 9, 0, 4, 9, 5500),
            9 => (5, 16, 12, 9, 6, 3, 10, 8000),
            _ => (5, 22, 16, 12, 9, 2, 12, 12000),
        };
        CUR_DEPTH = depth;
        CUR_W = [w0, w1, w2, w3];
        NODES = 0;
        NODE_BUDGET = budget;
        OPEN_TOTAL = (s_i * s_i / 5).max(1);

        // 根节点形势（读子确认死活，含征子）
        scan_groups(read_d, 600);
        radiation();
        morphology();
        let (est_b, est_w, infl_b, infl_w) = classify_and_est();
        let komi8 = if RULE == RULE_CHINESE { 60 } else { 52 };
        let lead_me = ((est_b - est_w) * 8 + (infl_b - infl_w) * 8 * AI_W_MOYO / 600 - komi8) * persp;

        // 中后盘大幅落后 → 认输
        if MOVE_COUNT > s_i * s_i * 3 / 5 && lead_me < -200 {
            AI_ACTION = 2;
            return AI_ACTION;
        }
        // 对手停一手且己方领先 → 停一手终局
        let opp_passed = MOVE_COUNT > 0 && LAST_X == -1;
        if opp_passed && MOVE_COUNT > s_i * s_i / 2 && lead_me > 0 {
            AI_ACTION = 0;
            return AI_ACTION;
        }

        // 强制战术应答：叫吃不应是最伤棋的行为——己方叫吃（读子确认逃得掉）
        // 或敌方叫吃（可提）时，只在战术点中选；逃不掉的（征子不利等）才弃子
        let mut f_n = 0usize;
        let mut f_x = [0i32; 8];
        let mut f_y = [0i32; 8];
        {
            let opp = if me == BLACK { WHITE } else { BLACK };
            READ_OPS = 0;
            'outer: for yy in 0..s {
                for xx in 0..s {
                    if f_n >= 8 {
                        break 'outer;
                    }
                    let stone = BOARD[yy * s + xx];
                    if stone == EMPTY {
                        continue;
                    }
                    if stone == me && ATARI[yy][xx] {
                        // 防守先走仍被吃（征子不利等）→ 弃子不救
                        if read_cap(xx as i32, yy as i32, read_d.max(4), false) {
                            continue;
                        }
                        let (_, libs, _) = group_info(xx as i32, yy as i32);
                        if libs < 1 {
                            continue;
                        }
                        let (lx, ly) = LIB_PTS[0];
                        let (r, u) = try_play(lx, ly);
                        if r == OK {
                            let (_, l2, _) = group_info(lx, ly);
                            let caps = (CAP_TOP - u.cap_top) as i32;
                            untry_play(lx, ly, u);
                            // 逃出后至少两气 / 或吃掉对方子解围，才算真逃点
                            if l2 >= 2 || caps > 0 {
                                let mut dup = false;
                                for k in 0..f_n {
                                    if f_x[k] == lx && f_y[k] == ly {
                                        dup = true;
                                    }
                                }
                                if !dup {
                                    f_x[f_n] = lx;
                                    f_y[f_n] = ly;
                                    f_n += 1;
                                }
                            }
                        }
                    } else if stone == opp && ATARI[yy][xx] {
                        let (_, libs, _) = group_info(xx as i32, yy as i32);
                        if libs < 1 {
                            continue;
                        }
                        let (lx, ly) = LIB_PTS[0];
                        let mut dup = false;
                        for k in 0..f_n {
                            if f_x[k] == lx && f_y[k] == ly {
                                dup = true;
                            }
                        }
                        if !dup {
                            f_x[f_n] = lx;
                            f_y[f_n] = ly;
                            f_n += 1;
                        }
                    }
                }
            }
        }

        if f_n > 0 {
            for i in 0..f_n {
                RC_X[i] = f_x[i];
                RC_Y[i] = f_y[i];
                RC_SC[i] = 10000;
                RC_V[i] = 0;
                RC_VB[i] = 0;
            }
            RC_N = f_n;
        } else {
            build_exp();
            let n = gen_candidates(me, w0, noise, false);
            if n == 0 {
                return AI_ACTION; // 无合法着点 → 停一手
            }
            // 根候选复制到独立缓冲（后续搜索递归会改写 C_X/C_Y）
            let k = w0.min(n);
            for i in 0..k {
                RC_X[i] = C_X[i];
                RC_Y[i] = C_Y[i];
                RC_SC[i] = C_SC[i];
                RC_V[i] = 0;
            }
            RC_N = k;
        }

        // 根：迭代加深 + 上一层最优优先（经典 ID 思路，同样预算下有效深度更高、
        // 剪枝更强）。预算耗尽时保留上一完整层的结果。
        let start_depth = if depth >= 3 { 2 } else { depth };
        let mut pd = start_depth;
        let mut best_i = 0usize;
        let mut best_v = -1_000_000i32;
        while pd <= depth {
            CUR_DEPTH = pd;
            if pd > start_depth {
                // 上一层搜索值降序（n ≤ 32，选择排序即可）
                for i in 0..RC_N {
                    let mut bi = i;
                    for j in (i + 1)..RC_N {
                        if RC_V[j] > RC_V[bi] {
                            bi = j;
                        }
                    }
                    if bi != i {
                        RC_X.swap(i, bi);
                        RC_Y.swap(i, bi);
                        RC_SC.swap(i, bi);
                        RC_V.swap(i, bi);
                    }
                }
            }
            for i in 0..RC_N {
                RC_V[i] = -2_000_000;
            }
            let mut bv = -1_000_000i32;
            let mut bi2 = 0usize;
            for i in 0..RC_N {
                let (r, u) = try_play(RC_X[i], RC_Y[i]);
                if r != OK {
                    continue;
                }
                let v = -search(pd - 1, -1_000_000, -bv, 0);
                untry_play(RC_X[i], RC_Y[i], u);
                RC_V[i] = v;
                if v > bv {
                    bv = v;
                    bi2 = i;
                }
            }
            if NODES > NODE_BUDGET && pd < depth {
                break; // 本层未搜完：沿用上一层结果
            }
            best_v = bv;
            best_i = bi2;
            // 保存本完整层各候选值，供近平局决胜使用
            for i in 0..RC_N {
                RC_VB[i] = RC_V[i];
            }
            pd += 1;
        }
        // 近平局决胜：与最佳值差窗口内的候选取排序分最高者。
        // 战术着（提/救/逃，排序分 ≥ 300）窗口放宽到 2 点——棋子安全优先于
        // 少量实地；普通着 0.5 点（评估噪声量级，体现布局期线位/形状偏好）
        for i in 0..RC_N {
            let window = if RC_SC[i] >= 300 { 16 } else { 4 };
            if RC_VB[i] >= best_v - window && RC_SC[i] > RC_SC[best_i] {
                best_i = i;
            }
        }

        // 停一手判定：用「落子前后的即时地盘增益」判断还有没有有利可图的着点
        // （数子规则下填中立点 +1，填自己的地 0；数目规则下填中立点 0，填自己的地 -1）
        if MOVE_COUNT > s_i * s_i / 3 {
            let my_est_now = if me == BLACK { est_b } else { est_w };
            // 先查搜索最佳点的立即增益
            let mut gain_x = -1i32;
            let mut gain_y = -1i32;
            let mut best_gain = -1_000_000i32;
            {
                let bx = RC_X[best_i];
                let by = RC_Y[best_i];
                let (r, u) = try_play(bx, by);
                if r == OK {
                    scan_groups(0, 0);
                    radiation();
                    morphology();
                    let (eb, ew, _, _) = classify_and_est();
                    untry_play(bx, by, u);
                    best_gain = (if me == BLACK { eb } else { ew }) - my_est_now;
                    gain_x = bx;
                    gain_y = by;
                }
            }
            if best_gain <= 0 {
                // 全盘扫一遍：找还有正增益的着点（填中立点 / 提子 / 收官）
                best_gain = 0;
                gain_x = -1;
                for yy in 0..s {
                    for xx in 0..s {
                        if at(xx as i32, yy as i32) != EMPTY || is_eye_for(xx as i32, yy as i32, me) {
                            continue;
                        }
                        let (r, u) = try_play(xx as i32, yy as i32);
                        if r != OK {
                            continue;
                        }
                        scan_groups(0, 0);
                        radiation();
                        morphology();
                        let (eb, ew, _, _) = classify_and_est();
                        untry_play(xx as i32, yy as i32, u);
                        let g = (if me == BLACK { eb } else { ew }) - my_est_now;
                        if g > best_gain {
                            best_gain = g;
                            gain_x = xx as i32;
                            gain_y = yy as i32;
                        }
                    }
                }
            }
            if gain_x < 0 {
                return AI_ACTION; // 无增益着点 → 停一手
            }
            if best_gain <= 0 {
                // 搜索最佳点无增益：改走扫描出的增益点
                AI_ACTION = 1;
                AI_X = gain_x;
                AI_Y = gain_y;
                return AI_ACTION;
            }
        }
        AI_ACTION = 1;
        AI_X = RC_X[best_i];
        AI_Y = RC_Y[best_i];
        AI_ACTION
    }
}
