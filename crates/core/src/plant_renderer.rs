//! Per-layer plant draw helper. Scenes call
//! `draw_plants(ctx, renderer, env, layer)` directly inside their `draw()`
//! after the corresponding environment layer has been drawn.

use embedded_graphics::prelude::Point;

use crate::{
    assets::plants::{plant_sprite, pot_sprite, PotKind},
    context::GameContext,
    environment::Environment,
    plant_system::{display_stage, PlantLayer},
    render::{Renderer, Sprite, SpriteOpts},
    scene::SceneId,
};

const DISPLAY_WIDTH: i32 = 128;

/// Draw all pots and plants for one scene + one layer.
pub fn draw_plants_layer(
    ctx: &GameContext,
    renderer: &mut Renderer,
    env: &Environment,
    scene: SceneId,
    layer: PlantLayer,
) {
    let offset = env.camera_offset(layer.to_env());
    for plant in ctx.plants.iter() {
        if plant.scene != scene || plant.layer != layer {
            continue;
        }
        let screen_x = plant.x - offset;
        if screen_x + 30 < 0 || screen_x > DISPLAY_WIDTH {
            continue;
        }

        let mut pot_h: i32 = 0;
        let mut pot_w: i32 = 0;
        if let Some(pot) = pot_sprite(plant.pot) {
            pot_h = pot.height as i32;
            pot_w = pot.width as i32;
            renderer.draw_sprite(
                pot,
                Point::new(screen_x, plant.y_snap - pot_h),
                SpriteOpts {
                    mirror_h: plant.mirror,
                    ..Default::default()
                },
            );
        }

        // Ground plants have no pot, fall back to centering on the cursor x
        // with a small reference width so the seedling sits where it was placed.
        let center_x = if plant.pot == PotKind::Ground {
            screen_x
        } else {
            screen_x + pot_w / 2
        };

        if let Some(seed) = plant.seed {
            if let Some(spr) = plant_sprite(seed, display_stage(plant)) {
                draw_plant_sprite(renderer, spr, center_x, plant.y_snap - pot_h, plant.mirror);
            }
        }
    }
}

fn draw_plant_sprite(
    renderer: &mut Renderer,
    sprite: &'static Sprite,
    center_x: i32,
    base_y: i32,
    mirror: bool,
) {
    let pos = Point::new(
        center_x - (sprite.width as i32) / 2,
        base_y - sprite.height as i32,
    );
    renderer.draw_sprite(
        sprite,
        pos,
        SpriteOpts {
            mirror_h: mirror,
            ..Default::default()
        },
    );
}
