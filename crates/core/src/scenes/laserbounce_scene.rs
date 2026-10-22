//! Laser Bounce minigame. Rotate the triangular mirrors until the laser
//! bounces from the emitter into the goal, both of which sit above the grid.
//!
//! Boards are built from a solved beam path walked down from the emitter,
//! plus decoy mirrors on cells the path never touches. Every mirror is then
//! rotated at random, with the path mirrors always knocked out of place.
//!
//! Every few wins at one difficulty, the boards step up to the next.
//!
//! The 14x6 grid at 9px pitch sits under a 10px band for the emitter, goal
//! and move counter.

use core::fmt::Write as _;

use embedded_graphics::prelude::{Point, Size};
use heapless::{String, Vec};
#[cfg(not(feature = "desktop"))]
use micromath::F32Ext;
use crate::t;

use crate::{
    context::{GameContext, StatId},
    input::{Button, Buttons},
    rand::{rand_bool, rand_range_f32, xorshift32},
    render::Renderer,
    scene::{Scene, SceneId},
    ui::burst::{BurstEffect, DEFAULT_STYLE},
    ui::menu::{Menu, MenuItem, MenuResult},
};

const COLS: usize = 14;
const ROWS: usize = 6;
const CELLS: usize = COLS * ROWS;

const PITCH: i32 = 9;
const GRID_X: i32 = 1;
const GRID_Y: i32 = 10;

const NONE: u8 = 0xFF;

const WIN_RESET_DELAY: f32 = 4.0;
/// Wins at one difficulty before the boards get harder.
const WINS_PER_STEP: u32 = 3;
const WIN_SPARKLES: usize = 8;
const WIN_SPARKLE_STAGGER: f32 = 0.1;
const MAX_TURNS: usize = 64;
const GEN_ATTEMPTS: usize = 1000;
/// Fewest columns between the emitter and the goal.
const MIN_GOAL_GAP: i32 = 4;

// Dust drifting through the beam, drawn as dark specks on it.
const MAX_MOTES: usize = 4;
/// Average new motes per second.
const MOTE_RATE: f32 = 3.0;
/// Drift speed range in px per second.
const MOTE_SPEED: (f32, f32) = (4.0, 12.0);
/// Lifetime range in seconds.
const MOTE_LIFE: (f32, f32) = (0.4, 1.2);

// Travel directions, clockwise so a right turn is `(d + 1) % 4`.
const RIGHT: u8 = 0;
const DOWN: u8 = 1;
const LEFT: u8 = 2;
const UP: u8 = 3;

const fn opposite(d: u8) -> u8 {
    (d + 2) % 4
}

const fn delta_row(d: u8) -> i32 {
    match d {
        DOWN => 1,
        UP => -1,
        _ => 0,
    }
}

const fn delta_col(d: u8) -> i32 {
    match d {
        RIGHT => 1,
        LEFT => -1,
        _ => 0,
    }
}

/// The two sides of the cell the mirrored face looks out of, by rotation.
/// The rotation is the corner holding the right angle, going clockwise:
/// bottom-left, top-left, top-right, bottom-right.
const OPEN: [(u8, u8); 4] = [(UP, RIGHT), (DOWN, RIGHT), (DOWN, LEFT), (UP, LEFT)];

/// Direction a beam travelling `dir` leaves a mirror in, or `None` if it
/// hits the mirror's back.
fn reflect(rot: u8, dir: u8) -> Option<u8> {
    let entry = opposite(dir);
    let (a, b) = OPEN[rot as usize];
    if entry == a {
        Some(b)
    } else if entry == b {
        Some(a)
    } else {
        None
    }
}

/// Rotation that turns a beam travelling `from` to travel `to`.
fn rot_for(from: u8, to: u8) -> u8 {
    (0..4).find(|&r| reflect(r, from) == Some(to)).unwrap_or(0)
}

fn in_grid(row: i32, col: i32) -> bool {
    row >= 0 && col >= 0 && row < ROWS as i32 && col < COLS as i32
}

fn idx(row: i32, col: i32) -> usize {
    row as usize * COLS + col as usize
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Difficulty {
    Easy,
    Normal,
    Hard,
}

impl Difficulty {
    /// Mirrors on the solved path. The beam goes down and comes back up, so
    /// it always takes an even number of turns.
    fn path_mirrors(self) -> (u32, u32) {
        match self {
            Difficulty::Easy => (8, 10),
            Difficulty::Normal => (10, 14),
            Difficulty::Hard => (14, 18),
        }
    }

    fn decoys(self) -> usize {
        match self {
            Difficulty::Easy => 16,
            Difficulty::Normal => 22,
            Difficulty::Hard => 28,
        }
    }

    /// Chance in 100 that the path walk turns at a free cell.
    fn turn_pct(self) -> u32 {
        match self {
            Difficulty::Easy => 30,
            Difficulty::Normal => 35,
            Difficulty::Hard => 45,
        }
    }

    fn harder(self) -> Self {
        match self {
            Difficulty::Easy => Difficulty::Normal,
            _ => Difficulty::Hard,
        }
    }

    fn coins(self) -> i32 {
        match self {
            Difficulty::Easy => 2,
            Difficulty::Normal => 3,
            Difficulty::Hard => 5,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LaserBounceAction {
    NewBoard,
    Easy,
    Normal,
    Hard,
}

const DIFFICULTY_ITEMS: &[MenuItem<LaserBounceAction>] = &[
    MenuItem { label: t!("Easy"),   icon: None, submenu: None, action: Some(LaserBounceAction::Easy),   confirm: None, confirm_on_vacation: None },
    MenuItem { label: t!("Normal"), icon: None, submenu: None, action: Some(LaserBounceAction::Normal), confirm: None, confirm_on_vacation: None },
    MenuItem { label: t!("Hard"),   icon: None, submenu: None, action: Some(LaserBounceAction::Hard),   confirm: None, confirm_on_vacation: None },
];

const OPTIONS_ITEMS: &[MenuItem<LaserBounceAction>] = &[
    MenuItem { label: t!("New Board"),  icon: None, submenu: None,                   action: Some(LaserBounceAction::NewBoard), confirm: None, confirm_on_vacation: None },
    MenuItem { label: t!("Difficulty"), icon: None, submenu: Some(DIFFICULTY_ITEMS), action: None,                              confirm: None, confirm_on_vacation: None },
];

/// Where the beam went. It stops on the `end_dir` side of the cell at
/// `end`, which is row -1 when it never got into the grid.
#[derive(Clone)]
struct Trace {
    turns: Vec<(i8, i8), MAX_TURNS>,
    end: (i8, i8),
    end_dir: u8,
    hit: bool,
}

// Path walk cell flags.
const PASS_H: u8 = 1;
const PASS_V: u8 = 2;
const MIRROR: u8 = 4;

struct Board {
    /// Mirror rotation per cell, or `NONE`.
    mirror: [u8; CELLS],
    /// Solved rotation of each path mirror, or `NONE`.
    solution: [u8; CELLS],
    emitter: usize,
    goal: usize,
}

impl Board {
    const fn new() -> Self {
        Self {
            mirror: [NONE; CELLS],
            solution: [NONE; CELLS],
            emitter: 0,
            goal: 1,
        }
    }

    fn generate(difficulty: Difficulty, rng: &mut u32) -> Self {
        let mut b = Self::solved(difficulty, rng);
        b.scramble(rng);
        b
    }

    /// A board in its solved layout, decoys included.
    fn solved(difficulty: Difficulty, rng: &mut u32) -> Self {
        let mut b = Self::new();
        let mut flags = [0u8; CELLS];
        let mut ok = false;
        for _ in 0..GEN_ATTEMPTS {
            if b.walk(difficulty, &mut flags, rng) {
                ok = true;
                break;
            }
        }
        if !ok {
            b.fallback(&mut flags);
        }
        b.solution = b.mirror;
        b.add_decoys(difficulty.decoys(), &flags, rng);
        b
    }

    /// Walk a beam down from a random emitter, placing a mirror at each turn,
    /// until it leaves the top of the grid somewhere else.
    fn walk(&mut self, difficulty: Difficulty, flags: &mut [u8; CELLS], rng: &mut u32) -> bool {
        *self = Self::new();
        *flags = [0; CELLS];
        let (lo, hi) = difficulty.path_mirrors();
        let e = (xorshift32(rng) as usize % COLS) as i32;
        self.emitter = e as usize;

        // Leaving the grid at (row, col) only counts out the top, well away
        // from the emitter, with enough turns taken.
        let finishes =
            |row: i32, col: i32, turns: u32| row < 0 && (col - e).abs() >= MIN_GOAL_GAP && turns >= lo;

        let (mut row, mut col, mut dir) = (-1i32, e, DOWN);
        let mut turns = 0u32;
        for _ in 0..CELLS * 3 {
            let (nr, nc) = (row + delta_row(dir), col + delta_col(dir));
            if !in_grid(nr, nc) {
                if finishes(nr, nc, turns) {
                    self.goal = nc as usize;
                    return true;
                }
                return false;
            }
            let i = idx(nr, nc);
            let axis = if dir == LEFT || dir == RIGHT { PASS_H } else { PASS_V };

            // A move is fine if it finishes the path, or lands on a cell the
            // walk can go on from: one with room beyond it to turn or carry on.
            let move_ok = |d: u8, turns: u32| {
                let (r1, c1) = (nr + delta_row(d), nc + delta_col(d));
                if !in_grid(r1, c1) {
                    return finishes(r1, c1, turns);
                }
                let (r2, c2) = (r1 + delta_row(d), c1 + delta_col(d));
                in_grid(r2, c2) || finishes(r2, c2, turns)
            };
            // Mirror cells can't be passed again, and neither can a cell
            // already crossed along this axis.
            let straight = flags[i] & (axis | MIRROR) == 0 && move_ok(dir, turns);
            let mut opts: Vec<u8, 2> = Vec::new();
            // No turning with the edge straight ahead, so every path mirror
            // has room beyond it for a decoy that could feed it backwards.
            let room_beyond = in_grid(nr + delta_row(dir), nc + delta_col(dir));
            if flags[i] == 0 && turns < hi && room_beyond {
                for d in [(dir + 1) % 4, (dir + 3) % 4] {
                    if move_ok(d, turns + 1) {
                        let _ = opts.push(d);
                    }
                }
            }

            let ahead = (nr + delta_row(dir), nc + delta_col(dir));
            let turn = if opts.is_empty() {
                None
            } else if straight && !in_grid(ahead.0, ahead.1) {
                // Straight on finishes the path.
                None
            } else if !straight || xorshift32(rng) % 100 < difficulty.turn_pct() {
                Some(opts[xorshift32(rng) as usize % opts.len()])
            } else {
                None
            };

            match turn {
                Some(d) => {
                    self.mirror[i] = rot_for(dir, d);
                    flags[i] |= MIRROR;
                    turns += 1;
                    dir = d;
                }
                None if straight => flags[i] |= axis,
                None => return false,
            }
            row = nr;
            col = nc;
        }
        false
    }

    /// Down one column, across and back up, for when the walk keeps failing.
    fn fallback(&mut self, flags: &mut [u8; CELLS]) {
        *self = Self::new();
        *flags = [0; CELLS];
        self.emitter = 1;
        self.goal = COLS - 2;
        for r in 0..2 {
            flags[idx(r, 1)] |= PASS_V;
            flags[idx(r, COLS as i32 - 2)] |= PASS_V;
        }
        for c in 2..COLS as i32 - 2 {
            flags[idx(2, c)] |= PASS_H;
        }
        self.mirror[idx(2, 1)] = rot_for(DOWN, RIGHT);
        self.mirror[idx(2, COLS as i32 - 2)] = rot_for(RIGHT, UP);
        flags[idx(2, 1)] |= MIRROR;
        flags[idx(2, COLS as i32 - 2)] |= MIRROR;
    }

    /// Put decoy mirrors on cells the solved beam never enters, as false
    /// branches off the path, so it can't be followed from either end.
    ///
    /// Forwards: a path mirror turned the wrong way sends the beam down a
    /// ray with a decoy on it. Backwards: a path mirror could also send the
    /// beam on its way when fed from the opposite side, so a decoy goes on
    /// that ray too. Either way, a decoy turns the beam to or from both sides,
    /// so each one opens two more rays. Branches are grown breadth first, so
    /// every path mirror gets its fakes before any branch gets longer.
    fn add_decoys(&mut self, count: usize, flags: &[u8; CELLS], rng: &mut u32) {
        // Path mirrors in beam order, as (cell, dir in, dir out).
        let mut turns: Vec<(u8, u8, u8), MAX_TURNS> = Vec::new();
        let (mut row, mut col, mut dir) = (-1i32, self.emitter as i32, DOWN);
        for _ in 0..CELLS * 4 {
            let (nr, nc) = (row + delta_row(dir), col + delta_col(dir));
            if !in_grid(nr, nc) {
                break;
            }
            let i = idx(nr, nc);
            if self.mirror[i] != NONE {
                match reflect(self.mirror[i], dir) {
                    Some(d) => {
                        let _ = turns.push((i as u8, dir, d));
                        dir = d;
                    }
                    None => break,
                }
            }
            row = nr;
            col = nc;
        }

        // Rays to put decoys on, as (cell, dir). Backward fakes are seeded
        // from the goal end and forward ones from the emitter end, taking
        // turns so both ends are covered first.
        let mut rays: Vec<(u8, u8), 160> = Vec::new();
        let n = turns.len();
        for k in 0..n {
            let (i, din, _) = turns[n - 1 - k];
            // Fed from the far side along the same line.
            let _ = rays.push((i, din));
            let (i, _, dout) = turns[k];
            // The wrong turn.
            let _ = rays.push((i, opposite(dout)));
        }

        let mut placed = 0;
        let mut head = 0;
        while placed < count && head < rays.len() {
            let (from, d) = rays[head];
            head += 1;
            // Free cells along the ray, up to the first mirror or the edge.
            let mut free: Vec<u8, { if COLS > ROWS { COLS } else { ROWS } }> = Vec::new();
            let (mut r, mut c) = ((from as usize / COLS) as i32, (from as usize % COLS) as i32);
            loop {
                r += delta_row(d);
                c += delta_col(d);
                if !in_grid(r, c) || self.mirror[idx(r, c)] != NONE {
                    break;
                }
                if flags[idx(r, c)] == 0 {
                    let _ = free.push(idx(r, c) as u8);
                }
            }
            if free.is_empty() {
                continue;
            }
            // Lean towards the far end so a wrong turn runs a while before it
            // shows itself.
            let far = (xorshift32(rng) as usize % free.len()).max(xorshift32(rng) as usize % free.len());
            let j = free[far];
            self.mirror[j as usize] = (xorshift32(rng) % 4) as u8;
            placed += 1;
            let _ = rays.push((j, (d + 1) % 4));
            let _ = rays.push((j, (d + 3) % 4));
        }

        // Once the branches run out of room, the rest go anywhere free.
        let mut pool = [0u8; CELLS];
        let mut len = 0;
        for i in 0..CELLS {
            if flags[i] == 0 && self.mirror[i] == NONE {
                pool[len] = i as u8;
                len += 1;
            }
        }
        // Partial Fisher-Yates: the first slots get decoys.
        for k in 0..(count - placed).min(len) {
            let j = k + xorshift32(rng) as usize % (len - k);
            pool.swap(k, j);
            self.mirror[pool[k] as usize] = (xorshift32(rng) % 4) as u8;
        }
    }

    /// Spin every mirror, knocking each path mirror out of place, until the
    /// beam no longer reaches the goal.
    fn scramble(&mut self, rng: &mut u32) {
        for _ in 0..20 {
            for i in 0..CELLS {
                if self.solution[i] != NONE {
                    self.mirror[i] = (self.solution[i] + 1 + (xorshift32(rng) % 3) as u8) % 4;
                } else if self.mirror[i] != NONE {
                    self.mirror[i] = (xorshift32(rng) % 4) as u8;
                }
            }
            if !self.trace().hit {
                return;
            }
        }
    }

    #[cfg(test)]
    fn mirror_count(&self) -> usize {
        self.mirror.iter().filter(|&&m| m != NONE).count()
    }

    /// Follow the beam from the emitter. Mirrors reflect it one-to-one, so
    /// it can't loop, but the step cap guards against that anyway.
    fn trace(&self) -> Trace {
        let mut t = Trace { turns: Vec::new(), end: (-1, self.emitter as i8), end_dir: DOWN, hit: false };
        let (mut row, mut col, mut dir) = (-1i32, self.emitter as i32, DOWN);
        for _ in 0..CELLS * 4 {
            let (nr, nc) = (row + delta_row(dir), col + delta_col(dir));
            if !in_grid(nr, nc) {
                t.hit = nr < 0 && nc == self.goal as i32;
                break;
            }
            let m = self.mirror[idx(nr, nc)];
            if m != NONE {
                match reflect(m, dir) {
                    Some(d) => {
                        let _ = t.turns.push((nr as i8, nc as i8));
                        dir = d;
                    }
                    None => break,
                }
            }
            row = nr;
            col = nc;
        }
        t.end = (row as i8, col as i8);
        t.end_dir = dir;
        t
    }
}

fn cell_origin(row: i32, col: i32) -> Point {
    Point::new(GRID_X + col * PITCH, GRID_Y + row * PITCH)
}

fn cell_centre(row: i32, col: i32) -> Point {
    cell_origin(row, col) + Point::new(PITCH / 2, PITCH / 2)
}

/// Filled right triangle inset 1px in its cell, right angle in the corner
/// picked by `rot`. Its hypotenuse runs through the cell centre.
fn draw_mirror(r: &mut Renderer, row: i32, col: i32, rot: u8) {
    let o = cell_origin(row, col);
    let (x0, x1) = (o.x + 1, o.x + PITCH - 2);
    for i in 0..PITCH - 2 {
        let y = o.y + 1 + i;
        let (a, b) = match rot {
            0 => (x0, x0 + i),
            1 => (x0, x1 - i),
            2 => (x0 + i, x1),
            _ => (x1 - i, x1),
        };
        r.draw_line(Point::new(a, y), Point::new(b, y));
    }
}

/// Where `dist` px along the beam falls.
fn point_along(points: &[Point], dist: i32) -> Option<Point> {
    let mut left = dist;
    for w in points.windows(2) {
        let d = w[1] - w[0];
        let len = d.x.abs() + d.y.abs();
        if left <= len {
            return Some(w[0] + Point::new(d.x.signum() * left, d.y.signum() * left));
        }
        left -= len;
    }
    None
}

fn beam_length(points: &[Point]) -> f32 {
    points.windows(2).map(|w| (w[1] - w[0]).x.abs() + (w[1] - w[0]).y.abs()).sum::<i32>() as f32
}

/// A speck of dust drifting along the beam. Dead once `life` runs out.
#[derive(Clone, Copy)]
struct Mote {
    dist: f32,
    speed: f32,
    life: f32,
    size: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Playing,
    Win,
}

pub struct LaserBounceScene {
    board: Board,
    beam: Trace,
    motes: [Mote; MAX_MOTES],
    /// Played round the goal on a hit.
    sparkles: BurstEffect,
    difficulty: Difficulty,
    /// Wins at the current difficulty.
    streak: u32,
    cursor: usize,
    moves: u32,
    state: State,
    timer: f32,

    session_wins: u32,
    session_coins: i32,

    options_menu: Menu<LaserBounceAction>,
    menu_active: bool,
}

impl LaserBounceScene {
    pub fn new() -> Self {
        let board = Board::new();
        let beam = board.trace();
        Self {
            board,
            beam,
            motes: [Mote { dist: 0.0, speed: 0.0, life: 0.0, size: 1 }; MAX_MOTES],
            sparkles: BurstEffect::new(),
            difficulty: Difficulty::Easy,
            streak: 0,
            cursor: 0,
            moves: 0,
            state: State::Playing,
            timer: 0.0,

            session_wins: 0,
            session_coins: 0,

            options_menu: Menu::new(OPTIONS_ITEMS),
            menu_active: false,
        }
    }

    fn new_board(&mut self, ctx: &mut GameContext) {
        let mut rng = ctx.rng;
        self.board = Board::generate(self.difficulty, &mut rng);
        ctx.rng = rng;
        self.beam = self.board.trace();
        self.sparkles = BurstEffect::new();
        self.cursor = self.board.emitter;
        self.moves = 0;
        self.state = State::Playing;
        self.timer = 0.0;
    }

    fn rotate(&mut self, steps: u8, rng: &mut u32) {
        let m = &mut self.board.mirror[self.cursor];
        if *m == NONE {
            return;
        }
        *m = (*m + steps) % 4;
        self.moves += 1;
        self.beam = self.board.trace();
        if self.beam.hit {
            self.session_wins += 1;
            self.session_coins += self.difficulty.coins();
            self.state = State::Win;
            self.timer = 0.0;
            // The goal is on the top edge, so they spread sideways and down.
            self.sparkles.trigger_with(rng, WIN_SPARKLES, DEFAULT_STYLE, -14.0, 14.0, -2.0, 14.0, WIN_SPARKLE_STAGGER);
            // The next board steps up after a few wins.
            self.streak += 1;
            if self.streak >= WINS_PER_STEP && self.difficulty != Difficulty::Hard {
                self.difficulty = self.difficulty.harder();
                self.streak = 0;
            }
        }
    }

    fn update_motes(&mut self, dt: f32, rng: &mut u32) {
        let len = beam_length(&self.beam_points());
        for m in self.motes.iter_mut() {
            if m.life <= 0.0 {
                continue;
            }
            m.dist += m.speed * dt;
            m.life -= dt;
            if m.dist < 0.0 || m.dist > len {
                m.life = 0.0;
            }
        }
        if rand_bool(rng, MOTE_RATE * dt) {
            if let Some(m) = self.motes.iter_mut().find(|m| m.life <= 0.0) {
                let speed = rand_range_f32(rng, MOTE_SPEED.0, MOTE_SPEED.1);
                *m = Mote {
                    dist: rand_range_f32(rng, 0.0, len),
                    speed: if rand_bool(rng, 0.5) { speed } else { -speed },
                    life: rand_range_f32(rng, MOTE_LIFE.0, MOTE_LIFE.1),
                    size: if rand_bool(rng, 0.3) { 2 } else { 1 },
                };
            }
        }
    }

    fn handle_menu(&mut self, ctx: &mut GameContext, buttons: &mut Buttons) {
        match self.options_menu.handle_input(buttons, false) {
            MenuResult::Continue => {}
            MenuResult::Closed => self.menu_active = false,
            MenuResult::Action(action) => {
                self.menu_active = false;
                if action != LaserBounceAction::NewBoard {
                    self.streak = 0;
                }
                match action {
                    LaserBounceAction::NewBoard => {}
                    LaserBounceAction::Easy => self.difficulty = Difficulty::Easy,
                    LaserBounceAction::Normal => self.difficulty = Difficulty::Normal,
                    LaserBounceAction::Hard => self.difficulty = Difficulty::Hard,
                }
                self.new_board(ctx);
            }
        }
    }

    /// Emitter and goal boxes with rails out to the edges, and the move
    /// counter in the widest gap left over.
    fn draw_band(&self, r: &mut Renderer) {
        let ex = cell_origin(0, self.board.emitter as i32).x;
        let gx = cell_origin(0, self.board.goal as i32).x;
        let box_w = (PITCH - 2) as u32;

        // Emitter: solid box with a nozzle.
        r.draw_rect(Point::new(ex + 1, 0), Size::new(box_w, 6), true);
        r.draw_rect(Point::new(ex + 3, 6), Size::new(3, 2), true);
        // Goal: hollow until lit, with a funnel underneath.
        r.draw_rect(Point::new(gx + 1, 0), Size::new(box_w, 6), self.beam.hit);
        r.draw_line(Point::new(gx + 3, 6), Point::new(gx + 3, 7));
        r.draw_line(Point::new(gx + 5, 6), Point::new(gx + 5, 7));

        let (lo, hi) = if ex < gx { (ex, gx) } else { (gx, ex) };
        // Gaps as (start, end) px: left of both, between, right of both.
        let gaps = [(0, lo), (lo + PITCH, hi), (hi + PITCH, 128)];

        let mut n: String<8> = String::new();
        let _ = write!(n, "{}", self.moves);
        let mut label: String<32> = String::new();
        crate::i18n::substitute(&mut label, t!("Moves: {n}"), &[("n", n.as_str())]);

        let widest = (0..3).max_by_key(|&k| gaps[k].1 - gaps[k].0).unwrap_or(1);
        let (g0, g1) = gaps[widest];
        let room = g1 - g0 - 4;
        let text = if label.chars().count() as i32 * 4 <= room { label.as_str() } else { n.as_str() };
        let w = text.chars().count() as i32 * 4;
        if w <= room {
            r.draw_text_small(text, Point::new(g0 + (g1 - g0 - w) / 2, 1));
        }

        // Rails on the outer sides, unless the counter took that side.
        for (k, &(a, b)) in [gaps[0], gaps[2]].iter().enumerate() {
            if k * 2 == widest || b - a < 2 {
                continue;
            }
            let (a, b) = if k == 0 { (a, b - 1) } else { (a + 1, b - 1) };
            r.draw_line(Point::new(a, 2), Point::new(b, 2));
            r.draw_line(Point::new(a, 4), Point::new(b, 4));
        }
    }

    /// Corners of the beam from the emitter nozzle to wherever it stops.
    fn beam_points(&self) -> Vec<Point, { MAX_TURNS + 2 }> {
        let t = &self.beam;
        let mut pts: Vec<Point, { MAX_TURNS + 2 }> = Vec::new();
        let start = Point::new(cell_centre(0, self.board.emitter as i32).x, 8);
        let _ = pts.push(start);
        for &(row, col) in t.turns.iter() {
            let _ = pts.push(cell_centre(row as i32, col as i32));
        }
        let prev = *pts.last().unwrap_or(&start);
        let (row, col) = (t.end.0 as i32, t.end.1 as i32);
        let next_row = row + delta_row(t.end_dir);
        let end = if next_row < 0 && t.end_dir == UP {
            // Out the top: into the goal or emitter if one is there.
            let y = if t.hit {
                6
            } else if col == self.board.emitter as i32 {
                8
            } else {
                0
            };
            Point::new(prev.x, y)
        } else {
            let half = PITCH / 2 + 1;
            cell_centre(row, col)
                + Point::new(delta_col(t.end_dir) * half, delta_row(t.end_dir) * half)
        };
        let _ = pts.push(end);
        pts
    }

    fn draw_beam(&self, r: &mut Renderer) {
        let pts = self.beam_points();
        for w in pts.windows(2) {
            r.draw_line(w[0], w[1]);
        }
        for m in self.motes.iter().filter(|m| m.life > 0.0) {
            for k in 0..m.size {
                if let Some(p) = point_along(&pts, m.dist as i32 + k) {
                    r.draw_pixel(p, false);
                }
            }
        }
    }

    fn draw_grid(&self, r: &mut Renderer) {
        for i in 0..CELLS {
            let m = self.board.mirror[i];
            if m != NONE {
                draw_mirror(r, (i / COLS) as i32, (i % COLS) as i32, m);
            }
        }
        if self.state == State::Playing {
            let o = cell_origin((self.cursor / COLS) as i32, (self.cursor % COLS) as i32);
            r.draw_rect(o, Size::new(PITCH as u32, PITCH as u32), false);
        }
    }
}

impl Scene for LaserBounceScene {
    fn enter(&mut self, ctx: &mut GameContext) {
        self.session_wins = 0;
        self.session_coins = 0;
        self.menu_active = false;
        self.new_board(ctx);
    }

    fn exit(&mut self, ctx: &mut GameContext) {
        if self.session_wins == 0 {
            return;
        }
        let scale = (self.session_wins as f32 / 5.0).sqrt();
        ctx.apply_stat_changes(&[
            (StatId::Intelligence, 5.0 * scale),
            (StatId::Focus,        5.0 * scale),
            (StatId::Loyalty,      0.5 * scale),
        ]);
        if self.session_coins > 0 {
            ctx.coins += self.session_coins;
        }
    }

    fn update(
        &mut self,
        ctx: &mut GameContext,
        buttons: &mut Buttons,
        dt: f32,
    ) -> Option<SceneId> {
        if buttons.was_just_pressed(Button::Menu1) {
            return Some(SceneId::Menu);
        }

        if self.menu_active {
            self.handle_menu(ctx, buttons);
            return None;
        }

        let mut rng = ctx.rng;
        self.update_motes(dt, &mut rng);
        ctx.rng = rng;
        self.sparkles.update(dt);

        if self.state == State::Win {
            self.timer += dt;
            if buttons.was_just_pressed(Button::A) || self.timer >= WIN_RESET_DELAY {
                self.new_board(ctx);
            }
            return None;
        }

        if buttons.was_just_pressed(Button::Menu2) {
            self.menu_active = true;
            self.options_menu.reset_to(OPTIONS_ITEMS);
            return None;
        }

        let row = self.cursor / COLS;
        let col = self.cursor % COLS;
        if buttons.was_just_pressed(Button::Up) && row > 0 {
            self.cursor -= COLS;
        } else if buttons.was_just_pressed(Button::Down) && row < ROWS - 1 {
            self.cursor += COLS;
        } else if buttons.was_just_pressed(Button::Left) && col > 0 {
            self.cursor -= 1;
        } else if buttons.was_just_pressed(Button::Right) && col < COLS - 1 {
            self.cursor += 1;
        }

        let mut rng = ctx.rng;
        if buttons.was_just_pressed(Button::A) {
            self.rotate(1, &mut rng);
        } else if buttons.was_just_pressed(Button::B) {
            self.rotate(3, &mut rng);
        }
        ctx.rng = rng;

        None
    }

    fn draw(&self, _ctx: &GameContext, renderer: &mut Renderer, _dt_ms: u64) {
        if self.menu_active {
            self.options_menu.draw(renderer);
            return;
        }

        self.draw_beam(renderer);
        self.draw_grid(renderer);
        self.draw_band(renderer);
        let goal = cell_centre(0, self.board.goal as i32);
        self.sparkles.draw(renderer, Point::new(goal.x, 3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_turn_from_the_front_and_block_from_the_back() {
        // Right angle bottom-left: the face looks up and right.
        assert_eq!(reflect(0, DOWN), Some(RIGHT));
        assert_eq!(reflect(0, LEFT), Some(UP));
        assert_eq!(reflect(0, UP), None);
        assert_eq!(reflect(0, RIGHT), None);
        for rot in 0..4 {
            let mut turned = 0;
            for dir in 0..4 {
                if let Some(out) = reflect(rot, dir) {
                    turned += 1;
                    assert!(out == (dir + 1) % 4 || out == (dir + 3) % 4);
                    // Beams are reversible.
                    assert_eq!(reflect(rot, opposite(out)), Some(opposite(dir)));
                }
            }
            assert_eq!(turned, 2);
        }
    }

    #[test]
    fn generated_boards_are_solvable_and_scrambled() {
        for difficulty in [Difficulty::Easy, Difficulty::Normal, Difficulty::Hard] {
            let (lo, hi) = difficulty.path_mirrors();
            for seed in 1..300u32 {
                let mut rng = seed.wrapping_mul(2654435761);
                let mut b = Board::solved(difficulty, &mut rng);
                assert_ne!(b.emitter, b.goal);
                let path = b.solution.iter().filter(|&&m| m != NONE).count() as u32;
                assert!((lo..=hi).contains(&path), "seed {} path {}", seed, path);
                assert_eq!(b.mirror_count(), path as usize + difficulty.decoys());
                assert!(b.trace().hit, "seed {} unsolved", seed);

                b.scramble(&mut rng);
                assert!(!b.trace().hit, "seed {} still solved", seed);
                for i in 0..CELLS {
                    if b.solution[i] != NONE {
                        assert_ne!(b.mirror[i], b.solution[i]);
                    }
                }
            }
        }
    }
}
