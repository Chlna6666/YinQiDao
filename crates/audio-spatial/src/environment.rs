pub(crate) const EARLY_REFLECTION_TAP_COUNT: usize = 6;
pub(crate) const SPEED_OF_SOUND_M_S: f32 = 343.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvironmentSettings {
    pub mix: f32,
    pub room_size: f32,
    pub damping: f32,
}

impl Default for EnvironmentSettings {
    fn default() -> Self {
        Self {
            mix: 0.10,
            room_size: 0.30,
            damping: 0.45,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReflectionWall {
    Left,
    Right,
    Front,
    Rear,
    Floor,
    Ceiling,
}
