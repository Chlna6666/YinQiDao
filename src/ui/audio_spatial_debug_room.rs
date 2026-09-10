use yinqidao_audio_spatial::{
    EnvironmentSettings, ListenerPose, Vec3, debug_room_half_extents,
};

const ROOM_GRID_DIVISIONS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RoomSegmentKind {
    Edge,
    FloorGrid,
    CeilingGrid,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ProjectedRoomReference {
    corners: [[f32; 3]; 8],
}

impl ProjectedRoomReference {
    pub(super) fn from_fixed_world_room(
        listener: ListenerPose,
        environment: EnvironmentSettings,
    ) -> Self {
        let half = debug_room_half_extents(environment);
        let world_corners = [
            Vec3::new(-half.x, -half.y, -half.z),
            Vec3::new(half.x, -half.y, -half.z),
            Vec3::new(half.x, -half.y, half.z),
            Vec3::new(-half.x, -half.y, half.z),
            Vec3::new(-half.x, half.y, -half.z),
            Vec3::new(half.x, half.y, -half.z),
            Vec3::new(half.x, half.y, half.z),
            Vec3::new(-half.x, half.y, half.z),
        ];
        let (right, up, forward) = listener.basis();
        let mut corners = [[0.0_f32; 3]; 8];
        for (destination, world) in corners.iter_mut().zip(world_corners) {
            let relative = world - listener.position;
            *destination = [
                relative.dot(right),
                relative.dot(up),
                relative.dot(forward),
            ];
        }
        Self { corners }
    }

    pub(super) fn max_extent(self) -> f32 {
        self.corners
            .into_iter()
            .map(length3)
            .fold(0.0_f32, f32::max)
    }

    /// Visit the twelve room edges plus sparse floor/ceiling grids without allocating temporary
    /// geometry. All positions are already listener-local while retaining the fixed world-room
    /// orientation, so turning/moving the listener changes only this projection, not the room.
    pub(super) fn for_each_segment(
        self,
        mut visit: impl FnMut([f32; 3], [f32; 3], RoomSegmentKind),
    ) {
        const EDGES: [(usize, usize); 12] = [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];
        for (start, end) in EDGES {
            visit(self.corners[start], self.corners[end], RoomSegmentKind::Edge);
        }

        self.for_each_plane_grid([0, 1, 2, 3], RoomSegmentKind::FloorGrid, &mut visit);
        self.for_each_plane_grid([4, 5, 6, 7], RoomSegmentKind::CeilingGrid, &mut visit);
    }

    fn for_each_plane_grid(
        self,
        corners: [usize; 4],
        kind: RoomSegmentKind,
        visit: &mut impl FnMut([f32; 3], [f32; 3], RoomSegmentKind),
    ) {
        let [a, b, c, d] = corners.map(|index| self.corners[index]);
        for division in 1..ROOM_GRID_DIVISIONS {
            let t = division as f32 / ROOM_GRID_DIVISIONS as f32;
            // Lines parallel to the local room X and Z edges. `lerp3` operates after world-room
            // projection, which is affine for the rigid ListenerPose basis and therefore exact.
            visit(lerp3(a, d, t), lerp3(b, c, t), kind);
            visit(lerp3(a, b, t), lerp3(d, c, t), kind);
        }
    }
}

#[inline]
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

#[inline]
fn length3(value: [f32; 3]) -> f32 {
    (value[0] * value[0] + value[1] * value[1] + value[2] * value[2]).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment() -> EnvironmentSettings {
        EnvironmentSettings {
            mix: 0.10,
            room_size: 0.30,
            damping: 0.45,
        }
    }

    #[test]
    fn identity_listener_keeps_world_room_axes() {
        let room = ProjectedRoomReference::from_fixed_world_room(
            ListenerPose::identity(),
            environment(),
        );
        let half = debug_room_half_extents(environment());
        assert_eq!(room.corners[0], [-half.x, -half.y, -half.z]);
        assert_eq!(room.corners[6], [half.x, half.y, half.z]);
    }

    #[test]
    fn listener_translation_does_not_move_world_room_with_listener() {
        let listener = ListenerPose {
            position: Vec3::new(1.0, 0.5, -0.75),
            ..ListenerPose::identity()
        };
        let room = ProjectedRoomReference::from_fixed_world_room(listener, environment());
        let half = debug_room_half_extents(environment());
        assert_eq!(
            room.corners[0],
            [-half.x - 1.0, -half.y - 0.5, -half.z + 0.75]
        );
    }

    #[test]
    fn room_reference_has_edges_and_sparse_floor_ceiling_grids() {
        let room = ProjectedRoomReference::from_fixed_world_room(
            ListenerPose::identity(),
            environment(),
        );
        let mut edges = 0usize;
        let mut floor = 0usize;
        let mut ceiling = 0usize;
        room.for_each_segment(|_, _, kind| match kind {
            RoomSegmentKind::Edge => edges += 1,
            RoomSegmentKind::FloorGrid => floor += 1,
            RoomSegmentKind::CeilingGrid => ceiling += 1,
        });
        assert_eq!(edges, 12);
        assert_eq!(floor, (ROOM_GRID_DIVISIONS - 1) * 2);
        assert_eq!(ceiling, (ROOM_GRID_DIVISIONS - 1) * 2);
    }
}
