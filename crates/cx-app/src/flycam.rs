//! The free-fly camera (S03/M1).
//!
//! All of the arithmetic, none of the input plumbing. [`crate::window`] decides
//! that `W` means forward; this decides what forward *is*, where it lands, and
//! what happens when you look straight up. The second list is where the bugs
//! live, and none of it needs a display server.
//!
//! # The camera is view state, so it moves in real time
//!
//! `ADR-0004` bans wall-clock time from the simulation. This is not the
//! simulation: the camera lives below the firewall, is not hashed, and does not
//! affect a single tick. It therefore moves in *seconds*, not ticks — which is
//! also the only thing that feels right, since a camera pinned to 30 Hz would
//! stutter on a 120 Hz display exactly as the entities would without
//! interpolation.
//!
//! # Position is a `WorldPos`, not a `Vec3`
//!
//! The camera can fly for a long time. At 100 km from the origin an absolute
//! `f32` has around a centimetre of resolution and the view visibly jitters
//! (`03-conventions.md`), so the eye is stored chunk-relative like everything
//! else and rebased at the moment a [`Camera`] is produced. That also makes the
//! camera the natural source of the extract origin — see [`FlyCamera::origin`].

use cx_core::math::{ChunkCoord, Vec3, WorldPos};
use cx_render::Camera;

/// How far the pitch may go from level, in radians.
///
/// Just under a right angle on purpose. At exactly 90° the view direction is
/// parallel to the up vector, `look_at` has no way to choose a roll, and the
/// matrix comes out full of `NaN` — a black screen rather than an error.
const PITCH_LIMIT: f32 = std::f32::consts::FRAC_PI_2 - 0.001;

/// Default movement speed, in metres per second.
const DEFAULT_SPEED: f32 = 20.0;

/// What holding the boost key multiplies speed by.
const BOOST: f32 = 6.0;

/// Where the player wants to go, in camera-relative axes.
///
/// Each axis is an intent in `-1..=1`, not a velocity: how fast that turns into
/// metres is the camera's business, so a future gamepad stick can feed the same
/// struct as a key press.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MoveIntent {
    /// Positive is towards where the camera looks.
    pub forward: f32,
    /// Positive is to the camera's right.
    pub right: f32,
    /// Positive is towards world up, regardless of where the camera looks.
    pub up: f32,
    /// Whether the boost modifier is held.
    pub boost: bool,
}

impl MoveIntent {
    /// Whether this asks for any movement at all.
    pub fn is_still(&self) -> bool {
        self.forward == 0.0 && self.right == 0.0 && self.up == 0.0
    }
}

/// How far the player wants to turn, in radians.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LookIntent {
    /// Positive turns left.
    pub yaw: f32,
    /// Positive looks up.
    pub pitch: f32,
}

/// A camera that flies.
#[derive(Debug, Clone, Copy)]
pub struct FlyCamera {
    /// Eye position.
    pub position: WorldPos,
    /// Movement speed in metres per second, before any boost.
    pub speed: f32,
    yaw: f32,
    pitch: f32,
}

impl Default for FlyCamera {
    fn default() -> Self {
        Self {
            position: WorldPos::new(ChunkCoord::new(0, 0), Vec3::new(0.0, 10.0, 30.0)),
            speed: DEFAULT_SPEED,
            yaw: 0.0,
            pitch: 0.0,
        }
    }
}

impl FlyCamera {
    /// A camera at `position`, facing along `-Z` and level.
    pub fn new(position: WorldPos) -> Self {
        Self {
            position,
            ..Self::default()
        }
    }

    /// A camera at `from` already facing `target`.
    ///
    /// Derives yaw and pitch from the direction rather than storing a target, so
    /// the first look input turns from where the camera is actually pointing
    /// instead of snapping to level.
    pub fn looking_at(from: WorldPos, target: WorldPos) -> Self {
        let direction = target.delta(from);
        let mut camera = Self::new(from);

        if direction.length_squared() > 0.0 {
            let direction = direction.normalize();
            // atan2(-x, -z) rather than (x, z): yaw zero must mean facing -Z, so
            // that a default camera looks down the axis the rest of the engine
            // treats as forward.
            camera.yaw = (-direction.x).atan2(-direction.z);
            camera.pitch = direction
                .y
                .clamp(-1.0, 1.0)
                .asin()
                .clamp(-PITCH_LIMIT, PITCH_LIMIT);
        }

        camera
    }

    /// Current heading, in radians. Always within `0..TAU`.
    pub const fn yaw(&self) -> f32 {
        self.yaw
    }

    /// Current elevation, in radians. Always within ±[`PITCH_LIMIT`].
    pub const fn pitch(&self) -> f32 {
        self.pitch
    }

    /// Turns the camera.
    pub fn look(&mut self, intent: LookIntent) {
        // Wrapped rather than accumulated. A camera turned in one direction for
        // an hour would otherwise reach a yaw large enough that `f32` can no
        // longer resolve a small turn, and the view would start to ratchet.
        self.yaw = (self.yaw + intent.yaw).rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch + intent.pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Moves the camera for `seconds` of real time.
    pub fn advance(&mut self, intent: MoveIntent, seconds: f32) {
        if intent.is_still() || seconds <= 0.0 {
            return;
        }

        let forward = self.forward();
        // World up, not camera up: on a fly camera, "up" that tilted with the
        // view would make ascending while looking down move you forwards, which
        // is disorienting in exactly the moment you are trying to get your
        // bearings.
        let right = forward.cross(Vec3::Y).normalize_or_zero();

        let direction = forward * intent.forward + right * intent.right + Vec3::Y * intent.up;

        // Normalized so diagonal movement is not faster than straight movement —
        // the oldest bug in first-person controls.
        let direction = direction.normalize_or_zero();

        let speed = if intent.boost {
            self.speed * BOOST
        } else {
            self.speed
        };

        self.position = self.position.offset(direction * speed * seconds);
    }

    /// Unit vector the camera is facing.
    pub fn forward(&self) -> Vec3 {
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        Vec3::new(-sin_yaw * cos_pitch, sin_pitch, -cos_yaw * cos_pitch)
    }

    /// The chunk positions should be rebased against.
    ///
    /// The camera's own chunk, because that is the point in the world where
    /// precision matters most: everything near the eye ends up with a small
    /// local offset, and the error that remains is out at the horizon where a
    /// centimetre is invisible.
    pub const fn origin(&self) -> ChunkCoord {
        self.position.chunk
    }

    /// The renderer's camera, in extract space relative to `origin`.
    ///
    /// `origin` is a parameter rather than [`FlyCamera::origin`] because the
    /// extract that produced the frame may have used a different one — the
    /// camera moves every frame and the origin should not. Passing the origin
    /// the frame was actually extracted with is what keeps the two in the same
    /// space.
    pub fn camera(&self, origin: ChunkCoord) -> Camera {
        let eye = self.position.delta(WorldPos::new(origin, Vec3::ZERO));
        Camera::looking_at(eye, eye + self.forward())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32, z: f32) -> WorldPos {
        WorldPos::new(ChunkCoord::new(0, 0), Vec3::new(x, y, z))
    }

    const ORIGIN: ChunkCoord = ChunkCoord { x: 0, z: 0 };

    #[test]
    fn a_default_camera_faces_negative_z() {
        let camera = FlyCamera::default();
        let forward = camera.forward();
        assert!(
            (forward - Vec3::NEG_Z).length() < 1e-5,
            "yaw zero should face -Z, got {forward:?}"
        );
    }

    #[test]
    fn looking_at_a_target_actually_faces_it() {
        // The round trip that matters: a direction turned into yaw and pitch and
        // back must come out the same, or the camera snaps somewhere else the
        // first time it is built.
        for target in [
            at(10.0, 0.0, 0.0),
            at(-10.0, 0.0, 0.0),
            at(0.0, 0.0, -10.0),
            at(0.0, 5.0, -10.0),
            at(-7.0, -3.0, 4.0),
        ] {
            let camera = FlyCamera::looking_at(at(0.0, 0.0, 0.0), target);
            let wanted = target.delta(at(0.0, 0.0, 0.0)).normalize();
            let got = camera.forward();
            assert!(
                (wanted - got).length() < 1e-4,
                "looking at {target:?}: wanted {wanted:?}, faced {got:?}"
            );
        }
    }

    #[test]
    fn pitch_cannot_reach_straight_up() {
        // At exactly 90 degrees the view direction is parallel to up and the
        // look-at matrix degenerates to NaN — a black screen with no error.
        let mut camera = FlyCamera::default();
        for _ in 0..100 {
            camera.look(LookIntent {
                yaw: 0.0,
                pitch: 1.0,
            });
        }

        assert!(camera.pitch() < std::f32::consts::FRAC_PI_2);
        assert!(camera.forward().is_finite());

        let matrix = camera.camera(ORIGIN).view_projection(16.0 / 9.0);
        assert!(
            matrix.is_finite(),
            "a camera pitched fully up must still produce a usable matrix, got {matrix:?}"
        );

        for _ in 0..100 {
            camera.look(LookIntent {
                yaw: 0.0,
                pitch: -1.0,
            });
        }
        assert!(camera.pitch() > -std::f32::consts::FRAC_PI_2);
        assert!(camera.camera(ORIGIN).view_projection(1.0).is_finite());
    }

    #[test]
    fn yaw_wraps_instead_of_growing_without_bound() {
        // An unwrapped yaw eventually gets large enough that f32 cannot resolve
        // a small turn, and the camera starts to ratchet. This is only visible
        // after a very long session, which is the worst kind of bug to find.
        let mut camera = FlyCamera::default();
        for _ in 0..1_000 {
            camera.look(LookIntent {
                yaw: 1.0,
                pitch: 0.0,
            });
        }

        assert!(
            (0.0..std::f32::consts::TAU).contains(&camera.yaw()),
            "yaw should stay wrapped, got {}",
            camera.yaw()
        );

        // And a small turn still moves it, which is the property the wrap exists
        // to preserve.
        let before = camera.yaw();
        camera.look(LookIntent {
            yaw: 0.001,
            pitch: 0.0,
        });
        assert_ne!(before, camera.yaw(), "a small turn must still register");
    }

    #[test]
    fn moving_forward_goes_where_the_camera_looks() {
        let mut camera = FlyCamera::new(at(0.0, 0.0, 0.0));
        camera.speed = 10.0;
        camera.advance(
            MoveIntent {
                forward: 1.0,
                ..MoveIntent::default()
            },
            1.0,
        );

        let moved = camera.position.delta(at(0.0, 0.0, 0.0));
        assert!(
            (moved - Vec3::new(0.0, 0.0, -10.0)).length() < 1e-4,
            "one second at 10 m/s facing -Z should land 10 m along -Z, got {moved:?}"
        );
    }

    #[test]
    fn diagonal_movement_is_not_faster_than_straight() {
        // Unnormalized input makes forward+right 1.41x faster than forward, which
        // players discover before developers do.
        let mut straight = FlyCamera::new(at(0.0, 0.0, 0.0));
        let mut diagonal = FlyCamera::new(at(0.0, 0.0, 0.0));

        straight.advance(
            MoveIntent {
                forward: 1.0,
                ..MoveIntent::default()
            },
            1.0,
        );
        diagonal.advance(
            MoveIntent {
                forward: 1.0,
                right: 1.0,
                up: 1.0,
                boost: false,
            },
            1.0,
        );

        let straight_distance = straight.position.delta(at(0.0, 0.0, 0.0)).length();
        let diagonal_distance = diagonal.position.delta(at(0.0, 0.0, 0.0)).length();
        assert!(
            (straight_distance - diagonal_distance).abs() < 1e-3,
            "straight {straight_distance} vs diagonal {diagonal_distance}"
        );
    }

    #[test]
    fn up_is_world_up_even_when_looking_down() {
        // Camera-relative up would turn "ascend" into "move forwards" whenever
        // the camera is pitched, which is disorienting precisely when someone is
        // trying to recover their bearings.
        let mut camera = FlyCamera::new(at(0.0, 0.0, 0.0));
        camera.look(LookIntent {
            yaw: 0.0,
            pitch: -1.0,
        });
        camera.advance(
            MoveIntent {
                up: 1.0,
                ..MoveIntent::default()
            },
            1.0,
        );

        let moved = camera.position.delta(at(0.0, 0.0, 0.0)).normalize();
        assert!(
            (moved - Vec3::Y).length() < 1e-4,
            "ascending should go straight up regardless of pitch, got {moved:?}"
        );
    }

    #[test]
    fn boost_multiplies_speed_without_changing_direction() {
        let mut plain = FlyCamera::new(at(0.0, 0.0, 0.0));
        let mut boosted = FlyCamera::new(at(0.0, 0.0, 0.0));

        let intent = MoveIntent {
            forward: 1.0,
            ..MoveIntent::default()
        };
        plain.advance(intent, 1.0);
        boosted.advance(
            MoveIntent {
                boost: true,
                ..intent
            },
            1.0,
        );

        let plain_moved = plain.position.delta(at(0.0, 0.0, 0.0));
        let boosted_moved = boosted.position.delta(at(0.0, 0.0, 0.0));

        assert!((boosted_moved.length() / plain_moved.length() - BOOST).abs() < 1e-3);
        assert!((plain_moved.normalize() - boosted_moved.normalize()).length() < 1e-5);
    }

    #[test]
    fn standing_still_and_zero_time_move_nothing() {
        let start = at(3.0, 4.0, 5.0);
        let mut camera = FlyCamera::new(start);

        camera.advance(MoveIntent::default(), 1.0);
        assert_eq!(camera.position, start);

        camera.advance(
            MoveIntent {
                forward: 1.0,
                ..MoveIntent::default()
            },
            0.0,
        );
        assert_eq!(camera.position, start, "zero elapsed time is zero movement");
    }

    #[test]
    fn flying_far_keeps_the_origin_with_the_camera() {
        // The whole reason position is a WorldPos. After flying a long way the
        // camera's extract-space position must stay small, or the view jitters.
        let mut camera = FlyCamera::new(at(0.0, 0.0, 0.0));
        camera.speed = 1_000.0;
        camera.advance(
            MoveIntent {
                forward: 1.0,
                ..MoveIntent::default()
            },
            100.0,
        );

        assert_ne!(
            camera.origin(),
            ChunkCoord::new(0, 0),
            "100 km of flying should have moved the camera out of the origin chunk"
        );

        let rebased = camera.camera(camera.origin()).position;
        assert!(
            rebased.length() < 1_000.0,
            "rebased against its own chunk the eye should be near the origin, got {rebased:?}"
        );
    }

    #[test]
    fn rebasing_against_a_stale_origin_still_puts_the_eye_in_the_right_place() {
        // The frame's origin lags the camera by design, so the two must agree
        // about where the eye is even when they disagree about the origin.
        let camera = FlyCamera::new(WorldPos::new(
            ChunkCoord::new(3, -2),
            Vec3::new(5.0, 1.0, 7.0),
        ));

        let own = camera.camera(camera.origin()).position;
        let stale = camera.camera(ChunkCoord::new(0, 0)).position;

        let shift = WorldPos::new(camera.origin(), Vec3::ZERO)
            .delta(WorldPos::new(ChunkCoord::new(0, 0), Vec3::ZERO));
        assert!(
            (stale - (own + shift)).length() < 1e-3,
            "the two rebasings should differ by exactly the origin shift"
        );
    }
}
