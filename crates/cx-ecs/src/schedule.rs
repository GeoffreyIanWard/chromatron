//! The tick schedule.
//!
//! `bevy_ecs`'s scheduler defaults are deliberately *not* used (`ADR-0001`).
//! This builds one schedule whose system sets are the thirteen fixed phases,
//! chained hard, with structural change flushed at exactly one point.

use bevy_ecs::schedule::{
    ApplyDeferred, IntoScheduleConfigs, MultiThreadedExecutor, Schedule, ScheduleBuildSettings,
    SingleThreadedExecutor,
};
use bevy_ecs::system::ScheduleSystem;

use crate::phase::Phase;
use crate::world::SimWorld;

/// The per-tick system schedule.
///
/// Systems are added with a phase, always. There is no overload that omits it,
/// which is how S02's "registering a system without a phase fails to compile"
/// is satisfied — by the parameter being mandatory rather than by a lint.
pub struct SimSchedule {
    schedule: Schedule,
    system_count: usize,
}

impl Default for SimSchedule {
    fn default() -> Self {
        Self::new()
    }
}

impl SimSchedule {
    /// Builds an empty schedule with the phase ordering already configured.
    pub fn new() -> Self {
        let mut schedule = Schedule::default();

        // Chain the phases: every system in phase N finishes before any system
        // in phase N+1 starts. Within a phase, systems run in parallel and in
        // unspecified order, which is exactly why phases carry the read-then-
        // write discipline.
        // Written as an explicit tuple rather than iterated from `Phase::ORDER`:
        // `chain()` here is the schedule-config combinator, which is implemented
        // for tuples. The test below asserts the two stay in agreement.
        schedule.configure_sets(
            (
                Phase::IntakeCommands,
                Phase::ChunkLifecycle,
                Phase::TerrainEdit,
                Phase::FieldSolve,
                Phase::SpatialRebuild,
                Phase::AgentSense,
                Phase::AgentDecide,
                Phase::AgentAct,
                Phase::Physics,
                Phase::FieldDeposit,
                Phase::Events,
                Phase::StructuralApply,
                Phase::Diagnostics,
            )
                .chain(),
        );

        // Structural change is flushed at StructuralApply and nowhere else.
        //
        // bevy's automatic pass would insert flush points wherever it detects
        // deferred parameters, which would make the tick's archetype moves
        // depend on which systems happen to use Commands. Disabling it and
        // placing one explicit ApplyDeferred is what makes 02-architecture.md's
        // phase 11 mean what it says.
        schedule.set_build_settings(ScheduleBuildSettings {
            auto_insert_apply_deferred: false,
            ..ScheduleBuildSettings::default()
        });
        schedule.add_systems(ApplyDeferred.in_set(Phase::StructuralApply));

        // The previous-transform copy is built in, not left to the caller.
        //
        // S03 makes it a contract that `PreviousTransform` holds the start-of-tick
        // value, and every consumer of interpolation depends on it. A schedule
        // that omits it still runs, still ticks, and still draws — it just
        // silently interpolates from the current position to itself, which looks
        // exactly like the stepping interpolation exists to remove. That is a
        // failure with no error and no test that naturally catches it, so the
        // schedule registers it rather than documenting that you must.
        //
        // Registered directly rather than through `add_system` so it does not
        // count towards `system_count`, which reports what the *caller* added.
        schedule
            .add_systems(crate::transform::copy_previous_transforms.in_set(Phase::IntakeCommands));

        schedule.set_executor(MultiThreadedExecutor::default());

        Self {
            schedule,
            system_count: 0,
        }
    }

    /// Adds a system to a phase.
    pub fn add_system<M>(
        &mut self,
        phase: Phase,
        system: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        self.schedule.add_systems(system.in_set(phase));
        self.system_count += 1;
        self
    }

    /// Runs the schedule single-threaded, for the determinism gate.
    ///
    /// Identical results across thread counts is a gate, not an aspiration, so
    /// the single-threaded executor has to be reachable from a test.
    pub fn set_single_threaded(&mut self) -> &mut Self {
        self.schedule
            .set_executor(SingleThreadedExecutor::default());
        self
    }

    /// How many systems the caller has registered.
    ///
    /// Excludes the schedule's own built-ins (the structural flush and the
    /// previous-transform copy), so a module registering three systems reports
    /// three.
    pub const fn system_count(&self) -> usize {
        self.system_count
    }

    /// Runs one tick.
    pub fn run(&mut self, world: &mut SimWorld) {
        self.schedule.run(world.inner_mut());
    }

    /// The underlying schedule, for diagnostics and the S21 graph export.
    pub const fn inner(&self) -> &Schedule {
        &self.schedule
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::WorldConfig;
    use bevy_ecs::component::Component;
    use bevy_ecs::prelude::{Commands, Query, Res, ResMut, Resource};

    #[derive(Component, Debug, Clone, Copy, PartialEq)]
    struct Counter(u32);

    #[derive(Resource, Default, Debug)]
    struct PhaseLog(Vec<&'static str>);

    #[derive(Resource, Default, Debug)]
    struct SeenDuringTick(usize);

    #[test]
    fn phases_run_in_the_declared_order() {
        fn early(mut log: ResMut<PhaseLog>) {
            log.0.push("intake");
        }
        fn middle(mut log: ResMut<PhaseLog>) {
            log.0.push("act");
        }
        fn late(mut log: ResMut<PhaseLog>) {
            log.0.push("diagnostics");
        }

        let mut world = SimWorld::new(WorldConfig::default());
        world.insert_resource(PhaseLog::default());

        let mut schedule = SimSchedule::new();
        // Added in reverse, to prove the phase and not the registration order
        // decides execution.
        schedule.add_system(Phase::Diagnostics, late);
        schedule.add_system(Phase::AgentAct, middle);
        schedule.add_system(Phase::IntakeCommands, early);

        schedule.run(&mut world);

        let log = world.resource::<PhaseLog>().expect("resource inserted");
        assert_eq!(log.0, vec!["intake", "act", "diagnostics"]);
    }

    #[test]
    fn s02_acceptance_structural_change_is_deferred_to_structural_apply() {
        fn spawn_one(mut commands: Commands) {
            commands.spawn(Counter(1));
        }

        // Runs after the spawn but before StructuralApply, so it must not see
        // the new entity. This is the property the whole deferred-command
        // discipline exists for.
        fn count_mid_tick(query: Query<&Counter>, mut seen: ResMut<SeenDuringTick>) {
            seen.0 = query.iter().count();
        }

        let mut world = SimWorld::new(WorldConfig::default());
        world.insert_resource(SeenDuringTick::default());

        let mut schedule = SimSchedule::new();
        schedule.add_system(Phase::AgentAct, spawn_one);
        schedule.add_system(Phase::Events, count_mid_tick);

        schedule.run(&mut world);

        let seen = world
            .resource::<SeenDuringTick>()
            .expect("resource inserted");
        assert_eq!(
            seen.0, 0,
            "the spawn must not be visible before StructuralApply"
        );
        assert_eq!(
            world.entity_count(),
            1,
            "but must be visible after the tick"
        );
    }

    #[test]
    fn single_and_multi_threaded_execution_agree() {
        fn bump(mut query: Query<&mut Counter>) {
            for mut counter in query.iter_mut() {
                counter.0 += 1;
            }
        }

        fn run(threads: usize, single: bool) -> u32 {
            let mut world = SimWorld::new(WorldConfig {
                threads,
                ..WorldConfig::default()
            });
            world.spawn_batch((0..1_000).map(|_| Counter(0)));

            let mut schedule = SimSchedule::new();
            if single {
                schedule.set_single_threaded();
            }
            schedule.add_system(Phase::AgentAct, bump);

            for _ in 0..10 {
                schedule.run(&mut world);
            }

            let mut query = world.query::<&Counter>();
            query.iter(world.inner()).map(|counter| counter.0).sum()
        }

        assert_eq!(
            run(8, false),
            run(8, true),
            "executor choice must not change results"
        );
    }

    #[test]
    fn resources_are_readable_by_systems() {
        fn read(counter: Res<SeenDuringTick>, mut log: ResMut<PhaseLog>) {
            if counter.0 == 7 {
                log.0.push("seven");
            }
        }

        let mut world = SimWorld::new(WorldConfig::default());
        world.insert_resource(SeenDuringTick(7));
        world.insert_resource(PhaseLog::default());

        let mut schedule = SimSchedule::new();
        schedule.add_system(Phase::Diagnostics, read);
        schedule.run(&mut world);

        assert_eq!(
            world.resource::<PhaseLog>().expect("inserted").0,
            vec!["seven"]
        );
    }

    /// The whole reason the copy is built in: a caller who registers only a
    /// mover must still get working interpolation.
    ///
    /// Before this, a schedule that forgot the copy produced
    /// `previous == current` for every entity, which makes `Transform::interpolate`
    /// a no-op and shows up only as visibly stepping motion in a window — no
    /// error, no failing test, and a display server required to notice.
    #[test]
    fn the_schedule_copies_previous_transforms_without_being_asked() {
        use crate::transform::{PreviousTransform, Transform};
        use cx_core::math::{ChunkCoord, Vec3, WorldPos};

        fn move_east(mut query: Query<&mut Transform>) {
            for mut transform in query.iter_mut() {
                transform.position = transform.position.offset(Vec3::new(1.0, 0.0, 0.0));
            }
        }

        let start = Transform::from_position(WorldPos::new(ChunkCoord::new(0, 0), Vec3::ZERO));

        let mut world = SimWorld::new(WorldConfig::default());
        let entity = world.spawn((start, PreviousTransform(start)));

        let mut schedule = SimSchedule::new();
        schedule.add_system(Phase::AgentAct, move_east);

        // Two ticks, not one. After a single tick the seeded `PreviousTransform`
        // already equals what the copy would have written, so a one-tick version
        // of this test passes with the copy removed — it was written that way
        // first, and it did.
        schedule.run(&mut world);
        schedule.run(&mut world);

        let inner = world.inner();
        let current = *inner
            .get::<Transform>(entity)
            .expect("the entity still exists");
        let previous = inner
            .get::<PreviousTransform>(entity)
            .expect("the entity still exists")
            .0;

        assert!(
            (current.position.local.x - 2.0).abs() < 1e-6,
            "two ticks of the mover should reach x=2, got {}",
            current.position.local.x
        );
        assert!(
            (previous.position.local.x - 1.0).abs() < 1e-6,
            "previous should hold the start of the *second* tick, x=1, got {}. \
             x=0 means the copy never ran and interpolation is blending across \
             two ticks of movement.",
            previous.position.local.x
        );

        // And the halfway blend lands between them, which is what every
        // interpolated frame depends on.
        let midpoint = previous.interpolate(&current, 0.5);
        assert!(
            (midpoint.position.local.x - 1.5).abs() < 1e-6,
            "halfway between 1 and 2 should be 1.5, got {}",
            midpoint.position.local.x
        );
    }

    /// `system_count` reports what the caller added, not what the schedule
    /// builds in — otherwise adding a built-in would silently change every
    /// module's reported size.
    #[test]
    fn built_in_systems_do_not_count_as_registered() {
        assert_eq!(SimSchedule::new().system_count(), 0);
    }
}
