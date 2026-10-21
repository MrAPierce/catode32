//! Reversi minigame. Play against the pet on an 8x8 board. Pets with high
//! intelligence look further ahead as the rounds progress.

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

const N: usize = 8;
const CELLS: usize = N * N;

// Board geometry: 6px cells separated by 1px grid lines, with a border.
const BOARD_OFFSET_X: i32 = 2;
const BOARD_OFFSET_Y: i32 = 3;
const PITCH: i32 = 7;
const CELL: i32 = 6;

// Cell values.
const EMPTY: u8 = 0;
const PLAYER: u8 = 1;
const PET: u8 = 2;

const PET_THINK_DELAY: f32 = 0.6;
const PASS_DELAY: f32 = 1.0;
const END_DELAY: f32 = 0.5;

/// Pets above this intelligence play the stronger AI.
const SMART_INTELLIGENCE: f32 = 75.0;

const DIRS: [(i32, i32); 8] = [
    (-1, -1), (-1, 0), (-1, 1),
    (0, -1),           (0, 1),
    (1, -1),  (1, 0),  (1, 1),
];

// Positional weights: corners are gold, the squares next to them are traps.
const WEIGHTS: [i32; CELLS] = [
    100, -20,  10,   5,   5,  10, -20, 100,
    -20, -50,  -2,  -2,  -2,  -2, -50, -20,
     10,  -2,   1,   1,   1,   1,  -2,  10,
      5,  -2,   1,   0,   0,   1,  -2,   5,
      5,  -2,   1,   0,   0,   1,  -2,   5,
     10,  -2,   1,   1,   1,   1,  -2,  10,
    -20, -50,  -2,  -2,  -2,  -2, -50, -20,
    100, -20,  10,   5,   5,  10, -20, 100,
];

// 4x4 disc masks centered in a cell, one row per byte, high bit = leftmost pixel.
const DISC_FILLED: [u8; 4] = [0b01100000, 0b11110000, 0b11110000, 0b01100000];
const DISC_OUTLINE: [u8; 4] = [0b01100000, 0b10010000, 0b10010000, 0b01100000];

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    PlayerTurn,
    PetTurn,
    /// `who` has no legal move; the other side moves again after a beat.
    Pass { who: u8 },
    PlayerWin,
    PetWin,
    Draw,
}

/// How the pet picks its moves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PetLevel {
    /// Greedy on positional weights, with occasional random moves.
    Casual { random_pct: u32 },
    /// Greedy on positional weights plus mobility.
    Mobility,
    /// Looks at the player's best reply to each move.
    Lookahead,
}

type Board = [u8; CELLS];

fn opponent(who: u8) -> u8 {
    if who == PLAYER { PET } else { PLAYER }
}

/// Number of discs `who` would flip in one direction by playing at `idx`.
fn flips_in_dir(board: &Board, idx: usize, who: u8, dr: i32, dc: i32) -> usize {
    let opp = opponent(who);
    let mut r = (idx / N) as i32 + dr;
    let mut c = (idx % N) as i32 + dc;
    let mut count = 0;
    while r >= 0 && r < N as i32 && c >= 0 && c < N as i32 {
        let cell = board[r as usize * N + c as usize];
        if cell == opp {
            count += 1;
        } else if cell == who {
            return count;
        } else {
            return 0;
        }
        r += dr;
        c += dc;
    }
    0
}

fn flip_count(board: &Board, idx: usize, who: u8) -> usize {
    if board[idx] != EMPTY {
        return 0;
    }
    DIRS.iter()
        .map(|&(dr, dc)| flips_in_dir(board, idx, who, dr, dc))
        .sum()
}

fn is_legal(board: &Board, idx: usize, who: u8) -> bool {
    flip_count(board, idx, who) > 0
}

/// Place a disc for `who` at `idx` and flip the captured discs.
fn apply_move(board: &mut Board, idx: usize, who: u8) {
    for &(dr, dc) in DIRS.iter() {
        let n = flips_in_dir(board, idx, who, dr, dc);
        let mut r = (idx / N) as i32;
        let mut c = (idx % N) as i32;
        for _ in 0..n {
            r += dr;
            c += dc;
            board[r as usize * N + c as usize] = who;
        }
    }
    board[idx] = who;
}

fn legal_moves(board: &Board, who: u8, out: &mut [u8; CELLS]) -> usize {
    let mut len = 0;
    for i in 0..CELLS {
        if is_legal(board, i, who) {
            out[len] = i as u8;
            len += 1;
        }
    }
    len
}

fn mobility(board: &Board, who: u8) -> i32 {
    (0..CELLS).filter(|&i| is_legal(board, i, who)).count() as i32
}

fn count(board: &Board, who: u8) -> u32 {
    board.iter().filter(|&&c| c == who).count() as u32
}

fn positional(board: &Board, who: u8) -> i32 {
    let opp = opponent(who);
    let mut score = 0;
    for i in 0..CELLS {
        if board[i] == who {
            score += WEIGHTS[i];
        } else if board[i] == opp {
            score -= WEIGHTS[i];
        }
    }
    score
}

/// Board evaluation from the pet's point of view.
fn evaluate(board: &Board) -> i32 {
    positional(board, PET) + 5 * (mobility(board, PET) - mobility(board, PLAYER))
}

pub struct ReversiScene {
    character: Character,
    result_popup: Popup,

    round_number: u32,
    player_score: f32,
    pet_score: f32,
    intelligence: f32,

    board: Board,
    cursor_pos: usize,
    pet_level: PetLevel,
    pet_think_timer: f32,
    pass_timer: f32,
    end_delay_timer: f32,
    state: State,
}

impl ReversiScene {
    pub fn new() -> Self {
        let mut scene = Self {
            character: Character::new(Point::new(100, 63)),
            result_popup: Popup::new(14, 0, 100, 40),

            round_number: 0,
            player_score: 0.0,
            pet_score: 0.0,
            intelligence: 0.0,

            board: [EMPTY; CELLS],
            cursor_pos: 0,
            pet_level: PetLevel::Casual { random_pct: 10 },
            pet_think_timer: 0.0,
            pass_timer: 0.0,
            end_delay_timer: 0.0,
            state: State::PlayerTurn,
        };
        scene.reset_game();
        scene
    }

    fn reset_game(&mut self) {
        self.board = [EMPTY; CELLS];
        self.board[3 * N + 3] = PET;
        self.board[4 * N + 4] = PET;
        self.board[3 * N + 4] = PLAYER;
        self.board[4 * N + 3] = PLAYER;

        self.pet_level = if self.intelligence <= SMART_INTELLIGENCE {
            PetLevel::Casual { random_pct: 10 }
        } else {
            match self.round_number {
                0 => PetLevel::Casual { random_pct: 5 },
                1 | 2 => PetLevel::Mobility,
                _ => PetLevel::Lookahead,
            }
        };

        self.pet_think_timer = 0.0;
        self.pass_timer = 0.0;
        self.end_delay_timer = 0.0;

        self.character.set_pose(PoseId::LayingSideNeutral);

        if self.round_number % 2 == 0 {
            self.start_player_turn();
        } else {
            self.state = State::PetTurn;
        }
    }

    fn start_player_turn(&mut self) {
        self.state = State::PlayerTurn;
        if !is_legal(&self.board, self.cursor_pos, PLAYER) {
            let mut moves = [0u8; CELLS];
            if legal_moves(&self.board, PLAYER, &mut moves) > 0 {
                self.cursor_pos = moves[0] as usize;
            }
        }
    }

    fn start_pet_turn(&mut self) {
        self.state = State::PetTurn;
        self.pet_think_timer = 0.0;
    }

    /// Hand the turn over after `mover` has played.
    fn advance_turn(&mut self, mover: u8) {
        let next = opponent(mover);
        if mobility(&self.board, next) > 0 {
            self.begin_turn(next);
        } else if mobility(&self.board, mover) > 0 {
            self.state = State::Pass { who: next };
            self.pass_timer = 0.0;
        } else {
            self.finish_game();
        }
    }

    fn begin_turn(&mut self, who: u8) {
        if who == PLAYER {
            self.start_player_turn();
        } else {
            self.start_pet_turn();
        }
    }

    fn finish_game(&mut self) {
        let player = count(&self.board, PLAYER);
        let pet = count(&self.board, PET);
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

    fn player_place_disc(&mut self) {
        if !is_legal(&self.board, self.cursor_pos, PLAYER) {
            return;
        }
        apply_move(&mut self.board, self.cursor_pos, PLAYER);
        self.advance_turn(PLAYER);
    }

    fn make_pet_move(&mut self, rng: &mut u32) {
        if let Some(m) = self.find_best_move(rng) {
            apply_move(&mut self.board, m, PET);
        }
        self.advance_turn(PET);
    }

    fn score_move(&self, idx: usize) -> i32 {
        let mut after = self.board;
        apply_move(&mut after, idx, PET);
        match self.pet_level {
            PetLevel::Casual { .. } => {
                // Flips only break ties between equally placed squares.
                WEIGHTS[idx] * 4 + flip_count(&self.board, idx, PET) as i32
            }
            PetLevel::Mobility => evaluate(&after),
            PetLevel::Lookahead => {
                let mut replies = [0u8; CELLS];
                let len = legal_moves(&after, PLAYER, &mut replies);
                if len == 0 {
                    // Making the player pass is worth a bonus.
                    return evaluate(&after) + 20;
                }
                let mut worst = i32::MAX;
                for &r in &replies[..len] {
                    let mut reply = after;
                    apply_move(&mut reply, r as usize, PLAYER);
                    worst = worst.min(evaluate(&reply));
                }
                worst
            }
        }
    }

    fn find_best_move(&self, rng: &mut u32) -> Option<usize> {
        let mut moves = [0u8; CELLS];
        let len = legal_moves(&self.board, PET, &mut moves);
        if len == 0 {
            return None;
        }

        // Occasional random move keeps casual games winnable.
        if let PetLevel::Casual { random_pct } = self.pet_level {
            if len > 1 && rand_range_u32(rng, 1, 100) <= random_pct {
                let pick = rand_range_u32(rng, 0, (len - 1) as u32) as usize;
                return Some(moves[pick] as usize);
            }
        }

        let mut best_score = i32::MIN;
        let mut best_buf = [0u8; CELLS];
        let mut best_len = 0usize;
        for &m in &moves[..len] {
            let s = self.score_move(m as usize);
            if s > best_score {
                best_score = s;
                best_buf[0] = m;
                best_len = 1;
            } else if s == best_score {
                best_buf[best_len] = m;
                best_len += 1;
            }
        }
        let pick = rand_range_u32(rng, 0, (best_len - 1) as u32) as usize;
        Some(best_buf[pick] as usize)
    }

    fn cell_to_pixel(cell_idx: usize) -> (i32, i32) {
        let row = (cell_idx / N) as i32;
        let col = (cell_idx % N) as i32;
        (
            BOARD_OFFSET_X + 1 + col * PITCH,
            BOARD_OFFSET_Y + 1 + row * PITCH,
        )
    }

    fn draw_board(&self, r: &mut Renderer) {
        let span = N as i32 * PITCH;
        for i in 0..=N as i32 {
            let x = BOARD_OFFSET_X + i * PITCH;
            r.draw_line(
                Point::new(x, BOARD_OFFSET_Y),
                Point::new(x, BOARD_OFFSET_Y + span),
            );
            let y = BOARD_OFFSET_Y + i * PITCH;
            r.draw_line(
                Point::new(BOARD_OFFSET_X, y),
                Point::new(BOARD_OFFSET_X + span, y),
            );
        }
    }

    fn draw_cells(&self, r: &mut Renderer) {
        let show_hints = self.state == State::PlayerTurn;
        for i in 0..CELLS {
            let (x, y) = Self::cell_to_pixel(i);
            let selected = show_hints && i == self.cursor_pos;
            // The cursor cell is drawn inverted.
            if selected {
                r.draw_rect(Point::new(x, y), Size::new(CELL as u32, CELL as u32), true);
            }
            let on = !selected;
            match self.board[i] {
                PLAYER => draw_disc(r, &DISC_OUTLINE, x + 1, y + 1, on),
                PET => draw_disc(r, &DISC_FILLED, x + 1, y + 1, on),
                _ if show_hints && is_legal(&self.board, i, PLAYER) => {
                    let (pos, size) = (Point::new(x + 2, y + 2), Size::new(2, 2));
                    if on {
                        r.draw_rect(pos, size, true);
                    } else {
                        r.fill_rect_off(pos, size);
                    }
                }
                _ => {}
            }
        }
    }

    fn draw_score(&self, r: &mut Renderer) {
        let mut buf: String<8> = String::new();
        self.draw_column(r, &mut buf, 62, t!("You"), PLAYER, &DISC_OUTLINE, self.player_score);
        self.draw_column(r, &mut buf, 92, t!("Pet"), PET, &DISC_FILLED, self.pet_score);
    }

    /// One side's panel: name, disc colour with current disc count, then rounds won.
    fn draw_column(
        &self,
        r: &mut Renderer,
        buf: &mut String<8>,
        x: i32,
        label: &str,
        who: u8,
        disc: &[u8; 4],
        wins: f32,
    ) {
        use core::fmt::Write as _;
        r.draw_text(label, Point::new(x, 1));

        draw_disc(r, disc, x + 1, 15, true);
        buf.clear();
        let _ = write!(buf, "{}", count(&self.board, who));
        r.draw_text(buf.as_str(), Point::new(x + 8, 12));

        // A pass briefly replaces the passing side's win count.
        if self.state == (State::Pass { who }) {
            r.draw_text_small(t!("Pass!"), Point::new(x, 24));
        } else {
            buf.clear();
            format_score(buf, wins);
            r.draw_text(buf.as_str(), Point::new(x, 22));
        }
    }

    fn draw_state_message(&self, r: &mut Renderer) {
        match self.state {
            State::PlayerWin | State::PetWin | State::Draw => {
                self.result_popup.draw(r, false);
            }
            State::PetTurn => {
                r.draw_text("...", Point::new(110, 1));
            }
            _ => {}
        }
    }
}

fn draw_disc(r: &mut Renderer, mask: &[u8; 4], x: i32, y: i32, on: bool) {
    for (dy, row) in mask.iter().enumerate() {
        for dx in 0..4 {
            if row & (0x80 >> dx) != 0 {
                r.draw_pixel(Point::new(x + dx, y + dy as i32), on);
            }
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

impl Scene for ReversiScene {
    fn enter(&mut self, ctx: &mut GameContext) {
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
        // Reversi games run longer than TicTacToe, so rounds count for more.
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
            State::Pass { who } => {
                self.pass_timer += dt;
                if self.pass_timer >= PASS_DELAY {
                    self.begin_turn(opponent(who));
                }
            }
            State::PlayerWin | State::PetWin | State::Draw => {
                self.end_delay_timer += dt;
                if buttons.was_just_pressed(Button::A) && self.end_delay_timer >= END_DELAY {
                    self.round_number += 1;
                    self.reset_game();
                }
            }
            State::PlayerTurn => {
                let row = self.cursor_pos / N;
                let col = self.cursor_pos % N;

                if buttons.was_just_pressed(Button::Up) && row > 0 {
                    self.cursor_pos -= N;
                } else if buttons.was_just_pressed(Button::Down) && row < N - 1 {
                    self.cursor_pos += N;
                } else if buttons.was_just_pressed(Button::Left) && col > 0 {
                    self.cursor_pos -= 1;
                } else if buttons.was_just_pressed(Button::Right) && col < N - 1 {
                    self.cursor_pos += 1;
                }

                if buttons.was_just_pressed(Button::A) {
                    self.player_place_disc();
                }
            }
        }

        None
    }

    fn draw(&self, _ctx: &GameContext, renderer: &mut Renderer, _dt_ms: u64) {
        self.draw_board(renderer);
        self.draw_cells(renderer);
        self.draw_score(renderer);
        self.draw_state_message(renderer);
        self.character.draw(renderer, 0);
    }
}
