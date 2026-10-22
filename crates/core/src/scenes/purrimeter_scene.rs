//! Purrimeter, a dots & boxes minigame. Take turns with the pet drawing lines
//! between dots; closing a box scores it and earns another turn. The board
//! grows from 5x5 to 7x9 boxes as the player wins.
//!
//! Lines are drawn in two steps: pick a dot, then a direction to connect it.

use embedded_graphics::prelude::{Point, Size};
use heapless::String;
#[cfg(not(feature = "desktop"))]
use micromath::F32Ext;
use crate::t;

use crate::{
    assets::character::PoseId,
    context::{GameContext, StatId},
    entities::character::Character,
    input::{Button, Buttons},
    rand::rand_range_u32,
    render::Renderer,
    scene::{Scene, SceneId},
    ui::popup::Popup,
};

const MAX_ROWS: usize = 7;
const MAX_COLS: usize = 9;
const H_EDGES: usize = (MAX_ROWS + 1) * MAX_COLS;
const V_EDGES: usize = MAX_ROWS * (MAX_COLS + 1);
const EDGES: usize = H_EDGES + V_EDGES;

// Board geometry: dots 8px apart, centred in the area left of the score panel.
const PITCH: i32 = 8;
const BOARD_AREA_W: i32 = 75;
const BOARD_AREA_H: i32 = 64;

// Box owners.
const EMPTY: u8 = 0;
const PLAYER: u8 = 1;
const PET: u8 = 2;

const PET_THINK_DELAY: f32 = 0.5;
const END_DELAY: f32 = 0.5;
/// How long the pet's latest line flashes, and the flash half-period.
const LAST_MOVE_FLASH: f32 = 0.9;
const BLINK_HALF: f32 = 0.15;

/// Pets above this intelligence never throw away boxes on purpose.
const SMART_INTELLIGENCE: f32 = 75.0;
/// Chance a casual pet plays any line instead of a safe one.
const CASUAL_RANDOM_PCT: u32 = 15;
/// Chance range (scaled by intelligence) that the pet plays the endgame cleverly.
const MIN_DEAL_PCT: u32 = 20;
const MAX_DEAL_PCT: u32 = 90;

/// Board size in boxes (rows, cols) for a level (1-based).
fn level_params(level: u32) -> (usize, usize) {
    match level {
        0 | 1 => (5, 5),
        2 => (5, 6),
        3 => (6, 6),
        4 => (6, 7),
        5 => (7, 7),
        6 => (7, 8),
        _ => (7, 9),
    }
}

/// Lines are packed into one index: horizontal lines first, then vertical,
/// both with fixed strides so the layout doesn't depend on board size.
fn h_edge(r: usize, c: usize) -> usize {
    r * MAX_COLS + c
}

fn v_edge(r: usize, c: usize) -> usize {
    H_EDGES + r * (MAX_COLS + 1) + c
}

#[derive(Clone, Copy)]
struct Board {
    rows: usize,
    cols: usize,
    lines: [bool; EDGES],
    owner: [u8; MAX_ROWS * MAX_COLS],
}

impl Board {
    fn new(rows: usize, cols: usize) -> Self {
        Self { rows, cols, lines: [false; EDGES], owner: [EMPTY; MAX_ROWS * MAX_COLS] }
    }

    fn is_valid(&self, e: usize) -> bool {
        if e < H_EDGES {
            e / MAX_COLS <= self.rows && e % MAX_COLS < self.cols
        } else {
            let i = e - H_EDGES;
            i / (MAX_COLS + 1) < self.rows && i % (MAX_COLS + 1) <= self.cols
        }
    }

    /// Every undrawn line, in index order.
    fn free_lines(&self, out: &mut [u8; EDGES]) -> usize {
        let mut len = 0;
        for e in 0..EDGES {
            if self.is_valid(e) && !self.lines[e] {
                out[len] = e as u8;
                len += 1;
            }
        }
        len
    }

    fn sides(&self, r: usize, c: usize) -> u8 {
        self.lines[h_edge(r, c)] as u8
            + self.lines[h_edge(r + 1, c)] as u8
            + self.lines[v_edge(r, c)] as u8
            + self.lines[v_edge(r, c + 1)] as u8
    }

    /// The (up to two) boxes a line borders.
    fn boxes_of(&self, e: usize) -> [Option<(usize, usize)>; 2] {
        if e < H_EDGES {
            let (r, c) = (e / MAX_COLS, e % MAX_COLS);
            [
                if r > 0 { Some((r - 1, c)) } else { None },
                if r < self.rows { Some((r, c)) } else { None },
            ]
        } else {
            let i = e - H_EDGES;
            let (r, c) = (i / (MAX_COLS + 1), i % (MAX_COLS + 1));
            [
                if c > 0 { Some((r, c - 1)) } else { None },
                if c < self.cols { Some((r, c)) } else { None },
            ]
        }
    }

    /// Boxes an undrawn line would close.
    fn closes(&self, e: usize) -> u8 {
        self.boxes_of(e).iter().flatten().filter(|&&(r, c)| self.sides(r, c) == 3).count() as u8
    }

    /// Whether an undrawn line would leave a box with three sides.
    fn gives_box(&self, e: usize) -> bool {
        self.boxes_of(e).iter().flatten().any(|&(r, c)| self.sides(r, c) == 2)
    }

    /// Draw a line for `who`, returning how many boxes it closed.
    fn draw_line(&mut self, e: usize, who: u8) -> u8 {
        self.lines[e] = true;
        let mut closed = 0;
        for (r, c) in self.boxes_of(e).into_iter().flatten() {
            if self.sides(r, c) == 4 && self.owner[r * MAX_COLS + c] == EMPTY {
                self.owner[r * MAX_COLS + c] = who;
                closed += 1;
            }
        }
        closed
    }

    fn count(&self, who: u8) -> u32 {
        self.owner.iter().filter(|&&o| o == who).count() as u32
    }

    fn is_full(&self) -> bool {
        (self.count(PLAYER) + self.count(PET)) as usize == self.rows * self.cols
    }

    /// Keep closing boxes for `who` until none are left to close.
    fn greedy_take(&mut self, who: u8) -> u32 {
        let mut taken = 0;
        let mut free = [0u8; EDGES];
        loop {
            let len = self.free_lines(&mut free);
            match free[..len].iter().find(|&&f| self.closes(f as usize) > 0) {
                Some(&f) => taken += self.draw_line(f as usize, who) as u32,
                None => return taken,
            }
        }
    }

    /// Boxes the opponent collects after the pet draws `e`, if they greedily
    /// keep closing boxes until none are left to close.
    fn boxes_given(&self, e: usize) -> u32 {
        let mut b = *self;
        b.draw_line(e, PET);
        b.greedy_take(PLAYER)
    }

    fn is_free(&self, e: usize) -> bool {
        self.is_valid(e) && !self.lines[e]
    }

    fn has_safe_line(&self) -> bool {
        (0..EDGES).any(|e| self.is_free(e) && !self.gives_box(e))
    }

    /// The fewest boxes the mover can hand over with their next line.
    fn min_give(&self) -> u32 {
        (0..EDGES).filter(|&e| self.is_free(e)).map(|e| self.boxes_given(e)).min().unwrap_or(0)
    }

    fn three_sided(&self) -> usize {
        (0..self.rows)
            .flat_map(|r| (0..self.cols).map(move |c| (r, c)))
            .filter(|&(r, c)| self.sides(r, c) == 3)
            .count()
    }

    /// Whether the boxes marked in `region` form one piece joined by undrawn lines.
    fn is_connected(&self, region: &[bool; MAX_ROWS * MAX_COLS]) -> bool {
        let Some(start) = region.iter().position(|&b| b) else { return true };
        let mut seen = [false; MAX_ROWS * MAX_COLS];
        let mut stack = [0usize; MAX_ROWS * MAX_COLS];
        let mut len = 1;
        stack[0] = start;
        seen[start] = true;
        while len > 0 {
            len -= 1;
            let (r, c) = (stack[len] / MAX_COLS, stack[len] % MAX_COLS);
            let neighbours = [
                (r > 0).then(|| (r - 1, c, h_edge(r, c))),
                (r + 1 < self.rows).then(|| (r + 1, c, h_edge(r + 1, c))),
                (c > 0).then(|| (r, c - 1, v_edge(r, c))),
                (c + 1 < self.cols).then(|| (r, c + 1, v_edge(r, c + 1))),
            ];
            for (nr, nc, e) in neighbours.into_iter().flatten() {
                let i = nr * MAX_COLS + nc;
                if !self.lines[e] && region[i] && !seen[i] {
                    seen[i] = true;
                    stack[len] = i;
                    len += 1;
                }
            }
        }
        region.iter().zip(seen.iter()).all(|(&r, &s)| !r || s)
    }
}

fn pick_random(picks: &[u8], rng: &mut u32) -> usize {
    picks[rand_range_u32(rng, 0, (picks.len() - 1) as u32) as usize] as usize
}

/// The all-but-two trick: once only the end of a chain (2 boxes) or loop
/// (4 boxes) is left to take and every other line gives boxes away, hand those
/// last boxes over so the opponent has to open the next chain instead.
fn double_deal(board: &Board) -> Option<usize> {
    let mut taken = *board;
    let cap = taken.greedy_take(PET);
    let mut region = [false; MAX_ROWS * MAX_COLS];
    for (i, r) in region.iter_mut().enumerate() {
        *r = board.owner[i] == EMPTY && taken.owner[i] != EMPTY;
    }
    let openings = (0..EDGES).filter(|&e| board.is_free(e) && board.closes(e) > 0).count();
    let loop_end = cap == 4 && openings >= 2 && board.is_connected(&region);
    if !(cap == 2 || loop_end) || taken.is_full() || taken.has_safe_line() {
        return None;
    }

    let mut best = None;
    let mut best_value = cap as i32 - taken.min_give() as i32;
    for d in 0..EDGES {
        if !board.is_free(d) || board.closes(d) > 0 {
            continue;
        }
        if !board.boxes_of(d).iter().flatten().any(|&(r, c)| region[r * MAX_COLS + c]) {
            continue;
        }
        let mut b = *board;
        b.draw_line(d, PET);
        if b.greedy_take(PLAYER) != cap {
            continue;
        }
        // The player then has to open something for the pet.
        let value = b.min_give() as i32 - cap as i32;
        if value > best_value {
            best_value = value;
            best = Some(d);
        }
    }
    best
}

/// Pick the pet's next line: close boxes when it can, otherwise play safe,
/// otherwise give away as little of a chain as possible. `deal_pct` is the
/// chance it plays the endgame cleverly (double-deals and handouts).
fn pick_pet_line(board: &Board, random_pct: u32, deal_pct: u32, rng: &mut u32) -> Option<usize> {
    let mut free = [0u8; EDGES];
    let len = board.free_lines(&mut free);
    if len == 0 {
        return None;
    }
    let free = &free[..len];
    let dealing = deal_pct > 0 && rand_range_u32(rng, 1, 100) <= deal_pct;

    let mut picks = [0u8; EDGES];
    let mut n = 0;
    // Free boxes first, preferring lines that close two at once.
    let best_close = free.iter().map(|&e| board.closes(e as usize)).max().unwrap_or(0);
    if best_close > 0 {
        if let Some(d) = double_deal(board).filter(|_| dealing) {
            return Some(d);
        }
        for &e in free {
            if board.closes(e as usize) == best_close {
                picks[n] = e;
                n += 1;
            }
        }
        return Some(pick_random(&picks[..n], rng));
    }

    // Casual pets sometimes don't look at what they're handing over.
    if random_pct > 0 && rand_range_u32(rng, 1, 100) <= random_pct {
        return Some(pick_random(free, rng));
    }

    for &e in free {
        if !board.gives_box(e as usize) {
            picks[n] = e;
            n += 1;
        }
    }
    if n > 0 {
        return Some(pick_random(&picks[..n], rng));
    }

    // Every line gives something away; pick the smallest chain. A clever pet
    // opens it so more boxes are three-sided (a 2-chain in the middle), which
    // leaves the player no last pair to hand back.
    let mut fewest = u32::MAX;
    let mut most_open = 0;
    for &e in free {
        let given = board.boxes_given(e as usize);
        let open = if dealing {
            let mut b = *board;
            b.draw_line(e as usize, PET);
            b.three_sided()
        } else {
            0
        };
        if given < fewest || (given == fewest && open > most_open) {
            fewest = given;
            most_open = open;
            n = 0;
        }
        if given == fewest && open == most_open {
            picks[n] = e;
            n += 1;
        }
    }
    Some(pick_random(&picks[..n], rng))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    PlayerTurn,
    PetTurn,
    PlayerWin,
    PetWin,
    Draw,
}

pub struct PurrimeterScene {
    character: Character,
    result_popup: Popup,

    level: u32,
    round_number: u32,
    player_score: f32,
    pet_score: f32,
    intelligence: f32,

    board: Board,
    /// The dot under the cursor, and whether it is picked so the arrows
    /// choose which neighbour to connect it to.
    cursor_row: usize,
    cursor_col: usize,
    dot_selected: bool,
    last_pet_line: Option<usize>,
    last_pet_timer: f32,
    blink_timer: f32,
    pet_think_timer: f32,
    end_delay_timer: f32,
    state: State,
}

impl PurrimeterScene {
    pub fn new() -> Self {
        let mut scene = Self {
            character: Character::new(Point::new(104, 63)),
            result_popup: Popup::new(14, 0, 100, 40),

            level: 1,
            round_number: 0,
            player_score: 0.0,
            pet_score: 0.0,
            intelligence: 0.0,

            board: Board::new(5, 5),
            cursor_row: 0,
            cursor_col: 0,
            dot_selected: false,
            last_pet_line: None,
            last_pet_timer: 0.0,
            blink_timer: 0.0,
            pet_think_timer: 0.0,
            end_delay_timer: 0.0,
            state: State::PlayerTurn,
        };
        scene.reset_game();
        scene
    }

    fn reset_game(&mut self) {
        let (rows, cols) = level_params(self.level);
        self.board = Board::new(rows, cols);
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.last_pet_line = None;
        self.pet_think_timer = 0.0;
        self.end_delay_timer = 0.0;

        self.character.set_pose(PoseId::LayingSideNeutral);

        if self.round_number % 2 == 0 {
            self.start_player_turn();
        } else {
            self.start_pet_turn();
        }
    }

    /// The line from the dot at (`row`, `col`) towards `dir`, if it stays on the board.
    fn line_from(&self, row: usize, col: usize, dir: Button) -> Option<usize> {
        match dir {
            Button::Up if row > 0 => Some(v_edge(row - 1, col)),
            Button::Down if row < self.board.rows => Some(v_edge(row, col)),
            Button::Left if col > 0 => Some(h_edge(row, col - 1)),
            Button::Right if col < self.board.cols => Some(h_edge(row, col)),
            _ => None,
        }
    }

    /// Undrawn lines from the dot at (`row`, `col`).
    fn free_from(&self, row: usize, col: usize) -> impl Iterator<Item = usize> + '_ {
        [Button::Up, Button::Down, Button::Left, Button::Right]
            .into_iter()
            .filter_map(move |d| self.line_from(row, col, d))
            .filter(|&e| !self.board.lines[e])
    }

    fn start_player_turn(&mut self) {
        self.state = State::PlayerTurn;
        self.dot_selected = false;
    }

    fn start_pet_turn(&mut self) {
        self.state = State::PetTurn;
        self.pet_think_timer = 0.0;
    }

    /// Hand the turn over after `mover` drew a line closing `closed` boxes.
    fn advance_turn(&mut self, mover: u8, closed: u8) {
        if self.board.is_full() {
            self.finish_game();
        } else if (mover == PLAYER) == (closed > 0) {
            self.start_player_turn();
        } else {
            self.start_pet_turn();
        }
    }

    fn finish_game(&mut self) {
        let player = self.board.count(PLAYER);
        let pet = self.board.count(PET);
        self.end_delay_timer = 0.0;
        if player > pet {
            self.state = State::PlayerWin;
            self.player_score += 1.0;
            self.character.set_pose(PoseId::LayingSideAnnoyed);
        } else if pet > player {
            self.state = State::PetWin;
            self.pet_score += 1.0;
            self.character.set_pose(PoseId::LayingSideHappy);
        } else {
            self.state = State::Draw;
            self.player_score += 0.5;
            self.pet_score += 0.5;
        }
        self.set_end_popup(player, pet);
    }

    fn set_end_popup(&mut self, player: u32, pet: u32) {
        use core::fmt::Write as _;
        let mut msg: String<64> = String::new();
        let _ = msg.push_str(match self.state {
            State::PlayerWin => t!("You Win!"),
            State::PetWin => t!("You Lose!"),
            _ => t!("Draw!"),
        });
        let _ = write!(msg, "\n{}-{}\n", player, pet);
        let _ = msg.push_str(t!("A: New Game"));
        self.result_popup.set_text(msg.as_str(), false, true);
    }

    fn player_draw_line(&mut self, e: usize) {
        let closed = self.board.draw_line(e, PLAYER);
        self.advance_turn(PLAYER, closed);
    }

    fn make_pet_move(&mut self, rng: &mut u32) {
        let random_pct = if self.intelligence <= SMART_INTELLIGENCE { CASUAL_RANDOM_PCT } else { 0 };
        let deal_pct = (self.intelligence as u32).clamp(MIN_DEAL_PCT, MAX_DEAL_PCT);
        if let Some(e) = pick_pet_line(&self.board, random_pct, deal_pct, rng) {
            let closed = self.board.draw_line(e, PET);
            self.last_pet_line = Some(e);
            self.last_pet_timer = 0.0;
            self.advance_turn(PET, closed);
        }
    }

    fn update_player_turn(&mut self, buttons: &mut Buttons) {
        let dir = [Button::Up, Button::Down, Button::Left, Button::Right]
            .into_iter()
            .find(|&b| buttons.was_just_pressed(b));

        if self.dot_selected {
            // Second step: an arrow draws towards that neighbour; A or B backs out.
            if let Some(e) = dir.and_then(|d| self.line_from(self.cursor_row, self.cursor_col, d)) {
                if !self.board.lines[e] {
                    self.dot_selected = false;
                    self.player_draw_line(e);
                }
            } else if buttons.was_just_pressed(Button::A) || buttons.was_just_pressed(Button::B) {
                self.dot_selected = false;
            }
            return;
        }

        match dir {
            Some(Button::Up) if self.cursor_row > 0 => self.cursor_row -= 1,
            Some(Button::Down) if self.cursor_row < self.board.rows => self.cursor_row += 1,
            Some(Button::Left) if self.cursor_col > 0 => self.cursor_col -= 1,
            Some(Button::Right) if self.cursor_col < self.board.cols => self.cursor_col += 1,
            _ => {}
        }
        if buttons.was_just_pressed(Button::A)
            && self.free_from(self.cursor_row, self.cursor_col).next().is_some()
        {
            self.dot_selected = true;
        }
    }

    fn origin(&self) -> (i32, i32) {
        let w = self.board.cols as i32 * PITCH + 1;
        let h = self.board.rows as i32 * PITCH + 1;
        ((BOARD_AREA_W - w) / 2, (BOARD_AREA_H - h) / 2)
    }

    /// Screen endpoints of a line.
    fn line_ends(&self, e: usize) -> (Point, Point) {
        let (ox, oy) = self.origin();
        let (r, c, dr, dc) = if e < H_EDGES {
            (e / MAX_COLS, e % MAX_COLS, 0, 1)
        } else {
            let i = e - H_EDGES;
            (i / (MAX_COLS + 1), i % (MAX_COLS + 1), 1, 0)
        };
        let x = ox + c as i32 * PITCH;
        let y = oy + r as i32 * PITCH;
        (Point::new(x, y), Point::new(x + dc * PITCH, y + dr * PITCH))
    }

    fn blink_on(&self) -> bool {
        (self.blink_timer / BLINK_HALF) as u32 % 2 == 0
    }

    fn draw_board(&self, r: &mut Renderer) {
        let (ox, oy) = self.origin();
        let flashing = self.last_pet_line.filter(|_| self.last_pet_timer < LAST_MOVE_FLASH);

        for e in 0..EDGES {
            if !self.board.is_valid(e) {
                continue;
            }
            let (a, b) = self.line_ends(e);
            // The pet's latest line blinks.
            if self.board.lines[e] && !(flashing == Some(e) && !self.blink_on()) {
                r.draw_line(a, b);
            }
        }

        for row in 0..=self.board.rows as i32 {
            for col in 0..=self.board.cols as i32 {
                r.draw_pixel(Point::new(ox + col * PITCH, oy + row * PITCH), true);
            }
        }

        for row in 0..self.board.rows {
            for col in 0..self.board.cols {
                let x = ox + col as i32 * PITCH;
                let y = oy + row as i32 * PITCH;
                match self.board.owner[row * MAX_COLS + col] {
                    PLAYER => r.draw_rect(Point::new(x + 2, y + 2), Size::new(5, 5), true),
                    PET => {
                        // A small round dot.
                        r.draw_rect(Point::new(x + 3, y + 4), Size::new(3, 1), true);
                        r.draw_rect(Point::new(x + 4, y + 3), Size::new(1, 3), true);
                    }
                    _ => {}
                }
            }
        }

        if self.state == State::PlayerTurn {
            self.draw_cursor(r, ox + self.cursor_col as i32 * PITCH, oy + self.cursor_row as i32 * PITCH);
        }
    }

    /// A box around the cursor dot. Once picked, the box blinks and dotted
    /// lines show where the dot can connect.
    fn draw_cursor(&self, r: &mut Renderer, x: i32, y: i32) {
        // Clear a 1px gap around the box so it stands out from filled boxes.
        r.fill_rect_off(Point::new(x - 3, y - 3), Size::new(7, 1));
        r.fill_rect_off(Point::new(x - 3, y + 3), Size::new(7, 1));
        r.fill_rect_off(Point::new(x - 3, y - 3), Size::new(1, 7));
        r.fill_rect_off(Point::new(x + 3, y - 3), Size::new(1, 7));
        if !self.dot_selected || self.blink_on() {
            r.draw_rect(Point::new(x - 2, y - 2), Size::new(5, 5), false);
        }
        r.draw_pixel(Point::new(x, y), true);
        if !self.dot_selected {
            return;
        }
        for (dir, dx, dy) in [(Button::Up, 0, -1), (Button::Down, 0, 1), (Button::Left, -1, 0), (Button::Right, 1, 0)] {
            match self.line_from(self.cursor_row, self.cursor_col, dir) {
                Some(e) if !self.board.lines[e] => {
                    for i in (4..PITCH).step_by(2) {
                        r.draw_pixel(Point::new(x + dx * i, y + dy * i), true);
                    }
                }
                _ => {}
            }
        }
    }

    fn draw_score(&self, r: &mut Renderer) {
        let mut buf: String<8> = String::new();
        self.draw_column(r, &mut buf, 77, t!("You"), PLAYER, self.player_score);
        self.draw_column(r, &mut buf, 101, t!("Pet"), PET, self.pet_score);
    }

    /// One side's panel: name, box marker with boxes this game, then rounds won.
    fn draw_column(&self, r: &mut Renderer, buf: &mut String<8>, x: i32, label: &str, who: u8, wins: f32) {
        use core::fmt::Write as _;
        r.draw_text(label, Point::new(x, 1));

        if who == PLAYER {
            r.draw_rect(Point::new(x, 14), Size::new(6, 6), true);
        } else {
            r.draw_rect(Point::new(x, 14), Size::new(6, 6), false);
            r.draw_rect(Point::new(x + 2, 16), Size::new(2, 2), true);
        }
        buf.clear();
        let _ = write!(buf, "{}", self.board.count(who));
        r.draw_text(buf.as_str(), Point::new(x + 8, 12));

        buf.clear();
        format_score(buf, wins);
        r.draw_text(buf.as_str(), Point::new(x, 22));
    }

    fn draw_state_message(&self, r: &mut Renderer) {
        match self.state {
            State::PlayerWin | State::PetWin | State::Draw => {
                self.result_popup.draw(r, false);
            }
            State::PetTurn => {
                r.draw_text_small("...", Point::new(113, 32));
            }
            State::PlayerTurn => {}
        }
    }
}

fn format_score(out: &mut String<8>, v: f32) {
    use core::fmt::Write as _;
    let whole = v as i32;
    if (v - whole as f32).abs() < 0.001 {
        let _ = write!(out, "{}", whole);
    } else {
        let _ = write!(out, "{}.5", whole);
    }
}

impl Scene for PurrimeterScene {
    fn enter(&mut self, ctx: &mut GameContext) {
        self.level = 1;
        self.round_number = 0;
        self.player_score = 0.0;
        self.pet_score = 0.0;
        self.intelligence = ctx.intelligence;
        self.reset_game();
    }

    fn exit(&mut self, ctx: &mut GameContext) {
        let current_ended = matches!(
            self.state,
            State::PlayerWin | State::PetWin | State::Draw
        );
        let total_rounds = self.round_number + if current_ended { 1 } else { 0 };
        if total_rounds == 0 {
            return;
        }
        let scale = (total_rounds as f32 / 4.0).sqrt();
        ctx.apply_stat_changes(&[
            (StatId::Sociability,  3.0 * scale),
            (StatId::Intelligence, 5.0 * scale),
            (StatId::Focus,        4.0 * scale),
            (StatId::Fulfillment,  3.0 * scale),
            (StatId::Loyalty,      1.0 * scale),
        ]);
        let coins = 4 * total_rounds as i32;
        if coins > 0 {
            ctx.coins += coins;
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

        self.character.animate(dt);
        self.blink_timer = (self.blink_timer + dt) % (2.0 * BLINK_HALF);
        self.last_pet_timer += dt;

        match self.state {
            State::PetTurn => {
                self.pet_think_timer += dt;
                if self.pet_think_timer >= PET_THINK_DELAY {
                    let mut rng = ctx.rng;
                    self.make_pet_move(&mut rng);
                    ctx.rng = rng;
                    self.pet_think_timer = 0.0;
                }
            }
            State::PlayerWin | State::PetWin | State::Draw => {
                self.end_delay_timer += dt;
                if buttons.was_just_pressed(Button::A) && self.end_delay_timer >= END_DELAY {
                    if self.state == State::PlayerWin {
                        self.level += 1;
                    }
                    self.round_number += 1;
                    self.reset_game();
                }
            }
            State::PlayerTurn => self.update_player_turn(buttons),
        }

        None
    }

    fn draw(&self, _ctx: &GameContext, renderer: &mut Renderer, _dt_ms: u64) {
        self.draw_board(renderer);
        self.draw_score(renderer);
        self.draw_state_message(renderer);
        self.character.draw(renderer, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_boxes_scores_them() {
        let mut b = Board::new(1, 2);
        for e in [h_edge(0, 0), h_edge(1, 0), v_edge(0, 0), h_edge(0, 1), h_edge(1, 1), v_edge(0, 2)] {
            assert_eq!(b.draw_line(e, PET), 0);
        }
        // The shared middle line closes both boxes at once.
        assert_eq!(b.draw_line(v_edge(0, 1), PLAYER), 2);
        assert_eq!(b.count(PLAYER), 2);
        assert!(b.is_full());
    }

    #[test]
    fn pet_takes_free_boxes_and_otherwise_plays_safe() {
        let mut rng = 0x1234_5678;
        for _ in 0..50 {
            // Top-left box has three sides: the pet should close it.
            let mut b = Board::new(3, 3);
            b.draw_line(h_edge(0, 0), PLAYER);
            b.draw_line(v_edge(0, 0), PLAYER);
            b.draw_line(h_edge(1, 0), PLAYER);
            assert_eq!(pick_pet_line(&b, 0, 0, &mut rng), Some(v_edge(0, 1)));

            // A box with two sides: safe lines exist, so it isn't given away.
            let mut b = Board::new(3, 3);
            b.draw_line(h_edge(0, 0), PLAYER);
            b.draw_line(v_edge(0, 0), PLAYER);
            let e = pick_pet_line(&b, 0, 0, &mut rng).unwrap();
            assert!(!b.gives_box(e));
        }
    }

    #[test]
    fn pet_double_deals_the_last_two_boxes() {
        // Left column: a 2-box chain open at the bottom edge, top box ready to
        // take. Right: a closed 2x2 loop where any line gives away all 4.
        let mut b = Board::new(2, 3);
        for e in [
            h_edge(0, 0), v_edge(0, 0), v_edge(0, 1), v_edge(1, 0), v_edge(1, 1),
            h_edge(0, 1), h_edge(0, 2), v_edge(0, 3), v_edge(1, 3), h_edge(2, 1), h_edge(2, 2),
        ] {
            b.draw_line(e, PLAYER);
        }
        let mut rng = 0x1234_5678;
        // Taking both boxes would mean opening the loop, so hand them over instead.
        assert_eq!(pick_pet_line(&b, 0, 100, &mut rng), Some(h_edge(2, 0)));
        assert_eq!(pick_pet_line(&b, 0, 0, &mut rng), Some(h_edge(1, 0)));
    }
}
