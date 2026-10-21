use crate::{
    assets::character::PoseId,
    behavior::{Behavior, BehaviorId, BehaviorState, NextBehavior, TrainingKind},
    behaviors::common,
    context::{GameContext, StatId},
    entities::character::Character,
    rand,
};

const REJECTION_DURATION: f32 = 5.0;

const REJECTION_POSES: &[PoseId] = &[
    PoseId::StandingSideNeutralLookingDown,
    PoseId::SittingSideLookingDown,
    PoseId::LayingSideNeutral2,
    PoseId::LayingSideBored,
    PoseId::SittingSillySideNeutral,
    PoseId::StandingSideAnnoyed,
    PoseId::LayingSideAnnoyed,
    PoseId::LayingSideContent,
    PoseId::SittingLickingSideLickingLeg,
];

const BEGGING_POSES: &[PoseId] = &[
    PoseId::BeggingSideArmUp,
    PoseId::BeggingSideArmUp2,
    PoseId::BeggingSideDemanding,
];

fn rejection_chance(ctx: &GameContext) -> f32 {
    // Rejection thresholds: energy 30, focus 30, courage 25, sociability 25.
    let mut complement: f32 = 1.0;
    let checks = [
        (ctx.energy, 30.0_f32),
        (ctx.focus, 30.0),
        (ctx.courage, 25.0),
        (ctx.sociability, 25.0),
    ];
    for (val, threshold) in checks {
        if val < threshold {
            let deficit = (threshold - val) / threshold;
            complement *= 1.0 - deficit;
        }
    }
    1.0 - complement
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    WarmingUp,
    Training,
    CoolingDown,
    Rejecting,
}

pub struct TrainingBehavior {
    kind: TrainingKind,
    phase: Phase,
    phase_timer: f32,
    warmup_duration: f32,
    train_duration: f32,
    cooldown_duration: f32,
    pose_id: PoseId,
    begging_pair: [PoseId; 2],
    begging_index: usize,
    pose_timer: f32,
    pose_duration: f32,
    rejected: bool,
}

impl TrainingBehavior {
    pub fn new(kind: TrainingKind) -> Self {
        Self {
            kind,
            phase: Phase::WarmingUp,
            phase_timer: 0.0,
            warmup_duration: 3.0,
            train_duration: 20.0,
            cooldown_duration: 3.0,
            pose_id: PoseId::StandingSideNeutral,
            begging_pair: [PoseId::BeggingSideArmUp, PoseId::BeggingSideArmUp2],
            begging_index: 0,
            pose_timer: 0.0,
            pose_duration: 2.0,
            rejected: false,
        }
    }
}

impl Behavior for TrainingBehavior {
    fn id(&self) -> BehaviorId {
        BehaviorId::Training
    }
    fn progress(&self) -> f32 {
        match self.phase {
            Phase::WarmingUp => 0.0,
            Phase::Training => (self.phase_timer / self.train_duration).clamp(0.0, 1.0),
            Phase::CoolingDown => 1.0,
            Phase::Rejecting => (self.phase_timer / REJECTION_DURATION).clamp(0.0, 1.0),
        }
    }
    fn pose(&self) -> PoseId {
        self.pose_id
    }

    fn enter(&mut self, ctx: &mut GameContext, _: &mut Character) {
        let p = rejection_chance(ctx);
        self.rejected = rand::rand_f32(&mut ctx.rng) < p;
        self.phase_timer = 0.0;
        self.warmup_duration = rand::rand_range_f32(&mut ctx.rng, 1.0, 5.0);
        self.train_duration = rand::rand_range_f32(&mut ctx.rng, 10.0, 30.0);
        self.cooldown_duration = rand::rand_range_f32(&mut ctx.rng, 1.0, 5.0);

        if self.rejected {
            self.phase = Phase::Rejecting;
            let i = rand::rand_range_u32(&mut ctx.rng, 0, (REJECTION_POSES.len() as u32) - 1)
                as usize;
            self.pose_id = REJECTION_POSES[i];
            return;
        }

        self.phase = Phase::WarmingUp;
        // Python: idx = randint(0, 2); offset = randint(1, 2);
        // pair = [BEGGING[idx], BEGGING[(idx + offset) % 3]]
        let i = rand::rand_range_u32(&mut ctx.rng, 0, 2) as usize;
        let offset = rand::rand_range_u32(&mut ctx.rng, 1, 2) as usize;
        let j = (i + offset) % 3;
        self.begging_pair = [BEGGING_POSES[i], BEGGING_POSES[j]];
        self.begging_index = 0;
        self.pose_timer = 0.0;
        self.pose_duration = rand::rand_range_f32(&mut ctx.rng, 1.5, 2.5);
        self.pose_id = PoseId::StandingSideNeutral;
    }

    fn update(
        &mut self,
        ctx: &mut GameContext,
        character: &mut Character,
        dt: f32,
    ) -> BehaviorState {
        self.phase_timer += dt;
        match self.phase {
            Phase::Rejecting if self.phase_timer >= REJECTION_DURATION => {
                return BehaviorState::Completed;
            }
            Phase::WarmingUp if self.phase_timer >= self.warmup_duration => {
                self.phase = Phase::Training;
                self.phase_timer = 0.0;
                self.begging_index = 0;
                self.pose_id = self.begging_pair[0];
            }
            Phase::Training => {
                self.pose_timer += dt;
                if self.pose_timer >= self.pose_duration {
                    self.begging_index = 1 - self.begging_index;
                    self.pose_timer = 0.0;
                    self.pose_duration = rand::rand_range_f32(&mut ctx.rng, 1.5, 2.5);
                    self.pose_id = self.begging_pair[self.begging_index];
                }
                if self.phase_timer >= self.train_duration {
                    self.phase = Phase::CoolingDown;
                    self.phase_timer = 0.0;
                    self.pose_id = PoseId::SittingSideLookingDown;
                    character.play_bursts(&mut ctx.rng, 5);
                }
            }
            Phase::CoolingDown if self.phase_timer >= self.cooldown_duration => {
                character.play_bursts(&mut ctx.rng, 5);
                return BehaviorState::Completed;
            }
            _ => {}
        }
        BehaviorState::Running
    }

    fn next(&self, ctx: &GameContext) -> Option<NextBehavior> {
        if self.rejected {
            return Some(NextBehavior::Meandering);
        }
        let mut rng = ctx.rng;
        if rand::rand_f32(&mut rng) < 0.5 {
            // Toy variant kept as Mouse until inventory exists.
            Some(NextBehavior::Playing(crate::behavior::PlayVariant::Mouse, false))
        } else {
            None
        }
    }

    fn apply_completion_bonus(&self, ctx: &mut GameContext, progress: f32) {
        if self.rejected {
            return;
        }
        // Stat tables per training_type.
        let mut bonus: heapless::Vec<(StatId, f32), 14> = heapless::Vec::new();
        match self.kind {
            TrainingKind::Intelligence => {
                common::bonus_add(&mut bonus, StatId::Energy, -3.0);
                common::bonus_add(&mut bonus, StatId::Focus, -1.0);
                common::bonus_add(&mut bonus, StatId::Playfulness, -4.0);
                common::bonus_add(&mut bonus, StatId::Intelligence, 4.0);
                common::bonus_add(&mut bonus, StatId::Fulfillment, 2.0);
                common::bonus_add(&mut bonus, StatId::Maturity, 1.0);
                common::bonus_add(&mut bonus, StatId::Courage, 1.0);
                common::bonus_add(&mut bonus, StatId::Sociability, 0.5);
                common::bonus_add(&mut bonus, StatId::Loyalty, 1.0);
                common::bonus_add(&mut bonus, StatId::Mischievousness, -1.0);
            }
            TrainingKind::Behavior => {
                common::bonus_add(&mut bonus, StatId::Energy, -3.5);
                common::bonus_add(&mut bonus, StatId::Focus, -2.0);
                common::bonus_add(&mut bonus, StatId::Playfulness, -5.0);
                common::bonus_add(&mut bonus, StatId::Loyalty, 3.5);
                common::bonus_add(&mut bonus, StatId::Courage, 2.5);
                common::bonus_add(&mut bonus, StatId::Maturity, 2.0);
                common::bonus_add(&mut bonus, StatId::Sociability, 1.5);
                common::bonus_add(&mut bonus, StatId::Fulfillment, 2.0);
                common::bonus_add(&mut bonus, StatId::Intelligence, 1.0);
                common::bonus_add(&mut bonus, StatId::Fitness, 0.5);
                common::bonus_add(&mut bonus, StatId::Mischievousness, -1.5);
            }
            TrainingKind::Fitness => {
                common::bonus_add(&mut bonus, StatId::Energy, -5.0);
                common::bonus_add(&mut bonus, StatId::Focus, -3.0);
                common::bonus_add(&mut bonus, StatId::Playfulness, -8.0);
                common::bonus_add(&mut bonus, StatId::Fitness, 5.0);
                common::bonus_add(&mut bonus, StatId::Courage, 2.0);
                common::bonus_add(&mut bonus, StatId::Fulfillment, 2.0);
                common::bonus_add(&mut bonus, StatId::Loyalty, 1.0);
                common::bonus_add(&mut bonus, StatId::Maturity, 0.5);
                common::bonus_add(&mut bonus, StatId::Intelligence, 0.5);
                common::bonus_add(&mut bonus, StatId::Sociability, 0.5);
                common::bonus_add(&mut bonus, StatId::Mischievousness, -0.5);
            }
            TrainingKind::Sociability => {
                common::bonus_add(&mut bonus, StatId::Energy, -2.0);
                common::bonus_add(&mut bonus, StatId::Focus, -1.0);
                common::bonus_add(&mut bonus, StatId::Playfulness, -3.0);
                common::bonus_add(&mut bonus, StatId::Sociability, 4.0);
                common::bonus_add(&mut bonus, StatId::Loyalty, 2.5);
                common::bonus_add(&mut bonus, StatId::Fulfillment, 3.0);
                common::bonus_add(&mut bonus, StatId::Courage, 1.5);
                common::bonus_add(&mut bonus, StatId::Intelligence, 1.0);
                common::bonus_add(&mut bonus, StatId::Maturity, 0.5);
                common::bonus_add(&mut bonus, StatId::Mischievousness, -0.5);
            }
        }
        for e in bonus.iter_mut() {
            e.1 *= progress;
        }
        ctx.apply_stat_changes(&bonus);
    }
}
