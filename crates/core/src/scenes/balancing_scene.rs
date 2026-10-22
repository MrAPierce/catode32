//! Balancing Act, a one-button stacker. The cat rides a box that slides back
//! and forth; press A to drop it onto the tower. Whatever hangs over the box
//! below is trimmed off and falls away, so the boxes keep getting narrower.
//! Miss the tower completely and the cat takes a tumble.
//!
//! Drops within a pixel of perfect snap into place, and every few perfect
//! drops in a row grow the box back a little, with a burst of sparkles.
//!
//! The sky changes with height: clouds drift past first, then stars come
//! out, and a tall enough tower reaches the moon.

use core::fmt::Write as _;

use embedded_graphics::prelude::{Point, Size};
use heapless::{String, Vec};
#[cfg(not(feature = "desktop"))]
use micromath::F32Ext;
use crate::t;

use crate::{
    assets::{
        effects::{POOF, POOF_FRAME_DUR},
        minigame_character::{PLATFORMER_CAT_JUMP, PLATFORMER_CAT_SIT, SMALL_BIRD1},
        nature::{CLOUD1, CLOUD2, CLOUD3, MOON},
        plants::{FREESIA_GROWING, FREESIA_YOUNG, GRASS_GROWING, GRASS_MATURE, GRASS_YOUNG, ROSE_YOUNG, SUNFLOWER_YOUNG},
    },
    board::{DISPLAY_HEIGHT, DISPLAY_WIDTH},
    context::{GameContext, StatId},
    input::{Button, Buttons},
    rand::{rand_range_f32, xorshift32},
    render::{Renderer, Sprite, SpriteOpts},
    scene::{Scene, SceneId},
    sky::ShootingStarEvent,
    ui::burst::{BurstEffect, DEFAULT_STYLE},
    ui::popup::Popup,
};

const DISPLAY_W: i32 = DISPLAY_WIDTH as i32;
const DISPLAY_H: i32 = DISPLAY_HEIGHT as i32;

// Tower layout. Row 0 is the base box, sitting on a ground line along the
// bottom of the screen.
const BOX_H: i32 = 4;
const BASE_W: u8 = 48;
const BASE_Y: i32 = DISPLAY_H - 1 - BOX_H;
/// Placed rows remembered for drawing; more than fit on screen.
const RING: usize = 20;

/// Once the tower is tall enough, the camera keeps the sliding box this far
/// down the screen, leaving room for the cat and the score above it.
const ACTIVE_MIN_Y: i32 = 28;
/// How quickly the camera catches up, per second.
const CAM_EASE: f32 = 8.0;

/// Slide speed in px/s from each height up. The first step comes early so
/// it's clear the game speeds up, then the gaps widen to give time to settle
/// into each speed. The game runs at 12 FPS, so 12 px/s is a pixel a frame;
/// the top speed keeps a perfect drop possible.
const SPEED_STEPS: [(u32, f32); 6] = [
    (0, 24.0),
    (5, 30.0),
    (15, 36.0),
    (30, 42.0),
    (50, 48.0),
    (75, 54.0),
];

/// Drops this close to the box below snap exactly onto it.
const PERFECT_PX: i32 = 1;
const PERFECT_FLASH: f32 = 0.6;
/// Every this many perfect drops in a row, the box grows by `GROW_PX`.
const GROW_STREAK: u32 = 3;
const GROW_PX: u8 = 4;
const GROW_SPARKLES: usize = 6;
const GROW_SPARKLE_STAGGER: f32 = 0.08;

/// Fall acceleration for trimmed pieces and a missed box, px/s^2.
const GRAVITY: f32 = 160.0;
const MAX_CHUNKS: usize = 4;

/// Pause after the poof before the result popup.
const RESULT_PAUSE: f32 = 0.3;

// Cat, borrowed from Hunter: wags its tail while riding, and uses the
// falling jump frame when the box drops out from under it.
const CAT_WAG_FPS: f32 = 4.0;
const CAT_FALL_FRAME: usize = 2;

// Sky. Scenery scrolls at a fraction of the tower's speed so it reads as far
// away. Heights are in boxes stacked.
const SKY_PARALLAX: f32 = 0.75;
/// Clouds drift down into view from above, one entering every few boxes
/// between these heights.
const MAX_CLOUDS: usize = 5;
/// Grass and flowers alternate along the ground either side of the base.
const GRASS_SPRITES: [&Sprite; 3] = [&GRASS_YOUNG, &GRASS_GROWING, &GRASS_MATURE];
const FLOWER_SPRITES: [&Sprite; 4] = [&FREESIA_YOUNG, &FREESIA_GROWING, &ROSE_YOUNG, &SUNFLOWER_YOUNG];
/// Tallest plant, for skipping them once the ground scrolls away.
const PLANT_MAX_H: i32 = 17;
const MAX_PLANTS: usize = 10;
const PLANT_GAP_MIN: u32 = 3;
const PLANT_GAP_MAX: u32 = 14;
/// Space kept clear either side of the base box.
const PLANT_BASE_MARGIN: i32 = 2;
/// Now and then a bird from Zoomies flies right to left across the sky,
/// between these heights. Only one at a time, and a run may see none: each
/// time the wait runs out, a bird comes only with `BIRD_CHANCE`.
const MAX_BIRDS: usize = 1;
const BIRDS_FROM: u32 = 10;
const BIRD_WAIT_MIN: f32 = 10.0;
const BIRD_WAIT_MAX: f32 = 25.0;
const BIRD_CHANCE: f32 = 0.5;
const BIRD_SPEED_MIN: f32 = 32.0;
const BIRD_SPEED_MAX: f32 = 48.0;
/// Screen band birds spawn in, below the score.
const BIRD_Y_MIN: f32 = 10.0;
const BIRD_Y_MAX: f32 = 34.0;
const BIRD_ANIM_FPS: f32 = 8.0;
/// Way up among the stars, the odd shooting star. Like birds, a wait runs
/// down and then a star comes only by chance, a little more often higher up.
/// Odds are (wait min s, wait max s, chance) for each height band.
const SHOOTING_STARS_FROM: u32 = 100;
const SHOOTING_STARS_MORE_FROM: u32 = 200;
const SHOOTING_STAR_ODDS: (f32, f32, f32) = (15.0, 40.0, 0.35);
const SHOOTING_STAR_MORE_ODDS: (f32, f32, f32) = (8.0, 25.0, 0.6);
const CLOUD_FIRST: u32 = 8;
const CLOUD_LAST: u32 = 40;
const CLOUD_DRIFT_MAX: f32 = 5.0;
const MAX_STARS: usize = 48;
/// Stars fade in between these heights, and scroll slower than clouds.
const STARS_FROM: u32 = 35;
const STARS_FULL: u32 = 65;
const STAR_PARALLAX: f32 = 0.15;
// Twinkle cycle, as in Lucky Landing and the outdoor night sky.
const TWINKLE_RATIO_PERCENT: u32 = 20;
const TWINKLE_CYCLE_LENGTH: u8 = 12;
const TWINKLE_LARGE_PHASE: u8 = 10;
const TWINKLE_SMALL_PHASES: [u8; 3] = [8, 9, 11];
const TWINKLE_PERIOD: f32 = 0.3;
/// The moon drifts down into view around this height, then stays put.
const MOON_HEIGHT: u32 = 75;
const MOON_X: i32 = 100;
const MOON_PARK_Y: i32 = 9;
const MOON_FULL_FRAME: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Playing,
    Falling,
    GameOver,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Row {
    x: i16,
    w: u8,
}

impl Row {
    fn right(&self) -> i32 {
        self.x as i32 + self.w as i32
    }

    fn overlaps(&self, other: Row) -> bool {
        (self.x as i32) < other.right() && (other.x as i32) < self.right()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Landing {
    kept: Row,
    overhang: Option<Row>,
    perfect: bool,
}

/// A trimmed piece falling away. `y` is in world space (see `row_world_y`).
struct Chunk {
    row: Row,
    y: f32,
    vy: f32,
}

struct Cloud {
    sprite: &'static Sprite,
    x: f32,
    /// Screen y when the camera is at 0; see `sky_y`.
    y: f32,
    vx: f32,
}

struct Bird {
    x: f32,
    /// In sky space, like clouds.
    y: f32,
    speed: f32,
    anim: f32,
}

struct Plant {
    sprite: &'static Sprite,
    x: i16,
    mirror: bool,
}

#[derive(Clone, Copy)]
struct Star {
    x: u8,
    y: u8,
    twinkles: bool,
    phase_offset: u8,
}

/// Where a box of width `w` dropped at `x` lands on `below`: the part that
/// overlaps, what's trimmed off, and whether it was close enough to snap.
/// None if it misses entirely.
fn land(below: Row, x: i32, w: u8) -> Option<Landing> {
    if (x - below.x as i32).abs() <= PERFECT_PX {
        return Some(Landing { kept: below, overhang: None, perfect: true });
    }
    let left = x.max(below.x as i32);
    let right = (x + w as i32).min(below.right());
    if right <= left {
        return None;
    }
    let overhang = if x < below.x as i32 {
        Row { x: x as i16, w: (below.x as i32 - x) as u8 }
    } else {
        Row { x: below.right() as i16, w: (x + w as i32 - below.right()) as u8 }
    };
    Some(Landing {
        kept: Row { x: left as i16, w: (right - left) as u8 },
        overhang: Some(overhang),
        perfect: false,
    })
}

fn speed_at(score: u32) -> f32 {
    SPEED_STEPS.iter().rev().find(|&&(h, _)| score >= h).map_or(SPEED_STEPS[0].1, |&(_, v)| v)
}

/// Top of row `k` relative to the base box's top; up is negative.
fn row_world_y(k: u32) -> f32 {
    -((k as i32 * BOX_H) as f32)
}

/// Where the camera settles once `height` rows are placed.
fn cam_for(height: u32) -> f32 {
    (ACTIVE_MIN_Y as f32 - BASE_Y as f32 - row_world_y(height)).max(0.0)
}

fn screen_y(world_y: f32, cam: f32) -> i32 {
    BASE_Y + world_y as i32 + cam as i32
}

/// Screen y of scenery placed at `y`, given the camera.
fn sky_y(y: f32, cam: f32) -> i32 {
    (y + cam * SKY_PARALLAX) as i32
}

pub struct BalancingScene {
    state: State,
    /// The last `RING` placed rows, row `k` at `k % RING`.
    rows: [Row; RING],
    /// Rows placed, base included. The sliding box is row `height`.
    height: u32,
    active_x: f32,
    active_w: u8,
    dir: f32,
    /// Screen offset pushing the tower down as it grows.
    cam: f32,
    chunks: Vec<Chunk, MAX_CHUNKS>,

    perfect_timer: f32,
    perfect_streak: u32,
    sparkles: BurstEffect,
    /// Centre x and row of the last box that grew, where the sparkles play.
    sparkle_at: (i32, u32),

    cat_frame: usize,
    cat_timer: f32,

    /// How far the missed box has fallen and how fast. It stops at
    /// `fall_stop` if it hits a lower row or the ground on the way down.
    fall_y: f32,
    fall_vy: f32,
    fall_stop: Option<f32>,
    /// The cat's poof: centre in world space and time since it started.
    poof: Option<(Point, f32)>,
    result_popup: Popup,

    clouds: Vec<Cloud, MAX_CLOUDS>,
    plants: Vec<Plant, MAX_PLANTS>,
    birds: Vec<Bird, MAX_BIRDS>,
    bird_wait: f32,
    shooting_star: Option<ShootingStarEvent>,
    shooting_star_wait: f32,
    stars: [Star; MAX_STARS],
    twinkle_timer: f32,
    twinkle_phase: u8,

    session_best: u32,
    /// Boxes stacked over all finished runs this visit.
    session_total: u32,
}

impl BalancingScene {
    pub fn new() -> Self {
        Self {
            state: State::Ready,
            rows: [Row { x: 0, w: 0 }; RING],
            height: 0,
            active_x: 0.0,
            active_w: 0,
            dir: 1.0,
            cam: 0.0,
            chunks: Vec::new(),

            perfect_timer: 0.0,
            perfect_streak: 0,
            sparkles: BurstEffect::new(),
            sparkle_at: (0, 0),

            cat_frame: 0,
            cat_timer: 0.0,

            fall_y: 0.0,
            fall_vy: 0.0,
            fall_stop: None,
            poof: None,
            result_popup: Popup::new(14, 13, 100, 38),

            clouds: Vec::new(),
            plants: Vec::new(),
            birds: Vec::new(),
            bird_wait: BIRD_WAIT_MIN,
            shooting_star: None,
            shooting_star_wait: SHOOTING_STAR_ODDS.0,
            stars: [Star { x: 0, y: 0, twinkles: false, phase_offset: 0 }; MAX_STARS],
            twinkle_timer: 0.0,
            twinkle_phase: 0,

            session_best: 0,
            session_total: 0,
        }
    }

    fn reset_round(&mut self, rng: &mut u32) {
        self.rows[0] = Row { x: ((DISPLAY_W - BASE_W as i32) / 2) as i16, w: BASE_W };
        self.height = 1;
        self.cam = 0.0;
        self.chunks.clear();
        self.perfect_timer = 0.0;
        self.perfect_streak = 0;
        self.sparkles = BurstEffect::new();
        self.fall_y = 0.0;
        self.fall_vy = 0.0;
        self.fall_stop = None;
        self.poof = None;
        self.state = State::Playing;
        self.spawn_box();
        self.generate_sky(rng);
        self.generate_plants(rng);
        self.birds.clear();
        self.bird_wait = rand_range_f32(rng, BIRD_WAIT_MIN, BIRD_WAIT_MAX);
        self.shooting_star = None;
        self.shooting_star_wait = rand_range_f32(rng, SHOOTING_STAR_ODDS.0, SHOOTING_STAR_ODDS.1);
    }

    /// Clouds spread through their band of heights, and a fresh starfield.
    /// Plants spaced out along the ground, walking left to right and
    /// skipping the base box.
    fn generate_plants(&mut self, rng: &mut u32) {
        self.plants.clear();
        let base = self.rows[0];
        let (clear_l, clear_r) = (base.x as i32 - PLANT_BASE_MARGIN, base.right() + PLANT_BASE_MARGIN);
        let mut x = (xorshift32(rng) % PLANT_GAP_MAX) as i32;
        let mut flower = xorshift32(rng) % 2 == 0;
        while x < DISPLAY_W && !self.plants.is_full() {
            let pool: &[&'static Sprite] = if flower { &FLOWER_SPRITES } else { &GRASS_SPRITES };
            let sprite = pool[xorshift32(rng) as usize % pool.len()];
            let w = sprite.width as i32;
            if x + w > clear_l && x < clear_r {
                x = clear_r;
                continue;
            }
            if x + w > DISPLAY_W {
                break;
            }
            let mirror = xorshift32(rng) % 2 == 0;
            let _ = self.plants.push(Plant { sprite, x: x as i16, mirror });
            flower = !flower;
            x += w + (PLANT_GAP_MIN + xorshift32(rng) % (PLANT_GAP_MAX - PLANT_GAP_MIN + 1)) as i32;
        }
    }

    fn generate_sky(&mut self, rng: &mut u32) {
        self.clouds.clear();
        let band = (CLOUD_LAST - CLOUD_FIRST) as f32 / MAX_CLOUDS as f32;
        for i in 0..MAX_CLOUDS {
            let sprite: &'static Sprite = match xorshift32(rng) % 6 {
                0 => &CLOUD1,
                1 | 2 => &CLOUD2,
                _ => &CLOUD3,
            };
            // Just above the top of the screen when the tower is this tall.
            let h = CLOUD_FIRST as f32 + band * (i as f32 + rand_range_f32(rng, 0.0, 1.0));
            let y = -(sprite.height as f32) - cam_for(h as u32) * SKY_PARALLAX;
            let x = rand_range_f32(rng, -(sprite.width as f32) / 2.0, DISPLAY_W as f32 - sprite.width as f32 / 2.0);
            let mut vx = rand_range_f32(rng, 1.0, CLOUD_DRIFT_MAX);
            if xorshift32(rng) % 2 == 0 {
                vx = -vx;
            }
            let _ = self.clouds.push(Cloud { sprite, x, y, vx });
        }

        for star in self.stars.iter_mut() {
            *star = Star {
                x: (xorshift32(rng) % DISPLAY_W as u32) as u8,
                y: (xorshift32(rng) % DISPLAY_H as u32) as u8,
                twinkles: xorshift32(rng) % 100 < TWINKLE_RATIO_PERCENT,
                phase_offset: (xorshift32(rng) % TWINKLE_CYCLE_LENGTH as u32) as u8,
            };
        }
    }

    fn score(&self) -> u32 {
        self.height - 1
    }

    fn top(&self) -> Row {
        self.rows[(self.height as usize - 1) % RING]
    }

    fn active_row(&self) -> Row {
        Row { x: self.active_x.round() as i16, w: self.active_w }
    }

    /// Next box, the width of the top row, entering from alternate sides.
    fn spawn_box(&mut self) {
        self.active_w = self.top().w;
        if self.height % 2 == 0 {
            self.active_x = 0.0;
            self.dir = 1.0;
        } else {
            self.active_x = (DISPLAY_W - self.active_w as i32) as f32;
            self.dir = -1.0;
        }
    }

    fn slide(&mut self, dt: f32) {
        let speed = speed_at(self.score());
        let max_x = (DISPLAY_W - self.active_w as i32) as f32;
        self.active_x += self.dir * speed * dt;
        if self.active_x < 0.0 {
            self.active_x = (-self.active_x).min(max_x);
            self.dir = 1.0;
        } else if self.active_x > max_x {
            self.active_x = (2.0 * max_x - self.active_x).max(0.0);
            self.dir = -1.0;
        }
    }

    fn drop_box(&mut self, rng: &mut u32) {
        let Some(landing) = land(self.top(), self.active_x.round() as i32, self.active_w) else {
            self.start_fall();
            return;
        };

        if let Some(row) = landing.overhang {
            if self.chunks.is_full() {
                self.chunks.remove(0);
            }
            let _ = self.chunks.push(Chunk { row, y: row_world_y(self.height), vy: 0.0 });
        }

        let mut kept = landing.kept;
        if landing.perfect {
            self.perfect_streak += 1;
            self.perfect_timer = PERFECT_FLASH;
            if self.perfect_streak % GROW_STREAK == 0 && kept.w < BASE_W {
                let w = (kept.w + GROW_PX).min(BASE_W);
                let x = kept.x as i32 - (w - kept.w) as i32 / 2;
                kept = Row { x: x.clamp(0, DISPLAY_W - w as i32) as i16, w };
                let spread = w as f32 / 2.0 + 4.0;
                self.sparkles.trigger_with(rng, GROW_SPARKLES, DEFAULT_STYLE, -spread, spread, -8.0, 4.0, GROW_SPARKLE_STAGGER);
                self.sparkle_at = (kept.x as i32 + w as i32 / 2, self.height);
            }
        } else {
            self.perfect_streak = 0;
        }

        self.rows[self.height as usize % RING] = kept;
        self.height += 1;
        self.session_best = self.session_best.max(self.score());
        self.spawn_box();
    }

    /// The box missed the top row. Work out what it hits on the way down:
    /// the highest lower row under it, or the ground if that's in memory.
    fn start_fall(&mut self) {
        self.state = State::Falling;
        self.fall_y = 0.0;
        self.fall_vy = 0.0;
        self.poof = None;
        self.session_total += self.score();

        let active = self.active_row();
        let lowest = self.height.saturating_sub(RING as u32);
        let hit = (lowest..self.height).rev().find(|&k| self.rows[k as usize % RING].overlaps(active));
        let rest = match hit {
            Some(k) => Some(row_world_y(k + 1)),
            None if lowest == 0 => Some(row_world_y(0)),
            None => None,
        };
        self.fall_stop = rest.map(|y| y - row_world_y(self.height));
    }

    fn update_fall(&mut self, dt: f32) {
        let landed = self.fall_stop.is_some_and(|s| self.fall_y >= s);
        if !landed {
            self.fall_vy += GRAVITY * dt;
            self.fall_y += self.fall_vy * dt;
            if let Some(stop) = self.fall_stop {
                self.fall_y = self.fall_y.min(stop);
            }
        }

        let box_y = row_world_y(self.height) + self.fall_y;
        match &mut self.poof {
            None => {
                // Poof on impact, or as the cat reaches the bottom of the
                // screen if whatever it would hit is out of sight.
                let hit = self.fall_stop.is_some_and(|s| self.fall_y >= s);
                if hit || screen_y(box_y, self.cam) >= DISPLAY_H - 2 {
                    let row = self.active_row();
                    let cx = row.x as i32 + row.w as i32 / 2;
                    let cy = box_y as i32 - PLATFORMER_CAT_JUMP.height as i32 / 2;
                    self.poof = Some((Point::new(cx, cy), 0.0));
                }
            }
            Some((_, t)) => {
                *t += dt;
                if *t >= POOF_FRAME_DUR * POOF.frames.len() as f32 + RESULT_PAUSE {
                    self.state = State::GameOver;
                    self.set_result_text();
                }
            }
        }
    }

    fn set_result_text(&mut self) {
        let mut text: String<64> = String::new();
        let mut n: String<8> = String::new();
        let _ = write!(n, "{}", self.score());
        crate::i18n::substitute(&mut text, t!("\n\nScore: {n}"), &[("n", n.as_str())]);
        let _ = text.push('\n');
        n.clear();
        let _ = write!(n, "{}", self.session_best);
        crate::i18n::substitute(&mut text, t!("Best: {n}"), &[("n", n.as_str())]);
        let _ = text.push('\n');
        let _ = text.push_str(t!("A: RETRY"));
        self.result_popup.set_text(text.trim_start_matches('\n'), false, true);
    }

    fn update_chunks(&mut self, dt: f32) {
        for c in self.chunks.iter_mut() {
            c.vy += GRAVITY * dt;
            c.y += c.vy * dt;
        }
        let cam = self.cam;
        self.chunks.retain(|c| screen_y(c.y, cam) < DISPLAY_H);
    }

    fn update_camera(&mut self, dt: f32) {
        let target = cam_for(self.height);
        self.cam += (target - self.cam) * (CAM_EASE * dt).min(1.0);
    }

    fn update_birds(&mut self, rng: &mut u32, dt: f32) {
        for b in self.birds.iter_mut() {
            b.x -= b.speed * dt;
            b.anim += BIRD_ANIM_FPS * dt;
        }
        self.birds.retain(|b| b.x > -(SMALL_BIRD1.width as f32));

        if !(BIRDS_FROM..STARS_FROM).contains(&self.score()) || self.birds.is_full() {
            return;
        }
        self.bird_wait -= dt;
        if self.bird_wait > 0.0 {
            return;
        }
        self.bird_wait = rand_range_f32(rng, BIRD_WAIT_MIN, BIRD_WAIT_MAX);
        if rand_range_f32(rng, 0.0, 1.0) < BIRD_CHANCE {
            let screen_y = rand_range_f32(rng, BIRD_Y_MIN, BIRD_Y_MAX);
            let _ = self.birds.push(Bird {
                x: DISPLAY_W as f32,
                y: screen_y - self.cam * SKY_PARALLAX,
                speed: rand_range_f32(rng, BIRD_SPEED_MIN, BIRD_SPEED_MAX),
                anim: 0.0,
            });
        }
    }

    fn update_shooting_star(&mut self, rng: &mut u32, dt: f32) {
        if let Some(ss) = &mut self.shooting_star {
            ss.update(dt);
            if !ss.is_active() {
                self.shooting_star = None;
            }
            return;
        }
        let score = self.score();
        if score < SHOOTING_STARS_FROM {
            return;
        }
        self.shooting_star_wait -= dt;
        if self.shooting_star_wait > 0.0 {
            return;
        }
        let (wait_min, wait_max, chance) = if score >= SHOOTING_STARS_MORE_FROM {
            SHOOTING_STAR_MORE_ODDS
        } else {
            SHOOTING_STAR_ODDS
        };
        self.shooting_star_wait = rand_range_f32(rng, wait_min, wait_max);
        if rand_range_f32(rng, 0.0, 1.0) < chance {
            self.shooting_star = Some(ShootingStarEvent::random(rng));
        }
    }

    fn update_ambient(&mut self, dt: f32) {
        self.cat_timer += dt;
        if self.cat_timer >= 1.0 / CAT_WAG_FPS {
            self.cat_timer -= 1.0 / CAT_WAG_FPS;
            self.cat_frame = (self.cat_frame + 1) % PLATFORMER_CAT_SIT.frames.len();
        }

        for c in self.clouds.iter_mut() {
            let w = c.sprite.width as f32;
            c.x += c.vx * dt;
            if c.x > DISPLAY_W as f32 {
                c.x = -w;
            } else if c.x < -w {
                c.x = DISPLAY_W as f32;
            }
        }

        self.twinkle_timer += dt;
        if self.twinkle_timer > TWINKLE_PERIOD {
            self.twinkle_timer = 0.0;
            self.twinkle_phase = (self.twinkle_phase + 1) % TWINKLE_CYCLE_LENGTH;
        }

        self.sparkles.update(dt);
    }

    fn apply_rewards(&self, ctx: &mut GameContext) {
        let mut total = self.session_total;
        if matches!(self.state, State::Ready | State::Playing) {
            total += self.score();
        }
        if total == 0 {
            return;
        }
        // A decent run is 15-25 boxes.
        let progress = (total as f32 / 40.0).sqrt();
        ctx.apply_stat_changes(&[
            (StatId::Focus,       5.0 * progress),
            (StatId::Playfulness, 3.0 * progress),
            (StatId::Fitness,     2.0 * progress),
            (StatId::Serenity,    2.0 * progress),
        ]);
        let coins = (4.0 * progress) as i32;
        if coins > 0 {
            ctx.coins += coins;
        }
    }

    // -----------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------

    fn draw_sky(&self, r: &mut Renderer) {
        // Stars fade in by count as the camera climbs.
        let (from, full) = (cam_for(STARS_FROM), cam_for(STARS_FULL));
        let visible = (((self.cam - from) / (full - from)).clamp(0.0, 1.0) * MAX_STARS as f32) as usize;
        let drift = (self.cam * STAR_PARALLAX) as i32;
        for star in &self.stars[..visible] {
            let x = star.x as i32;
            let y = (star.y as i32 + drift).rem_euclid(DISPLAY_H);
            r.draw_pixel(Point::new(x, y), true);
            if !star.twinkles {
                continue;
            }
            let phase = (self.twinkle_phase + star.phase_offset) % TWINKLE_CYCLE_LENGTH;
            if phase == TWINKLE_LARGE_PHASE || TWINKLE_SMALL_PHASES.contains(&phase) {
                r.draw_pixel(Point::new(x - 1, y), true);
                r.draw_pixel(Point::new(x + 1, y), true);
            }
            if phase == TWINKLE_LARGE_PHASE {
                r.draw_pixel(Point::new(x, y - 1), true);
                r.draw_pixel(Point::new(x, y + 1), true);
            }
        }

        if let Some(ss) = &self.shooting_star {
            ss.draw(r);
        }

        let moon_y = MOON_PARK_Y - ((cam_for(MOON_HEIGHT) - self.cam) * SKY_PARALLAX) as i32;
        let moon_y = moon_y.min(MOON_PARK_Y);
        if moon_y > -(MOON.height as i32) {
            let opts = SpriteOpts { frame: MOON_FULL_FRAME, ..SpriteOpts::default() };
            r.draw_sprite(&MOON, Point::new(MOON_X, moon_y), opts);
        }

        for c in &self.clouds {
            let y = sky_y(c.y, self.cam);
            if y < DISPLAY_H && y + c.sprite.height as i32 > 0 {
                r.draw_sprite(c.sprite, Point::new(c.x as i32, y), SpriteOpts::default());
            }
        }

        for b in &self.birds {
            let frame = b.anim as usize % SMALL_BIRD1.frames.len();
            let pos = Point::new(b.x as i32, sky_y(b.y, self.cam));
            r.draw_sprite(&SMALL_BIRD1, pos, SpriteOpts { frame, ..SpriteOpts::default() });
        }
    }

    fn draw_tower(&self, r: &mut Renderer) {
        let ground = BASE_Y + BOX_H + self.cam as i32;
        if ground < DISPLAY_H {
            r.draw_line(Point::new(0, ground), Point::new(DISPLAY_W - 1, ground));
        }
        // Plants stand on the ground line.
        if ground - PLANT_MAX_H < DISPLAY_H {
            for p in &self.plants {
                let pos = Point::new(p.x as i32, ground + 1 - p.sprite.height as i32);
                r.draw_sprite(p.sprite, pos, SpriteOpts { mirror_h: p.mirror, ..SpriteOpts::default() });
            }
        }
        for k in self.height.saturating_sub(RING as u32)..self.height {
            draw_box(r, self.rows[k as usize % RING], screen_y(row_world_y(k), self.cam), false);
        }
        for c in &self.chunks {
            draw_box(r, c.row, screen_y(c.y, self.cam), false);
        }
    }

    fn draw_active(&self, r: &mut Renderer) {
        let row = self.active_row();
        let y = screen_y(row_world_y(self.height) + self.fall_y, self.cam);
        draw_box(r, row, y, true);

        let mid_x = row.x as i32 + row.w as i32 / 2;
        let opts = SpriteOpts { mirror_h: self.dir < 0.0, ..SpriteOpts::default() };
        match (self.state, self.poof) {
            (State::Ready | State::Playing, _) => {
                let cat = &PLATFORMER_CAT_SIT;
                let pos = Point::new(mid_x - cat.width as i32 / 2, y - cat.height as i32);
                r.draw_sprite(cat, pos, SpriteOpts { frame: self.cat_frame, ..opts });
            }
            (State::Falling, None) => {
                let cat = &PLATFORMER_CAT_JUMP;
                let pos = Point::new(mid_x - cat.width as i32 / 2, y - cat.height as i32);
                r.draw_sprite(cat, pos, SpriteOpts { frame: CAT_FALL_FRAME, ..opts });
            }
            (_, Some((at, t))) => {
                let frame = (t / POOF_FRAME_DUR) as usize;
                if frame < POOF.frames.len() {
                    let pos = Point::new(
                        at.x - POOF.width as i32 / 2,
                        screen_y(at.y as f32, self.cam) - POOF.height as i32 / 2,
                    );
                    r.draw_sprite(&POOF, pos, SpriteOpts { frame, ..SpriteOpts::default() });
                }
            }
            _ => {}
        }

        if self.perfect_timer > 0.0 {
            let text = t!("Perfect!");
            let x = (DISPLAY_W - 4 * text.chars().count() as i32) / 2;
            let cat_top = screen_y(row_world_y(self.height), self.cam) - PLATFORMER_CAT_SIT.height as i32;
            r.draw_text_small(text, Point::new(x, cat_top - 7));
        }
    }

    fn draw_sparkles(&self, r: &mut Renderer) {
        let (x, k) = self.sparkle_at;
        let y = screen_y(row_world_y(k), self.cam) + BOX_H / 2;
        self.sparkles.draw(r, Point::new(x, y));
    }

    fn draw_hud(&self, r: &mut Renderer) {
        let mut n: String<8> = String::new();
        let _ = write!(n, "{}", self.score());
        r.draw_text_small(n.as_str(), Point::new(2, 1));

        if self.session_best > 0 {
            n.clear();
            let _ = write!(n, "{}", self.session_best);
            let mut best: String<24> = String::new();
            crate::i18n::substitute(&mut best, t!("Best: {n}"), &[("n", n.as_str())]);
            let x = DISPLAY_W - 1 - 4 * best.chars().count() as i32;
            r.draw_text_small(best.as_str(), Point::new(x, 1));
        }
    }
}

/// Placed boxes are outlined, hiding the sky behind them; the sliding one is
/// solid so it stands out.
fn draw_box(r: &mut Renderer, row: Row, y: i32, solid: bool) {
    if y >= DISPLAY_H || y + BOX_H <= 0 || row.w == 0 {
        return;
    }
    let (pos, size) = (Point::new(row.x as i32, y), Size::new(row.w as u32, BOX_H as u32));
    if !solid {
        r.fill_rect_off(pos, size);
    }
    r.draw_rect(pos, size, solid);
}

impl Scene for BalancingScene {
    fn enter(&mut self, ctx: &mut GameContext) {
        self.session_best = 0;
        self.session_total = 0;
        let mut rng = ctx.rng;
        self.reset_round(&mut rng);
        ctx.rng = rng;
        // Only the first round shows the controls popup.
        self.state = State::Ready;
    }

    fn exit(&mut self, ctx: &mut GameContext) {
        self.apply_rewards(ctx);
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

        let a = buttons.was_just_pressed(Button::A);
        let mut rng = ctx.rng;
        match self.state {
            State::Ready | State::Playing => {
                // Drop from where the box was last drawn, not a frame ahead.
                if a {
                    self.state = State::Playing;
                    self.drop_box(&mut rng);
                } else {
                    self.slide(dt);
                }
            }
            State::Falling => self.update_fall(dt),
            State::GameOver => {
                if a {
                    self.reset_round(&mut rng);
                }
            }
        }
        self.update_birds(&mut rng, dt);
        self.update_shooting_star(&mut rng, dt);
        ctx.rng = rng;

        self.perfect_timer = (self.perfect_timer - dt).max(0.0);
        self.update_chunks(dt);
        self.update_camera(dt);
        self.update_ambient(dt);
        None
    }

    fn draw(&self, _ctx: &GameContext, renderer: &mut Renderer, _dt_ms: u64) {
        self.draw_sky(renderer);
        self.draw_tower(renderer);
        self.draw_active(renderer);
        self.draw_sparkles(renderer);
        self.draw_hud(renderer);

        match self.state {
            State::Ready => {
                let mut popup = Popup::new(10, 8, 108, 18);
                popup.set_text(t!("A: Drop"), false, true);
                popup.draw(renderer, false);
            }
            State::GameOver => self.result_popup.draw(renderer, false),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landing_trims_overhang_and_snaps_near_misses() {
        let below = Row { x: 40, w: 20 };

        // Overhanging on the right.
        let l = land(below, 45, 20).unwrap();
        assert_eq!(l.kept, Row { x: 45, w: 15 });
        assert_eq!(l.overhang, Some(Row { x: 60, w: 5 }));
        assert!(!l.perfect);

        // Overhanging on the left.
        let l = land(below, 33, 20).unwrap();
        assert_eq!(l.kept, Row { x: 40, w: 13 });
        assert_eq!(l.overhang, Some(Row { x: 33, w: 7 }));

        // Within a pixel snaps exactly.
        let l = land(below, 41, 20).unwrap();
        assert_eq!(l, Landing { kept: below, overhang: None, perfect: true });

        // Clean misses either side.
        assert!(land(below, 60, 20).is_none());
        assert!(land(below, 20, 20).is_none());
    }
}
