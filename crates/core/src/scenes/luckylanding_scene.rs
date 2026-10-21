//! Lucky Landing minigame, a cat's take on Lunar Lander. The cat is falling
//! and has to land on its feet: rotate with Left/Right, thrust with Up, and
//! touch down gently and upright on one of the flat ledges. The narrower the
//! ledge, the bigger the score multiplier.
//!
//! The world is several screens wide and tall. The camera follows the cat
//! with a dead zone, scrolling down earlier than up so landing spots come
//! into view before the cat reaches them.

use core::fmt::Write as _;

use embedded_graphics::prelude::{Point, Size};
use heapless::{String, Vec};
#[cfg(not(feature = "desktop"))]
use micromath::F32Ext;
use crate::t;

use crate::{
    assets::{
        effects::{BURST1, BURST1_FRAME_DUR, POOF},
        minigame_character::LANDER_CAT,
    },
    board::{DISPLAY_HEIGHT, DISPLAY_WIDTH},
    context::{GameContext, StatId},
    input::{Button, Buttons},
    rand::{rand_range_f32, rand_range_u32},
    render::{Renderer, SpriteOpts},
    scene::{Scene, SceneId},
    ui::popup::Popup,
};

const DISPLAY_W: i32 = DISPLAY_WIDTH as i32;
const DISPLAY_H: i32 = DISPLAY_HEIGHT as i32;

// World
const WORLD_W: i32 = 512;
const WORLD_H: i32 = 192;
const TERRAIN_STEP: i32 = 4;
const TERRAIN_POINTS: usize = (WORLD_W / TERRAIN_STEP) as usize + 1;
/// Terrain stays between these heights, leaving open sky above.
const TERRAIN_MIN_Y: i32 = WORLD_H * 9 / 20;
const TERRAIN_MAX_Y: i32 = WORLD_H - 6;
/// Max change in height between neighbouring terrain points.
const TERRAIN_MAX_SLOPE: f32 = 6.0;

// Landing pads: width in terrain segments and the score multiplier for it.
// Each round uses a few distinct sizes.
const PAD_SIZES: [(u16, u8); 4] = [(3, 5), (4, 3), (6, 2), (8, 1)];
const MIN_PADS: u32 = 3;
const MAX_PADS: usize = 4;

// Cat physics (px, seconds, degrees). Angle 0 is upright, positive is
// clockwise on screen.
const GRAVITY: f32 = 4.5;
const THRUST: f32 = 16.0;
const TURN_SPEED: f32 = 90.0;
const MAX_ANGLE: f32 = 90.0;
const FUEL_MAX: f32 = 100.0;
const FUEL_BURN: f32 = 9.0;
const SPAWN_Y: f32 = 12.0;
const SPAWN_DRIFT: f32 = 6.0;

// Landing tolerances.
const SAFE_ANGLE: f32 = 12.0;
const SAFE_VY: f32 = 12.0;
const SAFE_VX: f32 = 8.0;
const LANDING_BASE_SCORE: f32 = 50.0;

/// Leg tips relative to the cat's center, unrotated.
const CAT_HALF_W: f32 = LANDER_CAT.width as f32 / 2.0;
const CAT_HALF_H: f32 = LANDER_CAT.height as f32 / 2.0;
const LEG_TIPS: [(f32, f32); 2] = [(-4.5, CAT_HALF_H), (4.5, CAT_HALF_H)];
/// Body corners relative to the cat's center, so a tumbling cat still hits
/// the ground.
const BODY_CORNERS: [(f32, f32); 4] = [
    (-CAT_HALF_W, -CAT_HALF_H),
    (CAT_HALF_W, -CAT_HALF_H),
    (CAT_HALF_W, 1.0),
    (-CAT_HALF_W, 1.0),
];

// Thrust sparkles
const MAX_SPARKS: usize = 16;
const SPARK_INTERVAL: f32 = 0.06;
const SPARK_SPEED: f32 = 20.0;
const SPARK_JITTER: f32 = 6.0;
const SPARK_LIFE: f32 = BURST1_FRAME_DUR * 5.0;

// Camera dead zone in screen px: 25%/75% horizontally, 25% from the top and
// 50% from the bottom.
const CAM_LEFT: f32 = DISPLAY_W as f32 * 0.25;
const CAM_RIGHT: f32 = DISPLAY_W as f32 * 0.75;
const CAM_TOP: f32 = DISPLAY_H as f32 * 0.25;
const CAM_BOTTOM: f32 = DISPLAY_H as f32 * 0.5;

// Fuel bar in the top-right corner, with a 1px cap at each end.
const FUEL_BAR_W: i32 = 30;
const FUEL_BAR_H: u32 = 2;
const FUEL_BAR_X: i32 = DISPLAY_W - FUEL_BAR_W - 2;

// Starfield, fixed in the world so it scrolls with the camera and shows how
// fast the cat is moving when no terrain is in view. Twinkles on the same
// cycle as the outdoor night sky (`sky.rs`).
const STAR_COUNT: usize = 160;
/// Stars stay this far above the ground so a twinkle never touches it.
const STAR_GROUND_MARGIN: f32 = 3.0;
const TWINKLE_RATIO_PERCENT: u32 = 20;
const TWINKLE_CYCLE_LENGTH: u8 = 12;
const TWINKLE_LARGE_PHASE: u8 = 10;
const TWINKLE_SMALL_PHASES: [u8; 3] = [8, 9, 11];
const TWINKLE_PERIOD: f32 = 0.3;

const POOF_FRAME_DUR: f32 = 1.0 / 8.0;
/// Seconds after touchdown before the result popup appears, so the landing
/// or the poof can be seen first.
const RESULT_DELAY: f32 = POOF_FRAME_DUR * 5.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Playing,
    Landed,
    Crashed,
}

#[derive(Clone, Copy, Debug)]
struct Pad {
    /// First and last terrain point of the flat stretch.
    i0: u16,
    i1: u16,
    y: i16,
    mult: u8,
}

impl Pad {
    fn x0(&self) -> f32 {
        (self.i0 as i32 * TERRAIN_STEP) as f32
    }

    fn x1(&self) -> f32 {
        (self.i1 as i32 * TERRAIN_STEP) as f32
    }
}

struct Terrain {
    heights: [i16; TERRAIN_POINTS],
    pads: Vec<Pad, MAX_PADS>,
}

impl Terrain {
    fn generate(rng: &mut u32) -> Self {
        let mut heights = [0i16; TERRAIN_POINTS];
        let lo = TERRAIN_MIN_Y as f32;
        let hi = TERRAIN_MAX_Y as f32;
        let mut h = rand_range_f32(rng, lo + 20.0, hi - 20.0);
        let mut slope = 0.0;
        for p in heights.iter_mut() {
            slope = (slope + rand_range_f32(rng, -2.5, 2.5))
                .clamp(-TERRAIN_MAX_SLOPE, TERRAIN_MAX_SLOPE);
            h += slope;
            if h < lo || h > hi {
                h = h.clamp(lo, hi);
                slope = -slope * 0.5;
            }
            *p = h as i16;
        }

        // Shuffle the pad sizes so each round gets a different mix, then give
        // each pad its own zone of the world so they never overlap.
        let mut sizes = PAD_SIZES;
        for i in (1..sizes.len()).rev() {
            let j = rand_range_u32(rng, 0, i as u32) as usize;
            sizes.swap(i, j);
        }
        let count = rand_range_u32(rng, MIN_PADS, MAX_PADS as u32) as usize;
        let zone = (TERRAIN_POINTS - 1) / count;
        let mut pads = Vec::new();
        for (z, &(len, mult)) in sizes.iter().take(count).enumerate() {
            let zone_start = z * zone + 2;
            let zone_end = (z + 1) * zone - 2 - len as usize;
            let i0 = rand_range_u32(rng, zone_start as u32, zone_end as u32) as usize;
            let i1 = i0 + len as usize;
            let y = heights[i0..=i1].iter().map(|&v| v as i32).sum::<i32>() / (len as i32 + 1);
            for p in &mut heights[i0..=i1] {
                *p = y as i16;
            }
            let _ = pads.push(Pad { i0: i0 as u16, i1: i1 as u16, y: y as i16, mult });
        }
        Self { heights, pads }
    }

    /// Ground height at world x, interpolated between terrain points.
    fn ground_y(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, WORLD_W as f32);
        let i = ((x as i32 / TERRAIN_STEP) as usize).min(TERRAIN_POINTS - 2);
        let frac = (x - (i as i32 * TERRAIN_STEP) as f32) / TERRAIN_STEP as f32;
        let a = self.heights[i] as f32;
        let b = self.heights[i + 1] as f32;
        a + (b - a) * frac
    }

    fn pad_at(&self, x: f32) -> Option<usize> {
        self.pads.iter().position(|p| x >= p.x0() && x <= p.x1())
    }
}

#[derive(Clone, Copy)]
struct Star {
    x: i16,
    y: i16,
    twinkles: bool,
    phase_offset: u8,
}

const NO_STAR: Star = Star { x: 0, y: 0, twinkles: false, phase_offset: 0 };

#[derive(Clone, Copy)]
struct Spark {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    age: f32,
}

pub struct LuckyLandingScene {
    state: State,
    terrain: Terrain,
    stars: [Star; STAR_COUNT],
    twinkle_timer: f32,
    twinkle_phase: u8,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    angle: f32,
    fuel: f32,
    thrusting: bool,
    sparks: Vec<Spark, MAX_SPARKS>,
    spark_timer: f32,
    cam_x: f32,
    cam_y: f32,
    /// Seconds since touchdown or crash.
    result_timer: f32,
    last_score: i32,
    session_score: i32,
}

impl LuckyLandingScene {
    pub fn new() -> Self {
        Self {
            state: State::Ready,
            terrain: Terrain { heights: [0; TERRAIN_POINTS], pads: Vec::new() },
            stars: [NO_STAR; STAR_COUNT],
            twinkle_timer: 0.0,
            twinkle_phase: 0,
            x: 0.0,
            y: 0.0,
            vx: 0.0,
            vy: 0.0,
            angle: 0.0,
            fuel: FUEL_MAX,
            thrusting: false,
            sparks: Vec::new(),
            spark_timer: 0.0,
            cam_x: 0.0,
            cam_y: 0.0,
            result_timer: 0.0,
            last_score: 0,
            session_score: 0,
        }
    }

    fn reset_round(&mut self, rng: &mut u32) {
        self.state = State::Ready;
        self.terrain = Terrain::generate(rng);
        self.generate_stars(rng);
        self.x = rand_range_f32(rng, 64.0, (WORLD_W - 64) as f32);
        self.y = SPAWN_Y;
        self.vx = rand_range_f32(rng, -SPAWN_DRIFT, SPAWN_DRIFT);
        self.vy = 0.0;
        self.angle = 0.0;
        self.fuel = FUEL_MAX;
        self.thrusting = false;
        self.sparks.clear();
        self.spark_timer = 0.0;
        self.result_timer = 0.0;
        self.last_score = 0;
        self.cam_x = self.x - DISPLAY_W as f32 / 2.0;
        self.cam_y = 0.0;
        self.update_camera();
    }

    /// Scatter stars over the sky, keeping them clear of the terrain.
    fn generate_stars(&mut self, rng: &mut u32) {
        for star in &mut self.stars {
            let x = rand_range_f32(rng, 0.0, WORLD_W as f32);
            let top = self.terrain.ground_y(x) - STAR_GROUND_MARGIN;
            *star = Star {
                x: x as i16,
                y: rand_range_f32(rng, 0.0, top) as i16,
                twinkles: rand_range_u32(rng, 0, 99) < TWINKLE_RATIO_PERCENT,
                phase_offset: rand_range_u32(rng, 0, TWINKLE_CYCLE_LENGTH as u32 - 1) as u8,
            };
        }
    }

    /// Rotate a point relative to the cat's center by its angle and return
    /// it in world coordinates.
    fn to_world(&self, (lx, ly): (f32, f32)) -> (f32, f32) {
        let theta = self.angle * core::f32::consts::PI / 180.0;
        let (s, c) = (theta.sin(), theta.cos());
        (self.x + lx * c - ly * s, self.y + lx * s + ly * c)
    }

    fn update_flight(&mut self, buttons: &Buttons, dt: f32) {
        if buttons.is_pressed(Button::Left) {
            self.angle -= TURN_SPEED * dt;
        }
        if buttons.is_pressed(Button::Right) {
            self.angle += TURN_SPEED * dt;
        }
        self.angle = self.angle.clamp(-MAX_ANGLE, MAX_ANGLE);

        self.thrusting = buttons.is_pressed(Button::Up) && self.fuel > 0.0;
        let mut ax = 0.0;
        let mut ay = GRAVITY;
        if self.thrusting {
            let theta = self.angle * core::f32::consts::PI / 180.0;
            ax += THRUST * theta.sin();
            ay -= THRUST * theta.cos();
            self.fuel = (self.fuel - FUEL_BURN * dt).max(0.0);
        }
        self.vx += ax * dt;
        self.vy += ay * dt;
        self.x += self.vx * dt;
        self.y += self.vy * dt;

        let min_x = CAT_HALF_W;
        let max_x = WORLD_W as f32 - CAT_HALF_W;
        if self.x < min_x {
            self.x = min_x;
            self.vx = 0.0;
        } else if self.x > max_x {
            self.x = max_x;
            self.vx = 0.0;
        }
        if self.y < CAT_HALF_H {
            self.y = CAT_HALF_H;
            self.vy = 0.0;
        }

        self.check_contact();
    }

    fn check_contact(&mut self) {
        let legs = LEG_TIPS.map(|p| self.to_world(p));
        let corners = BODY_CORNERS.map(|p| self.to_world(p));
        let touching = legs
            .iter()
            .chain(corners.iter())
            .any(|&(x, y)| y >= self.terrain.ground_y(x));
        if !touching {
            return;
        }

        let pad = self.terrain.pad_at(legs[0].0);
        let on_pad = pad.is_some() && pad == self.terrain.pad_at(legs[1].0);
        let gentle = self.angle.abs() <= SAFE_ANGLE
            && self.vy <= SAFE_VY
            && self.vx.abs() <= SAFE_VX;

        self.thrusting = false;
        self.result_timer = 0.0;
        match pad {
            Some(i) if on_pad && gentle => {
                let pad = self.terrain.pads[i];
                // Settle upright with both feet on the ledge.
                self.angle = 0.0;
                self.y = pad.y as f32 - CAT_HALF_H;
                self.last_score = (pad.mult as f32 * (LANDING_BASE_SCORE + self.fuel)) as i32;
                self.session_score += self.last_score;
                self.state = State::Landed;
            }
            _ => self.state = State::Crashed,
        }
        self.vx = 0.0;
        self.vy = 0.0;
    }

    fn update_sparks(&mut self, rng: &mut u32, dt: f32) {
        let mut i = 0;
        while i < self.sparks.len() {
            let s = &mut self.sparks[i];
            s.age += dt;
            s.x += s.vx * dt;
            s.y += s.vy * dt;
            if s.age >= SPARK_LIFE {
                self.sparks.swap_remove(i);
            } else {
                i += 1;
            }
        }

        if !self.thrusting {
            self.spark_timer = 0.0;
            return;
        }
        self.spark_timer -= dt;
        if self.spark_timer > 0.0 || self.sparks.is_full() {
            return;
        }
        self.spark_timer = SPARK_INTERVAL;
        // Shoot out of the cat's underside, away from the thrust.
        let (x, y) = self.to_world((0.0, CAT_HALF_H + 2.0));
        let theta = self.angle * core::f32::consts::PI / 180.0;
        let _ = self.sparks.push(Spark {
            x,
            y,
            vx: self.vx - SPARK_SPEED * theta.sin()
                + rand_range_f32(rng, -SPARK_JITTER, SPARK_JITTER),
            vy: self.vy + SPARK_SPEED * theta.cos()
                + rand_range_f32(rng, -SPARK_JITTER, SPARK_JITTER),
            age: 0.0,
        });
    }

    /// Move the camera just enough to keep the cat inside the dead zone.
    fn update_camera(&mut self) {
        let sx = self.x - self.cam_x;
        let sy = self.y - self.cam_y;
        if sx < CAM_LEFT {
            self.cam_x = self.x - CAM_LEFT;
        } else if sx > CAM_RIGHT {
            self.cam_x = self.x - CAM_RIGHT;
        }
        if sy < CAM_TOP {
            self.cam_y = self.y - CAM_TOP;
        } else if sy > CAM_BOTTOM {
            self.cam_y = self.y - CAM_BOTTOM;
        }
        self.cam_x = self.cam_x.clamp(0.0, (WORLD_W - DISPLAY_W) as f32);
        self.cam_y = self.cam_y.clamp(0.0, (WORLD_H - DISPLAY_H) as f32);
    }

    fn cam(&self) -> (i32, i32) {
        (self.cam_x as i32, self.cam_y as i32)
    }

    fn draw_stars(&self, renderer: &mut Renderer) {
        let (cx, cy) = self.cam();
        for star in &self.stars {
            let x = star.x as i32 - cx;
            let y = star.y as i32 - cy;
            if x < 0 || x >= DISPLAY_W || y < 0 || y >= DISPLAY_H {
                continue;
            }
            // Keep the fuel bar readable.
            if y <= FUEL_BAR_H as i32 + 1 && x >= FUEL_BAR_X - 4 {
                continue;
            }
            renderer.draw_pixel(Point::new(x, y), true);
            if !star.twinkles {
                continue;
            }
            let phase = (self.twinkle_phase + star.phase_offset) % TWINKLE_CYCLE_LENGTH;
            if phase == TWINKLE_LARGE_PHASE || TWINKLE_SMALL_PHASES.contains(&phase) {
                renderer.draw_pixel(Point::new(x - 1, y), true);
                renderer.draw_pixel(Point::new(x + 1, y), true);
            }
            if phase == TWINKLE_LARGE_PHASE {
                renderer.draw_pixel(Point::new(x, y - 1), true);
                renderer.draw_pixel(Point::new(x, y + 1), true);
            }
        }
    }

    fn draw_terrain(&self, renderer: &mut Renderer) {
        let (cx, cy) = self.cam();
        let first = (cx / TERRAIN_STEP) as usize;
        let last = (((cx + DISPLAY_W) / TERRAIN_STEP) as usize + 1).min(TERRAIN_POINTS - 1);
        let h = &self.terrain.heights;
        for i in first..last {
            renderer.draw_line(
                Point::new(i as i32 * TERRAIN_STEP - cx, h[i] as i32 - cy),
                Point::new((i as i32 + 1) * TERRAIN_STEP - cx, h[i + 1] as i32 - cy),
            );
        }

        for pad in &self.terrain.pads {
            let x0 = pad.x0() as i32 - cx;
            let x1 = pad.x1() as i32 - cx;
            if x1 < 0 || x0 >= DISPLAY_W {
                continue;
            }
            let y = pad.y as i32 - cy;
            renderer.draw_line(Point::new(x0, y + 1), Point::new(x1, y + 1));

            let mut label: String<4> = String::new();
            let _ = write!(label, "x{}", pad.mult);
            let label_x = (x0 + x1) / 2 - label.len() as i32 * 2; // FONT_4X6
            renderer.draw_text_small(label.as_str(), Point::new(label_x, y + 4));
        }
    }

    fn draw_cat(&self, renderer: &mut Renderer) {
        let (cx, cy) = self.cam();
        if self.state == State::Crashed {
            let frame = (self.result_timer / POOF_FRAME_DUR) as usize;
            if frame < POOF.frames.len() {
                let pos = Point::new(
                    self.x as i32 - cx - POOF.width as i32 / 2,
                    self.y as i32 - cy - POOF.height as i32 / 2,
                );
                renderer.draw_sprite(&POOF, pos, SpriteOpts { frame, transparent: true, ..Default::default() });
            }
            return;
        }
        let pos = Point::new(
            (self.x - CAT_HALF_W) as i32 - cx,
            (self.y - CAT_HALF_H) as i32 - cy,
        );
        renderer.draw_sprite_raw_rotated(
            LANDER_CAT.frames[0],
            LANDER_CAT.width,
            LANDER_CAT.height,
            pos,
            self.angle,
            SpriteOpts { transparent: true, ..Default::default() },
        );
    }

    fn draw_sparks(&self, renderer: &mut Renderer) {
        let (cx, cy) = self.cam();
        let hw = BURST1.width as i32 / 2;
        let hh = BURST1.height as i32 / 2;
        for s in &self.sparks {
            let frame = ((s.age / BURST1_FRAME_DUR) as usize).min(BURST1.frames.len() - 1);
            renderer.draw_sprite_raw(
                BURST1.frames[frame],
                BURST1.width,
                BURST1.height,
                Point::new(s.x as i32 - cx - hw, s.y as i32 - cy - hh),
                SpriteOpts { transparent: true, ..Default::default() },
            );
        }
    }

    fn draw_fuel(&self, renderer: &mut Renderer) {
        let h = FUEL_BAR_H as i32 - 1;
        let left_cap = FUEL_BAR_X - 2;
        let right_cap = FUEL_BAR_X + FUEL_BAR_W + 1;
        renderer.draw_line(Point::new(left_cap, 0), Point::new(left_cap, h));
        renderer.draw_line(Point::new(right_cap, 0), Point::new(right_cap, h));
        let w = (self.fuel / FUEL_MAX * FUEL_BAR_W as f32).ceil() as u32;
        if w > 0 {
            renderer.draw_rect(Point::new(FUEL_BAR_X, 0), Size::new(w, FUEL_BAR_H), true);
        }
    }

    /// Popup y for a result message, on whichever half of the screen the cat
    /// isn't in so the landing stays visible.
    fn result_popup_y(&self, height: u32) -> i32 {
        if self.y - self.cam_y < DISPLAY_H as f32 / 2.0 {
            DISPLAY_H - height as i32 - 4
        } else {
            4
        }
    }

    fn draw_popup(&self, renderer: &mut Renderer) {
        match self.state {
            State::Ready => {
                // Below the spawn point; the terrain starts well under it.
                let mut popup = Popup::new(10, 26, 108, 28);
                popup.set_text(t!("Left/Right: turn\nUp: thrust"), false, true);
                popup.draw(renderer, false);
            }
            State::Landed if self.result_timer >= RESULT_DELAY => {
                let mut n: String<8> = String::new();
                let _ = write!(n, "{}", self.last_score);
                let mut buf: String<32> = String::new();
                crate::i18n::substitute(&mut buf, t!("Lucky landing!\n+{n}"), &[("n", n.as_str())]);
                let mut popup = Popup::new(10, self.result_popup_y(28), 108, 28);
                popup.set_text(buf.as_str(), false, true);
                popup.draw(renderer, false);
            }
            State::Crashed if self.result_timer >= RESULT_DELAY => {
                let mut popup = Popup::new(10, self.result_popup_y(18), 108, 18);
                popup.set_text(t!("Not so lucky..."), false, true);
                popup.draw(renderer, false);
            }
            _ => {}
        }
    }

    fn apply_rewards(&self, ctx: &mut GameContext) {
        if self.session_score <= 0 {
            return;
        }
        // A typical x2 landing is ~150-200 points.
        let progress = (self.session_score as f32 / 500.0).sqrt();
        ctx.apply_stat_changes(&[
            (StatId::Focus,       5.0 * progress),
            (StatId::Playfulness, 3.0 * progress),
            (StatId::Fitness,     2.0 * progress),
            (StatId::Fulfillment, 3.0 * progress),
        ]);
        let coins = (4.0 * progress) as i32;
        if coins > 0 {
            ctx.coins += coins;
        }
    }
}

impl Scene for LuckyLandingScene {
    fn enter(&mut self, ctx: &mut GameContext) {
        let mut rng = ctx.rng;
        self.session_score = 0;
        self.reset_round(&mut rng);
        ctx.rng = rng;
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

        let mut rng = ctx.rng;
        match self.state {
            State::Ready => {
                if buttons.was_just_pressed(Button::A) || buttons.was_just_pressed(Button::Up) {
                    self.state = State::Playing;
                }
            }
            State::Playing => {
                self.update_flight(buttons, dt);
                self.update_camera();
            }
            State::Landed | State::Crashed => {
                self.result_timer += dt;
                if self.result_timer >= RESULT_DELAY && buttons.was_just_pressed(Button::A) {
                    self.reset_round(&mut rng);
                }
            }
        }
        self.update_sparks(&mut rng, dt);
        ctx.rng = rng;

        self.twinkle_timer += dt;
        if self.twinkle_timer > TWINKLE_PERIOD {
            self.twinkle_timer = 0.0;
            self.twinkle_phase = (self.twinkle_phase + 1) % TWINKLE_CYCLE_LENGTH;
        }
        None
    }

    fn draw(&self, _ctx: &GameContext, renderer: &mut Renderer, _dt_ms: u64) {
        self.draw_stars(renderer);
        self.draw_terrain(renderer);
        self.draw_sparks(renderer);
        self.draw_cat(renderer);
        self.draw_fuel(renderer);
        self.draw_popup(renderer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_are_flat_separate_and_in_bounds() {
        let mut rng = 0x1234_5678;
        for _ in 0..200 {
            let t = Terrain::generate(&mut rng);
            assert!(t.pads.len() >= MIN_PADS as usize);
            for h in t.heights {
                assert!((TERRAIN_MIN_Y..=TERRAIN_MAX_Y).contains(&(h as i32)));
            }
            let mut prev_end = 0;
            for p in &t.pads {
                assert!(p.i0 > prev_end && (p.i1 as usize) < TERRAIN_POINTS);
                assert!(t.heights[p.i0 as usize..=p.i1 as usize].iter().all(|&h| h == p.y));
                prev_end = p.i1;
            }
        }
    }
}
