/// Snapshot of everything the viewer needs to draw overlays on a single
/// background plate: entities from the timeline, optional movement/hotspot
/// fixtures, and camera data recovered from the set file.
pub struct ViewerScene {
    pub entities: Vec<SceneEntity>,
    pub position_bounds: Option<SceneBounds>,
    pub timeline: Option<TimelineSummary>,
    pub movement: Option<MovementTrace>,
    pub hotspot_events: Vec<HotspotEvent>,
    pub camera: Option<CameraParameters>,
    pub active_setup: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CameraParameters {
    pub name: String,
    pub position: [f32; 3],
    pub interest: [f32; 3],
    pub roll_degrees: f32,
    pub fov_degrees: f32,
    pub near_clip: f32,
    pub far_clip: f32,
}

impl CameraParameters {
    pub fn from_setup(name: &str, setup: &Setup) -> Option<Self> {
        let position = setup.position.as_ref()?;
        let interest = setup.interest.as_ref()?;
        let roll_degrees = setup.roll.unwrap_or(0.0);
        let fov_degrees = setup.fov?;
        let near_clip = setup.near_clip?;
        let far_clip = setup.far_clip?;

        Some(Self {
            name: name.to_string(),
            position: [position.x, position.y, position.z],
            interest: [interest.x, interest.y, interest.z],
            roll_degrees,
            fov_degrees,
            near_clip,
            far_clip,
        })
    }

    fn projector(&self, aspect_ratio: f32) -> Option<CameraProjector> {
        if !aspect_ratio.is_finite() || aspect_ratio <= 0.0 {
            return None;
        }

        let eye = Vec3::from_array(self.position);
        let target = Vec3::from_array(self.interest);
        let mut forward = target - eye;
        if forward.length_squared() <= f32::EPSILON {
            return None;
        }
        forward = forward.normalize();

        let mut up = Vec3::Z;
        let roll_radians = self.roll_degrees.to_radians();
        if roll_radians.abs() > f32::EPSILON {
            let rotation = Mat3::from_axis_angle(forward, roll_radians);
            up = rotation * up;
        }

        if up.length_squared() <= f32::EPSILON {
            up = Vec3::Y;
        }

        let view = Mat4::look_at_rh(eye, target, up.normalize());
        let projection = Mat4::perspective_rh(
            self.fov_degrees.to_radians(),
            aspect_ratio,
            self.near_clip.max(1e-4),
            self.far_clip.max(self.near_clip + 1.0),
        );

        Some(CameraProjector {
            view_projection: projection * view,
        })
    }
}

#[derive(Debug, Clone)]
pub struct CameraProjector {
    view_projection: Mat4,
}

impl CameraProjector {
    pub fn project(&self, position: [f32; 3]) -> Option<[f32; 2]> {
        let clip = self.view_projection * Vec4::new(position[0], position[1], position[2], 1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        if !ndc.x.is_finite() || !ndc.y.is_finite() {
            return None;
        }
        Some([ndc.x, ndc.y])
    }
}

impl ViewerScene {
    pub fn attach_movement_trace(&mut self, trace: MovementTrace) {
        if let Some(bounds) = self.position_bounds.as_mut() {
            bounds.include_bounds(&trace.bounds);
        } else {
            self.position_bounds = Some(trace.bounds.clone());
        }
        self.movement = Some(trace);
    }

    pub fn movement_trace(&self) -> Option<&MovementTrace> {
        self.movement.as_ref()
    }

    pub fn attach_hotspot_events(&mut self, events: Vec<HotspotEvent>) {
        self.hotspot_events = events;
    }

    pub fn hotspot_events(&self) -> &[HotspotEvent] {
        &self.hotspot_events
    }

    pub fn entity_position(&self, name: &str) -> Option<[f32; 3]> {
        self.entities
            .iter()
            .find(|entity| entity.name.eq_ignore_ascii_case(name))
            .and_then(|entity| entity.position)
    }

    pub fn camera_projector(&self, aspect_ratio: f32) -> Option<CameraProjector> {
        self.camera
            .as_ref()
            .and_then(|camera| camera.projector(aspect_ratio))
    }

    pub fn active_setup(&self) -> Option<&str> {
        self.active_setup.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SceneEntityKind {
    Actor,
    Object,
    InterestActor,
}

impl SceneEntityKind {
    pub fn label(self) -> &'static str {
        match self {
            SceneEntityKind::Actor => "Actor",
            SceneEntityKind::Object => "Object",
            SceneEntityKind::InterestActor => "Interest Actor",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SceneEntityKey {
    kind: SceneEntityKind,
    name: String,
}

impl SceneEntityKey {
    fn new(kind: SceneEntityKind, name: String) -> Self {
        Self { kind, name }
    }
}

#[derive(Debug)]
struct SceneEntityBuilder {
    key: SceneEntityKey,
    created_by: Option<String>,
    timeline_hook_index: Option<usize>,
    timeline_stage_index: Option<u32>,
    timeline_stage_label: Option<String>,
    timeline_hook_name: Option<String>,
    methods: BTreeSet<String>,
    position: Option<[f32; 3]>,
    rotation: Option<[f32; 3]>,
    facing_target: Option<String>,
    head_control: Option<String>,
    head_look_rate: Option<f32>,
    last_played: Option<String>,
    last_looping: Option<String>,
    last_completed: Option<String>,
}

impl SceneEntityBuilder {
    fn new(kind: SceneEntityKind, name: String) -> Self {
        Self {
            key: SceneEntityKey::new(kind, name),
            created_by: None,
            timeline_hook_index: None,
            timeline_stage_index: None,
            timeline_stage_label: None,
            timeline_hook_name: None,
            methods: BTreeSet::new(),
            position: None,
            rotation: None,
            facing_target: None,
            head_control: None,
            head_look_rate: None,
            last_played: None,
            last_looping: None,
            last_completed: None,
        }
    }

    fn apply_actor_snapshot(&mut self, value: &Value, hooks: &HookLookup) {
        if let Some(reference_value) = value.get("created_by") {
            if let Some(reference) = parse_hook_reference(reference_value) {
                if self.created_by.is_none() {
                    self.created_by = Some(format_hook_reference(&reference));
                }
                self.register_hook_reference(&reference, hooks);
            }
        }

        if let Some(methods) = value
            .get("method_totals")
            .and_then(|totals| totals.as_object())
        {
            for key in methods.keys() {
                self.methods.insert(key.clone());
            }
        }

        if let Some(transform) = value.get("transform") {
            if let Some(position) = transform.get("position") {
                self.position = parse_vec3_object(position);
            }
            if let Some(rotation) = transform.get("rotation") {
                self.rotation = parse_vec3_object(rotation);
            }
            if let Some(facing) = transform
                .get("facing_target")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.facing_target = Some(facing);
            }
            if let Some(control) = transform
                .get("head_control")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.head_control = Some(control);
            }
            if let Some(rate) = transform
                .get("head_look_rate")
                .and_then(|v| v.as_f64())
                .map(|value| value as f32)
            {
                self.head_look_rate = Some(rate);
            }
        }

        if let Some(chore) = value.get("chore_state") {
            if let Some(name) = chore
                .get("last_played")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.last_played = Some(name);
            }
            if let Some(name) = chore
                .get("last_looping")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.last_looping = Some(name);
            }
            if let Some(name) = chore
                .get("last_completed")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                self.last_completed = Some(name);
            }
        }
    }

    fn apply_event(
        &mut self,
        method: &str,
        args: &[String],
        trigger: Option<HookReference>,
        hooks: &HookLookup,
    ) {
        if let Some(reference) = trigger {
            if self.created_by.is_none() {
                self.created_by = Some(format_hook_reference(&reference));
            }
            self.register_hook_reference(&reference, hooks);
        }

        self.methods.insert(method.to_string());

        let lower = method.to_ascii_lowercase();
        match lower.as_str() {
            "setpos" | "set_pos" | "set_position" => {
                if let Some(vec) = parse_vec3_args(args) {
                    self.position = Some(vec);
                }
            }
            "setrot" | "set_rot" | "set_rotation" => {
                if let Some(vec) = parse_vec3_args(args) {
                    self.rotation = Some(vec);
                }
            }
            "set_face_target" | "set_facing" | "look_at" => {
                if let Some(target) = args.first() {
                    let trimmed = target.trim();
                    if !trimmed.is_empty() && trimmed != "<expr>" {
                        self.facing_target = Some(trimmed.to_string());
                    }
                }
            }
            "head_look_at" | "head_look_at_named" => {
                if let Some(target) = args.first() {
                    let trimmed = target.trim();
                    if !trimmed.is_empty() {
                        self.head_control = Some(format!("look_at {trimmed}"));
                    }
                }
            }
            "head_look_at_point" => {
                if args.len() >= 3 {
                    self.head_control = Some(format!(
                        "look_at_point ({}, {}, {})",
                        args[0], args[1], args[2]
                    ));
                }
            }
            "set_head" => {
                if args.is_empty() {
                    self.head_control = Some("set_head".to_string());
                } else {
                    self.head_control = Some(format!("set_head {}", args.join(", ")));
                }
            }
            "set_look_rate" => {
                if let Some(value) = args.first().and_then(|arg| arg.parse::<f32>().ok()) {
                    self.head_look_rate = Some(value);
                }
            }
            "enable_head_control" => {
                let state_label = args
                    .first()
                    .map(|value| format!("enable {value}"))
                    .unwrap_or_else(|| "enable".to_string());
                self.head_control = Some(state_label);
            }
            "disable_head_control" => {
                self.head_control = Some("disable".to_string());
            }
            "play_chore" => {
                if let Some(name) = args.first() {
                    self.last_played = Some(name.clone());
                }
            }
            "play_chore_looping" => {
                if let Some(name) = args.first() {
                    self.last_looping = Some(name.clone());
                    self.last_played = Some(name.clone());
                }
            }
            "complete_chore" => {
                if let Some(name) = args.first() {
                    self.last_completed = Some(name.clone());
                }
            }
            _ => {}
        }
    }

    fn build(self) -> SceneEntity {
        SceneEntity {
            kind: self.key.kind,
            name: self.key.name,
            created_by: self.created_by,
            timeline_hook_index: self.timeline_hook_index,
            timeline_stage_index: self.timeline_stage_index,
            timeline_stage_label: self.timeline_stage_label,
            timeline_hook_name: self.timeline_hook_name,
            methods: self.methods.into_iter().collect(),
            position: self.position,
            rotation: self.rotation,
            facing_target: self.facing_target,
            head_control: self.head_control,
            head_look_rate: self.head_look_rate,
            last_played: self.last_played,
            last_looping: self.last_looping,
            last_completed: self.last_completed,
        }
    }

    fn register_hook_reference(&mut self, reference: &HookReference, hooks: &HookLookup) {
        if self.timeline_hook_index.is_none() {
            self.timeline_hook_index = hooks.find(reference);
        }
        if self.timeline_stage_index.is_none() {
            self.timeline_stage_index = reference.stage_index;
        }
        if self.timeline_stage_label.is_none() {
            self.timeline_stage_label = reference.stage_label.clone();
        }
        if self.timeline_hook_name.is_none() {
            self.timeline_hook_name = Some(reference.name().to_string());
        }
    }
}

#[derive(Debug)]
pub struct SceneEntity {
    pub kind: SceneEntityKind,
    pub name: String,
    pub created_by: Option<String>,
    pub timeline_hook_index: Option<usize>,
    pub timeline_stage_index: Option<u32>,
    pub timeline_stage_label: Option<String>,
    pub timeline_hook_name: Option<String>,
    pub methods: Vec<String>,
    pub position: Option<[f32; 3]>,
    pub rotation: Option<[f32; 3]>,
    pub facing_target: Option<String>,
    pub head_control: Option<String>,
    pub head_look_rate: Option<f32>,
    pub last_played: Option<String>,
    pub last_looping: Option<String>,
    pub last_completed: Option<String>,
}

impl SceneEntity {
    pub fn describe(&self) -> String {
        let mut method_list = self.methods.clone();
        method_list.sort();
        let methods_label = if method_list.is_empty() {
            Cow::Borrowed("no recorded methods")
        } else {
            let preview_len = method_list.len().min(5);
            let mut label = method_list[..preview_len].join(", ");
            if method_list.len() > preview_len {
                label.push_str(&format!(", +{} more", method_list.len() - preview_len));
            }
            Cow::Owned(label)
        };

        let header = format!("[{}] {}", self.kind.label(), self.name);
        match &self.created_by {
            Some(source) => format!("{header} ({methods}) <= {source}", methods = methods_label),
            None => format!("{header} ({methods})", methods = methods_label),
        }
    }
}

fn prune_entities_for_set(
    entities: Vec<SceneEntity>,
    set_variable_name: Option<&str>,
    set_display_name: Option<&str>,
) -> Vec<SceneEntity> {
    if is_manny_office(set_variable_name, set_display_name) {
        return prune_manny_office_entities(entities, set_variable_name);
    }
    entities
}

fn is_manny_office(set_variable_name: Option<&str>, set_display_name: Option<&str>) -> bool {
    set_variable_name
        .map(|value| value.eq_ignore_ascii_case("mo"))
        .unwrap_or(false)
        || set_display_name
            .map(|value| value.eq_ignore_ascii_case("Manny's Office"))
            .unwrap_or(false)
}

fn manny_office_entity_names(set_prefix: &str) -> Vec<String> {
    let prefix = if set_prefix.is_empty() {
        "mo"
    } else {
        set_prefix
    };
    let mut names = vec!["manny".to_string()];
    for suffix in [
        "cards",
        "cards.interest_actor",
        "computer",
        "tube",
        "tube.interest_actor",
    ] {
        names.push(format!("{prefix}.{suffix}"));
    }
    names
}

fn prune_manny_office_entities(
    entities: Vec<SceneEntity>,
    set_variable_name: Option<&str>,
) -> Vec<SceneEntity> {
    let set_prefix = set_variable_name.unwrap_or("mo");
    let allowed = manny_office_entity_names(set_prefix);

    entities
        .into_iter()
        .filter(|entity| {
            allowed
                .iter()
                .any(|allowed| entity.name.eq_ignore_ascii_case(allowed))
        })
        .collect()
}

#[cfg(test)]
mod entity_filter_tests {
    use super::*;
    use std::collections::BTreeSet;

    fn make_entity(kind: SceneEntityKind, name: &str) -> SceneEntity {
        SceneEntityBuilder::new(kind, name.to_string()).build()
    }

    #[test]
    fn manny_office_allowlist_matches_trimmed_entities() {
        let expected = vec![
            "manny".to_string(),
            "mo.cards".to_string(),
            "mo.cards.interest_actor".to_string(),
            "mo.computer".to_string(),
            "mo.tube".to_string(),
            "mo.tube.interest_actor".to_string(),
        ];
        assert_eq!(manny_office_entity_names("mo"), expected);
        assert_eq!(manny_office_entity_names(""), expected);

        let mut with_custom_prefix = expected.clone();
        for name in with_custom_prefix.iter_mut().skip(1) {
            *name = name.replace("mo", "custom");
        }
        assert_eq!(
            manny_office_entity_names("custom"),
            with_custom_prefix,
            "allowlist should respect provided prefix"
        );
    }

    #[test]
    fn prune_entities_for_manny_office_keeps_core_entities() {
        let entities = vec![
            make_entity(SceneEntityKind::Actor, "Actor"),
            make_entity(SceneEntityKind::Actor, "meche"),
            make_entity(SceneEntityKind::Actor, "mo"),
            make_entity(SceneEntityKind::Object, "loading_menu"),
            make_entity(SceneEntityKind::Object, "manny"),
            make_entity(SceneEntityKind::Object, "mo.cards"),
            make_entity(SceneEntityKind::InterestActor, "mo.cards"),
            make_entity(SceneEntityKind::InterestActor, "mo.cards.interest_actor"),
            make_entity(SceneEntityKind::Object, "mo.computer"),
            make_entity(SceneEntityKind::Object, "mo.tube"),
            make_entity(SceneEntityKind::InterestActor, "mo.tube.interest_actor"),
            make_entity(SceneEntityKind::Object, "canister_actor"),
        ];

        let pruned = prune_entities_for_set(entities, Some("mo"), Some("Manny's Office"));
        let names: Vec<&str> = pruned.iter().map(|entity| entity.name.as_str()).collect();

        assert_eq!(
            names,
            vec![
                "manny",
                "mo.cards",
                "mo.cards",
                "mo.cards.interest_actor",
                "mo.computer",
                "mo.tube",
                "mo.tube.interest_actor",
            ]
        );

        let unique: BTreeSet<&str> = names.iter().copied().collect();
        let expected: BTreeSet<String> = manny_office_entity_names("mo").into_iter().collect();
        let expected_refs: BTreeSet<&str> = expected.iter().map(|s| s.as_str()).collect();
        assert_eq!(unique, expected_refs);
    }

    #[test]
    fn prune_entities_leaves_other_sets_untouched() {
        let entities = vec![
            make_entity(SceneEntityKind::Actor, "Actor"),
            make_entity(SceneEntityKind::Object, "gl.cards"),
            make_entity(SceneEntityKind::InterestActor, "gl.cards"),
        ];

        let pruned = prune_entities_for_set(entities, Some("gl"), Some("Glottis' Garage"));
        let names: Vec<&str> = pruned.iter().map(|entity| entity.name.as_str()).collect();

        assert_eq!(names, vec!["Actor", "gl.cards", "gl.cards"]);
    }
}

#[derive(Debug, Clone)]
pub struct SceneBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl SceneBounds {
    pub fn update(&mut self, position: [f32; 3]) {
        for axis in 0..3 {
            self.min[axis] = self.min[axis].min(position[axis]);
            self.max[axis] = self.max[axis].max(position[axis]);
        }
    }

    pub fn include_bounds(&mut self, other: &SceneBounds) {
        self.update(other.min);
        self.update(other.max);
    }

    pub fn top_down_axes(&self) -> (usize, usize) {
        let spans = [
            (self.max[0] - self.min[0]).abs(),
            (self.max[1] - self.min[1]).abs(),
            (self.max[2] - self.min[2]).abs(),
        ];

        let span_x = spans[0];
        let span_y = spans[1];
        let span_z = spans[2];
        const EPSILON: f32 = 1e-3;

        let has_x = span_x > EPSILON;
        let has_z = span_z > EPSILON;

        if has_x && has_z {
            if span_x >= span_z {
                return (0, 2);
            }
            return (2, 0);
        }

        if has_x {
            if span_z > EPSILON {
                return (0, 2);
            }
            if span_y > EPSILON {
                return (0, 1);
            }
            return (0, 2);
        }

        if has_z {
            if span_x > EPSILON {
                return (2, 0);
            }
            if span_y > EPSILON {
                return (2, 1);
            }
            return (2, 0);
        }

        self.projection_axes()
    }

    pub fn projection_axes(&self) -> (usize, usize) {
        let spans = [
            (self.max[0] - self.min[0]).abs(),
            (self.max[1] - self.min[1]).abs(),
            (self.max[2] - self.min[2]).abs(),
        ];

        let mut horizontal = 0usize;
        for axis in 1..3 {
            if spans[axis] > spans[horizontal] {
                horizontal = axis;
            }
        }

        let mut vertical = (horizontal + 1) % 3;
        for axis in 0..3 {
            if axis == horizontal {
                continue;
            }
            if spans[axis] > spans[vertical] || vertical == horizontal {
                vertical = axis;
            }
        }

        (horizontal, vertical)
    }
}

#[cfg(test)]
mod bounds_tests {
    use super::SceneBounds;

    #[test]
    fn projection_axes_prioritise_largest_spans() {
        let bounds = SceneBounds {
            min: [0.0, -2.0, 1.0],
            max: [3.0, 4.0, 1.5],
        };
        let (horizontal, vertical) = bounds.projection_axes();
        assert_eq!(horizontal, 1);
        assert_eq!(vertical, 0);
    }

    #[test]
    fn projection_axes_fall_back_when_axes_flat() {
        let bounds = SceneBounds {
            min: [1.0, 1.0, 1.0],
            max: [1.0, 2.5, 1.0],
        };
        let (horizontal, vertical) = bounds.projection_axes();
        assert_eq!(horizontal, 1);
        assert_ne!(vertical, horizontal);
    }

    #[test]
    fn top_down_axes_prefer_ground_plane() {
        let bounds = SceneBounds {
            min: [-12.0, -3.0, -1.5],
            max: [18.0, 7.0, 2.0],
        };
        let (horizontal, vertical) = bounds.top_down_axes();
        assert_eq!(horizontal, 0);
        assert_eq!(vertical, 2);
    }

    #[test]
    fn top_down_axes_fall_back_when_flat() {
        let bounds = SceneBounds {
            min: [0.0, -1.0, 0.0],
            max: [0.0, 3.0, 0.0],
        };
        let (horizontal, vertical) = bounds.top_down_axes();
        assert_ne!(horizontal, vertical);
    }
}

