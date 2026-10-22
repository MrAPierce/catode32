//! Sleeping Dogs minigame, a cat's take on minesweeper. Tiptoe across the
//! yard without waking any of the dogs napping in it. Open tiles count the
//! dogs asleep next to them. Dogs aren't placed until the first step, which
//! always lands on a tile with no dog on or around it.
//!
//! The 18x9 board at 7px pitch fills the 128x64 screen.

use core::fmt::Write as _;

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
    rand::xorshift32,
    render::Renderer,
    scene::{Scene, SceneId},
    ui::menu::{Menu, MenuItem, MenuResult},
    ui::popup::Popup,
};

const COLS: usize = 18;
const ROWS: usize = 9;
const CELLS: usize = COLS * ROWS;

const PITCH: i32 = 7;
const CELL_IN: u32 = (PITCH - 1) as u32;
const BOARD_X: i32 = 1;

// Cell states.
const HIDDEN: u8 = 0;
const OPEN: u8 = 1;
const MARKED: u8 = 2;

const WIN_RESET_DELAY: f32 = 3.0;
const SPOOKED_TIME: f32 = 2.5;
/// Seconds per on/off cycle of the revealed dogs.
const PULSE_PERIOD: f32 = 0.5;
/// Left edge of the "Spooked!" text, just past the cat.
const TEXT_X: i32 = 60;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Playing,
    Win,
    /// A dog woke up: the cat jumps out of its skin.
    Spooked,
    /// Back on the board with the dogs revealed.
    Lose,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Difficulty {
    Easy,
    Normal,
    Hard,
}

impl Difficulty {
    fn dogs(self) -> usize {
        match self {
            Difficulty::Easy => 20,
            Difficulty::Normal => 26,
            Difficulty::Hard => 33,
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
enum SleepingDogsAction {
    NewBoard,
    Easy,
    Normal,
    Hard,
}

const DIFFICULTY_ITEMS: &[MenuItem<SleepingDogsAction>] = &[
    MenuItem { label: t!("Easy"),   icon: None, submenu: None, action: Some(SleepingDogsAction::Easy),   confirm: None, confirm_on_vacation: None },
    MenuItem { label: t!("Normal"), icon: None, submenu: None, action: Some(SleepingDogsAction::Normal), confirm: None, confirm_on_vacation: None },
    MenuItem { label: t!("Hard"),   icon: None, submenu: None, action: Some(SleepingDogsAction::Hard),   confirm: None, confirm_on_vacation: None },
];

const OPTIONS_ITEMS: &[MenuItem<SleepingDogsAction>] = &[
    MenuItem { label: t!("New Board"),  icon: None, submenu: None,                   action: Some(SleepingDogsAction::NewBoard), confirm: None, confirm_on_vacation: None },
    MenuItem { label: t!("Difficulty"), icon: None, submenu: Some(DIFFICULTY_ITEMS), action: None,                               confirm: None, confirm_on_vacation: None },
];

/// What happened when the cat stepped somewhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Nothing,
    Safe,
    Woke(usize),
}

/// The yard. Dogs are placed on the first step so it can't wake one.
struct Board {
    dog: [bool; CELLS],
    adj: [u8; CELLS],
    cell: [u8; CELLS],
    dogs: usize,
    placed: bool,
    opened: usize,
}

impl Board {
    const fn new() -> Self {
        Self {
            dog: [false; CELLS],
            adj: [0; CELLS],
            cell: [HIDDEN; CELLS],
            dogs: 0,
            placed: false,
            opened: 0,
        }
    }

    fn reset(&mut self, dogs: usize) {
        *self = Self::new();
        self.dogs = dogs;
    }

    /// Scatter the dogs over every cell except `safe` and its neighbours.
    fn place_dogs(&mut self, safe: usize, rng: &mut u32) {
        let mut pool = [0u8; CELLS];
        let mut len = 0;
        for i in 0..CELLS {
            if !is_near(i, safe) {
                pool[len] = i as u8;
                len += 1;
            }
        }
        // Partial Fisher-Yates: the first `dogs` slots become the dogs.
        let count = self.dogs.min(len);
        for k in 0..count {
            let j = k + (xorshift32(rng) as usize) % (len - k);
            pool.swap(k, j);
            self.dog[pool[k] as usize] = true;
        }
        self.dogs = count;
        for i in 0..CELLS {
            self.adj[i] = neighbours(i).filter(|&j| self.dog[j]).count() as u8;
        }
        self.placed = true;
    }

    /// Step on a hidden cell, opening the area around it if no dogs are near.
    fn step(&mut self, idx: usize, rng: &mut u32) -> Step {
        if self.cell[idx] != HIDDEN {
            return Step::Nothing;
        }
        if !self.placed {
            self.place_dogs(idx, rng);
        }
        if self.dog[idx] {
            self.cell[idx] = OPEN;
            return Step::Woke(idx);
        }
        let mut stack = [0u8; CELLS];
        let mut top = 0;
        self.cell[idx] = OPEN;
        self.opened += 1;
        stack[top] = idx as u8;
        top += 1;
        while top > 0 {
            top -= 1;
            let i = stack[top] as usize;
            if self.adj[i] != 0 {
                continue;
            }
            for j in neighbours(i) {
                if self.cell[j] == HIDDEN {
                    self.cell[j] = OPEN;
                    self.opened += 1;
                    stack[top] = j as u8;
                    top += 1;
                }
            }
        }
        Step::Safe
    }

    /// Step on every unmarked neighbour of an open number whose dogs are all
    /// marked. A wrong mark wakes a dog.
    fn step_around(&mut self, idx: usize, rng: &mut u32) -> Step {
        if self.cell[idx] != OPEN || self.adj[idx] == 0 {
            return Step::Nothing;
        }
        let marks = neighbours(idx).filter(|&j| self.cell[j] == MARKED).count();
        if marks != self.adj[idx] as usize {
            return Step::Nothing;
        }
        let mut result = Step::Nothing;
        for j in neighbours(idx) {
            match self.step(j, rng) {
                Step::Woke(w) => return Step::Woke(w),
                Step::Safe => result = Step::Safe,
                Step::Nothing => {}
            }
        }
        result
    }

    fn toggle_mark(&mut self, idx: usize) {
        self.cell[idx] = match self.cell[idx] {
            HIDDEN => MARKED,
            MARKED => HIDDEN,
            s => s,
        };
    }

    fn cleared(&self) -> bool {
        self.placed && self.opened == CELLS - self.dogs
    }

    fn mark_all_dogs(&mut self) {
        for i in 0..CELLS {
            if self.dog[i] {
                self.cell[i] = MARKED;
            }
        }
    }
}

/// `a` is `b` or one of its neighbours.
fn is_near(a: usize, b: usize) -> bool {
    let (ar, ac) = ((a / COLS) as i32, (a % COLS) as i32);
    let (br, bc) = ((b / COLS) as i32, (b % COLS) as i32);
    (ar - br).abs() <= 1 && (ac - bc).abs() <= 1
}

fn neighbours(i: usize) -> impl Iterator<Item = usize> {
    let row = (i / COLS) as i32;
    let col = (i % COLS) as i32;
    (-1..=1i32).flat_map(move |dr| {
        (-1..=1i32).filter_map(move |dc| {
            let (r, c) = (row + dr, col + dc);
            if (dr, dc) == (0, 0) || r < 0 || c < 0 || r >= ROWS as i32 || c >= COLS as i32 {
                None
            } else {
                Some((r * COLS as i32 + c) as usize)
            }
        })
    })
}

pub struct SleepingDogsScene {
    character: Character,
    win_popup: Popup,

    board: Board,
    difficulty: Difficulty,
    cursor: usize,
    state: State,
    timer: f32,

    session_wins: u32,
    session_coins: i32,

    options_menu: Menu<SleepingDogsAction>,
    menu_active: bool,
}

impl SleepingDogsScene {
    pub fn new() -> Self {
        Self {
            character: Character::new(Point::new(44, 63)),
            win_popup: Popup::new(10, 14, 108, 36),

            board: Board::new(),
            difficulty: Difficulty::Easy,
            cursor: CELLS / 2,
            state: State::Playing,
            timer: 0.0,

            session_wins: 0,
            session_coins: 0,

            options_menu: Menu::new(OPTIONS_ITEMS),
            menu_active: false,
        }
    }

    fn new_board(&mut self) {
        self.board.reset(self.difficulty.dogs());
        self.cursor = CELLS / 2;
        self.state = State::Playing;
        self.timer = 0.0;
    }

    fn after_step(&mut self, step: Step) {
        match step {
            Step::Woke(_) => {
                self.state = State::Spooked;
                self.timer = 0.0;
                self.character.set_pose(PoseId::SittingForwardShocked);
            }
            Step::Safe if self.board.cleared() => {
                self.board.mark_all_dogs();
                self.session_wins += 1;
                self.session_coins += self.difficulty.coins();
                self.state = State::Win;
                self.timer = 0.0;
                self.win_popup.set_text(t!("Tiptoed through!"), false, true);
            }
            _ => {}
        }
    }

    fn handle_menu(&mut self, buttons: &mut Buttons) {
        match self.options_menu.handle_input(buttons, false) {
            MenuResult::Continue => {}
            MenuResult::Closed => self.menu_active = false,
            MenuResult::Action(action) => {
                self.menu_active = false;
                match action {
                    SleepingDogsAction::NewBoard => {}
                    SleepingDogsAction::Easy => self.difficulty = Difficulty::Easy,
                    SleepingDogsAction::Normal => self.difficulty = Difficulty::Normal,
                    SleepingDogsAction::Hard => self.difficulty = Difficulty::Hard,
                }
                self.new_board();
            }
        }
    }

    fn draw_board(&self, r: &mut Renderer) {
        let b = &self.board;
        let show_dogs = self.state == State::Lose;
        let pulse_on = self.timer % PULSE_PERIOD < PULSE_PERIOD / 2.0;
        let mut buf: String<2> = String::new();
        for i in 0..CELLS {
            let x = BOARD_X + (i % COLS) as i32 * PITCH;
            let y = (i / COLS) as i32 * PITCH + 1;
            let pos = Point::new(x, y);
            let size = Size::new(CELL_IN, CELL_IN);
            let inv = i == self.cursor && self.state == State::Playing;

            if show_dogs && b.dog[i] && b.cell[i] != MARKED {
                if pulse_on {
                    r.draw_rect(pos, size, true);
                }
                continue;
            }
            match b.cell[i] {
                OPEN => {
                    buf.clear();
                    if b.adj[i] > 0 {
                        let _ = write!(buf, "{}", b.adj[i]);
                    }
                    draw_cell_text(r, buf.as_str(), pos, size, inv);
                }
                MARKED => {
                    r.draw_rect(pos, size, inv);
                    if show_dogs && !b.dog[i] {
                        // Wrong mark.
                        r.fill_rect_off(Point::new(x + 1, y + 1), Size::new(CELL_IN - 2, CELL_IN - 2));
                        r.draw_line(Point::new(x + 1, y + 1), Point::new(x + 4, y + 4));
                        r.draw_line(Point::new(x + 4, y + 1), Point::new(x + 1, y + 4));
                    } else if inv {
                        r.fill_rect_off(Point::new(x + 2, y + 2), Size::new(2, 2));
                    } else {
                        r.draw_rect(Point::new(x + 2, y + 2), Size::new(2, 2), true);
                    }
                }
                // Hidden tiles are hollow, filled under the cursor.
                _ => r.draw_rect(pos, size, inv),
            }
        }
    }
}

/// Open cell with optional text, filled and inverted under the cursor.
fn draw_cell_text(r: &mut Renderer, text: &str, pos: Point, size: Size, inv: bool) {
    let at = Point::new(pos.x + 1, pos.y);
    if inv {
        r.draw_rect(pos, size, true);
        r.draw_text_small_inverted(text, at);
    } else {
        r.draw_text_small(text, at);
    }
}

impl Scene for SleepingDogsScene {
    fn enter(&mut self, _ctx: &mut GameContext) {
        self.session_wins = 0;
        self.session_coins = 0;
        self.menu_active = false;
        self.new_board();
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
            self.handle_menu(buttons);
            return None;
        }

        match self.state {
            State::Win => {
                self.timer += dt;
                if buttons.was_just_pressed(Button::A) || self.timer >= WIN_RESET_DELAY {
                    self.new_board();
                }
                return None;
            }
            State::Spooked => {
                self.timer += dt;
                self.character.animate(dt);
                if buttons.was_just_pressed(Button::A) || self.timer >= SPOOKED_TIME {
                    self.state = State::Lose;
                    self.timer = 0.0;
                }
                return None;
            }
            State::Lose => {
                self.timer += dt;
                if buttons.was_just_pressed(Button::A) {
                    self.new_board();
                } else if buttons.was_just_pressed(Button::Menu2) {
                    self.menu_active = true;
                    self.options_menu.reset_to(OPTIONS_ITEMS);
                }
                return None;
            }
            State::Playing => {}
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

        if buttons.was_just_pressed(Button::A) {
            let mut rng = ctx.rng;
            let step = if self.board.cell[self.cursor] == OPEN {
                self.board.step_around(self.cursor, &mut rng)
            } else {
                self.board.step(self.cursor, &mut rng)
            };
            ctx.rng = rng;
            self.after_step(step);
        } else if buttons.was_just_pressed(Button::B) {
            self.board.toggle_mark(self.cursor);
        }

        None
    }

    fn draw(&self, _ctx: &GameContext, renderer: &mut Renderer, _dt_ms: u64) {
        if self.menu_active {
            self.options_menu.draw(renderer);
            return;
        }
        if self.state == State::Spooked {
            self.character.draw(renderer, 0);
            // Centred in the space right of the cat.
            let text = t!("Spooked!");
            let w = text.chars().count() as i32 * 6;
            renderer.draw_text(text, Point::new(TEXT_X + (128 - TEXT_X - w) / 2, 28));
            return;
        }

        self.draw_board(renderer);

        if self.state == State::Win {
            self.win_popup.draw(renderer, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_step_never_wakes_a_dog() {
        for seed in 1..500u32 {
            let mut rng = seed;
            let mut b = Board::new();
            b.reset(33);
            let first = (seed as usize * 7) % CELLS;
            assert_eq!(b.step(first, &mut rng), Step::Safe);
            assert_eq!(b.dog.iter().filter(|&&d| d).count(), 33);
            for i in 0..CELLS {
                if is_near(i, first) {
                    assert!(!b.dog[i], "seed {} cell {}", seed, i);
                }
            }
            // No dogs nearby, so the first step always opens an area.
            assert_eq!(b.adj[first], 0);
            assert!(b.opened >= 4);
        }
    }

    #[test]
    fn opening_every_safe_tile_clears_the_yard() {
        let mut rng = 12345;
        let mut b = Board::new();
        b.reset(20);
        b.step(40, &mut rng);
        // The cascade stops at numbers and never opens a dog.
        for i in 0..CELLS {
            if b.cell[i] == OPEN {
                assert!(!b.dog[i]);
                if b.adj[i] == 0 {
                    assert!(neighbours(i).all(|j| b.cell[j] == OPEN));
                }
            }
        }
        assert!(!b.cleared());
        for i in 0..CELLS {
            if !b.dog[i] {
                assert_ne!(b.step(i, &mut rng), Step::Woke(i));
            }
        }
        assert!(b.cleared());
        let dog = b.dog.iter().position(|&d| d).unwrap();
        assert_eq!(b.step(dog, &mut rng), Step::Woke(dog));
    }
}
