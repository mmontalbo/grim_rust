//! Helpers for converting engine-provided rotations into the viewer's axis basis.
//! The runtime records Euler angles as `{pitch, yaw, roll}` with yaw spinning
//! around the game's vertical (Z) axis. The viewer consumes those values both
//! for orienting decoded meshes and for aligning auxiliary visuals (e.g. axis
//! gizmos) that need a stable forward/up/right basis.

use glam::{Quat, Vec3};

/// Orientation basis derived from engine Euler rotations.
#[derive(Debug, Clone, Copy)]
pub struct EntityOrientation {
    /// Quaternion that rotates from local (+X right, +Y forward, +Z up) into world space.
    pub quaternion: Quat,
    /// Forward (+Y) axis expressed in world space.
    pub forward: [f32; 3],
    /// Right (+X) axis expressed in world space.
    pub right: [f32; 3],
    /// Up (+Z) axis expressed in world space.
    pub up: [f32; 3],
}

impl EntityOrientation {
    /// Build an orientation basis from `{pitch, yaw, roll}` degrees reported by the engine.
    pub fn from_degrees(rotation: [f32; 3]) -> Self {
        let quaternion = quat_from_engine_rotation(rotation);
        let forward = (quaternion * Vec3::Y).to_array();
        let right = (quaternion * Vec3::X).to_array();
        let up = (quaternion * Vec3::Z).to_array();

        Self {
            quaternion,
            forward,
            right,
            up,
        }
    }
}

/// Convert engine Euler angles (pitch, yaw, roll in degrees) into a quaternion that
/// respects the shared right-handed, Z-up basis used by the viewer.
pub fn quat_from_engine_rotation(rotation: [f32; 3]) -> Quat {
    let pitch = rotation[0].to_radians();
    let yaw = rotation[1].to_radians();
    let roll = rotation[2].to_radians();
    let yaw_z = Quat::from_rotation_z(yaw);
    let pitch_x = Quat::from_rotation_x(pitch);
    let roll_y = Quat::from_rotation_y(roll);
    yaw_z * pitch_x * roll_y
}

#[cfg(test)]
mod tests {
    use super::*;
    const EPSILON: f32 = 1e-6;

    fn approx_eq(a: [f32; 3], b: [f32; 3]) {
        for (lhs, rhs) in a.into_iter().zip(b) {
            assert!((lhs - rhs).abs() <= EPSILON, "{lhs} != {rhs}");
        }
    }

    #[test]
    fn yaw_spins_around_z_axis() {
        let orientation = EntityOrientation::from_degrees([0.0, 90.0, 0.0]);
        approx_eq(orientation.forward, [-1.0, 0.0, 0.0]);
        approx_eq(orientation.up, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn pitch_spins_around_x_axis() {
        let orientation = EntityOrientation::from_degrees([45.0, 0.0, 0.0]);
        approx_eq(orientation.forward, [0.0, 0.70710677, 0.70710677]);
    }

    #[test]
    fn roll_spins_around_y_axis() {
        let orientation = EntityOrientation::from_degrees([0.0, 0.0, 90.0]);
        approx_eq(orientation.right, [0.0, 0.0, -1.0]);
    }

    #[test]
    fn identity_rotation_preserves_axes() {
        let orientation = EntityOrientation::from_degrees([0.0, 0.0, 0.0]);
        approx_eq(orientation.forward, [0.0, 1.0, 0.0]);
        approx_eq(orientation.right, [1.0, 0.0, 0.0]);
        approx_eq(orientation.up, [0.0, 0.0, 1.0]);
    }
}
