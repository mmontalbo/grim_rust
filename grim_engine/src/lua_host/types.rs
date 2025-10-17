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

pub(crate) const MANNY_INTRO_START_POS: Vec3 = Vec3 {
    x: 1.396_010_04,
    y: 1.486_529_95,
    z: 0.0,
};

pub(crate) const MANNY_INTRO_FINAL_POS: Vec3 = Vec3 {
    x: 1.327_370_05,
    y: 1.598_809_96,
    z: 0.0,
};

pub(crate) const MANNY_INTRO_FINAL_ROT: Vec3 = Vec3 {
    x: 0.0,
    y: 50.381_802,
    z: 0.0,
};

pub(crate) const MO_INTRO_SETUP_INDEX: i32 = 0;
