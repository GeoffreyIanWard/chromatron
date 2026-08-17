//! S03 acceptance tests that need a real world and schedule.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use cx_core::Fixed;
use cx_ecs::{Phase, ResMut, Resource, SimSchedule, SimWorld, WorldConfig};
use cx_time::{HeadlessDriver, PacedDriver, StopReason, TickRate, TimeControl};

#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
struct Counter(u64);

fn bump(mut counter: ResMut<Counter>) {
    counter.0 += 1;
}

fn world_and_schedule() -> (SimWorld, SimSchedule) {
    let mut world = SimWorld::new(WorldConfig::default());
    world.insert_resource(Counter::default());

    let mut schedule = SimSchedule::new();
    schedule.add_system(Phase::AgentAct, bump);

    (world, schedule)
}

fn counter(world: &SimWorld) -> u64 {
    world.resource::<Counter>().expect("inserted").0
}

#[test]
fn s03_acceptance_a_headless_run_executes_exactly_the_declared_ticks() {
    let (mut world, mut schedule) = world_and_schedule();
    let mut driver = HeadlessDriver::default();

    let report = driver.run(&mut world, &mut schedule, 10_000);

    assert_eq!(report.ticks, 10_000);
    assert_eq!(report.reason, StopReason::Completed);
    assert_eq!(
        counter(&world),
        10_000,
        "every tick must have run the schedule"
    );
    assert_eq!(driver.clock().tick().0, 10_000);
}

#[test]
fn a_stop_condition_ends_the_run_early() {
    let (mut world, mut schedule) = world_and_schedule();
    let mut driver = HeadlessDriver::default();

    let report = driver.run_until(&mut world, &mut schedule, 1_000, |tick| tick.0 >= 42);

    assert_eq!(report.ticks, 42);
    assert_eq!(report.reason, StopReason::ConditionMet);
    assert_eq!(counter(&world), 42);
}

#[test]
fn s03_acceptance_pause_step_resume_matches_a_continuous_run() {
    // Continuous: five ticks in one go.
    let (mut world, mut schedule) = world_and_schedule();
    let mut driver = HeadlessDriver::default();
    driver.run(&mut world, &mut schedule, 5);
    let continuous = counter(&world);

    // Stepped: paused, then five single steps, then resumed.
    let (mut world, mut schedule) = world_and_schedule();
    let mut paced = PacedDriver::default();

    paced.set_control(TimeControl::Paused);
    paced.frame(&mut world, &mut schedule, Fixed::from_micros(33_333));
    assert_eq!(counter(&world), 0, "a paused frame must not tick");

    for _ in 0..5 {
        paced.set_control(TimeControl::Stepping { remaining: 1 });
        paced.frame(&mut world, &mut schedule, Fixed::ZERO);
    }

    assert_eq!(
        counter(&world),
        continuous,
        "stepping must reproduce a continuous run"
    );
    assert_eq!(
        paced.control(),
        TimeControl::Paused,
        "stepping should retire to paused"
    );
}

#[test]
fn s03_acceptance_both_drivers_produce_the_same_state() {
    // The property behind "identical state hashes under WindowedDriver and
    // HeadlessDriver": the same tick count means the same work, whatever fed it.
    let (mut headless_world, mut headless_schedule) = world_and_schedule();
    let mut headless = HeadlessDriver::new(TickRate::default());
    headless.run(&mut headless_world, &mut headless_schedule, 90);

    let (mut paced_world, mut paced_schedule) = world_and_schedule();
    let mut paced = PacedDriver::new(TickRate::default());
    // 90 ticks at 30 Hz is 3 s of real time, delivered in 16 ms frames.
    for _ in 0..188 {
        paced.frame(
            &mut paced_world,
            &mut paced_schedule,
            Fixed::from_micros(16_000),
        );
    }

    assert_eq!(
        counter(&headless_world),
        counter(&paced_world),
        "the two drivers disagreed on how much simulation happened"
    );
}

#[test]
fn a_stall_does_not_produce_a_burst_of_ticks() {
    let (mut world, mut schedule) = world_and_schedule();
    let mut paced = PacedDriver::default();

    // Two seconds of stall, as an injected breakpoint would produce.
    let produced = paced.frame(&mut world, &mut schedule, Fixed::from_micros(2_000_000));

    assert!(produced.ticks <= cx_time::MAX_CATCHUP);
    assert!(
        produced.fell_behind,
        "the diagnostic must fire rather than silently slowing"
    );
    assert_eq!(counter(&world), produced.ticks);
}

#[test]
fn time_acceleration_runs_more_ticks_per_frame() {
    let (mut world, mut schedule) = world_and_schedule();
    let mut paced = PacedDriver::default();
    paced.set_control(TimeControl::playing(4.0).expect("4x is supported"));

    paced.frame(&mut world, &mut schedule, Fixed::from_micros(33_333));

    assert_eq!(
        counter(&world),
        4,
        "4x should produce four ticks from one frame's time"
    );
}

#[test]
fn a_slower_tick_rate_runs_fewer_ticks_for_the_same_real_time() {
    let (mut world, mut schedule) = world_and_schedule();
    let mut paced = PacedDriver::new(TickRate::from_hz(10).expect("10 Hz is supported"));

    // One second of real time, in 100 ms frames so the clamp never binds.
    for _ in 0..10 {
        paced.frame(&mut world, &mut schedule, Fixed::from_micros(100_000));
    }

    assert_eq!(counter(&world), 10, "10 Hz means ten ticks per real second");
}
