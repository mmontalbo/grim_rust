#[derive(Debug, Copy, Clone)]
pub(crate) struct Vec3 {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) z: f32,
}

pub(crate) const MANNY_OFFICE_SEED_POS: Vec3 = Vec3 {
    x: 0.606_999_993,
    y: 2.040_999_89,
    z: 0.0,
};

pub(crate) const MANNY_OFFICE_SEED_ROT: Vec3 = Vec3 {
    x: 0.0,
    y: 222.210_007,
    z: 0.0,
};
