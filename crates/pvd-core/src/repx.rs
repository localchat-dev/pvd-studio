use crate::{Error, Object, SceneState, Value, error};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

mod aggregate;
mod articulation;
mod particle;
mod selection;

#[cfg(test)]
mod kinematic_tests;
#[cfg(test)]
mod shared_tests;

#[derive(Clone, Debug)]
pub enum Selection {
    Frame,
    /// Includes one Particle system or connected rigid Actors, complete Articulation trees, joints and Aggregates.
    Actor(u64),
    /// Captured identities in one frame; duplicates share the same dependency closure.
    Actors(Vec<ActorKey>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActorKey {
    pub id: u64,
    pub generation: u32,
}

pub const MAX_SELECTED_ACTORS: usize = 4096;

pub struct Export {
    pub xml: String,
    pub actors: usize,
    pub kinematic_actors: usize,
    pub shapes: usize,
    pub materials: usize,
    pub joints: usize,
    pub aggregates: usize,
    pub articulations: usize,
    pub particle_systems: usize,
}

const ACTOR_FIELDS: &[&str] = &[
    "Name",
    "ActorFlags",
    "DominanceGroup",
    "OwnerClient",
    "GlobalPose",
    "Shapes",
    "Scene",
    "ConcreteTypeName",
    "ClientBehaviorFlags",
    "Aggregate",
    "Joints",
];
const BODY_FIELDS: &[&str] = &[
    "CMassLocalPose",
    "Mass",
    "MassSpaceInertiaTensor",
    "LinearVelocity",
    "AngularVelocity",
    "MinCCDAdvanceCoefficient",
    "MaxDepenetrationVelocity",
    "MaxContactImpulse",
    "LinearDamping",
    "AngularDamping",
    "MaxAngularVelocity",
    "SleepThreshold",
    "StabilizationThreshold",
    "WakeCounter",
    "SolverIterationCounts.minPositionIters",
    "SolverIterationCounts.minVelocityIters",
    "ContactReportThreshold",
    "RigidBodyFlags",
    "RigidDynamicLockFlags",
    "InvMass",
    "MassSpaceInvInertiaTensor",
    "IsSleeping",
    "KinematicTarget",
];

pub fn export(state: &SceneState, selection: Selection) -> Result<Export, Error> {
    let physics = state
        .objects
        .values()
        .find(|o| kind(o) == "PxPhysics")
        .ok_or_else(|| error("RepX requires the captured PxPhysics version and tolerance scale"))?;
    if integer(physics, "Version.Major")? != 3
        || integer(physics, "Version.Minor")? != 4
        || integer(physics, "Version.Bugfix")? != 2
    {
        return Err(error(
            "RepX export is validated only for captured PhysX 3.4.2 objects",
        ));
    }
    let selection::Plan {
        actors,
        joints,
        aggregates,
        articulations,
        particles,
    } = selection::Plan::new(state, selection)?;
    if actors.is_empty() && particles.is_empty() {
        return Err(error("RepX export has no supported actors"));
    }
    let joint_ids: BTreeSet<u64> = joints.iter().map(|joint| joint.id).collect();
    let mut materials = BTreeMap::<u64, &Object>::new();
    let mut meshes = BTreeMap::<u64, String>::new();
    let mut mesh_budget = 128 * 1024 * 1024usize;
    let shape_plan = ShapePlan::new(state, physics, &actors)?;
    let mut actor_joints = BTreeMap::new();
    let mut shape_xml = BTreeMap::new();
    for (&id, shape) in &shape_plan.shapes {
        shape_xml.insert(
            id,
            write_shape(
                state,
                shape.object,
                shape.non_kinematic_dynamic,
                &mut materials,
                &mut meshes,
                &mut mesh_budget,
            )?,
        );
    }
    let mut actor_xml = String::new();
    let mut kinematic_actors = 0;
    let mut link_xml = BTreeMap::new();
    for actor in &actors {
        if kind(actor) == "PxArticulationLink" {
            actor_joints.insert(actor.id, validate_actor_joints(state, actor, &joint_ids)?);
            link_xml.insert(
                actor.id,
                articulation::write_link(state, actor, &shape_plan, &mut shape_xml)?,
            );
            continue;
        }
        let dynamic = match kind(actor) {
            "PxRigidDynamic" => true,
            "PxRigidStatic" => false,
            _ => {
                return Err(error(format!(
                    "{} is not a supported rigid actor",
                    actor.type_name
                )));
            }
        };
        validate_fields(actor, ACTOR_FIELDS, if dynamic { BODY_FIELDS } else { &[] })?;
        validate_actor(actor, dynamic)?;
        actor_joints.insert(actor.id, validate_actor_joints(state, actor, &joint_ids)?);
        writeln!(actor_xml, "<{}>", kind(actor)).unwrap();
        tag(&mut actor_xml, "Id", &actor.id.to_string())?;
        write_actor_common(&mut actor_xml, actor, &shape_plan, &mut shape_xml)?;
        if dynamic {
            let body_flags = integer(actor, "RigidBodyFlags")?;
            if body_flags & 5 == 5 {
                return Err(error("RepX kinematic actors cannot enable CCD"));
            }
            kinematic_actors += usize::from(body_flags & 1 != 0);
            if body_flags & 36 == 36 {
                return Err(error(
                    "RepX dynamic actors cannot enable CCD and speculative CCD together",
                ));
            }
            // PVD 目标使用 Body 坐标且没有有效标志；RepX 只保存 Actor 参数。
            if actor.properties.contains_key("KinematicTarget") {
                pose(actor, "KinematicTarget")?;
            }
            for field in &BODY_FIELDS[..14] {
                property(&mut actor_xml, actor, field)?;
            }
            flag_property(
                &mut actor_xml,
                actor,
                "RigidBodyFlags",
                &[
                    "eKINEMATIC",
                    "eUSE_KINEMATIC_TARGET_FOR_SCENE_QUERIES",
                    "eENABLE_CCD",
                    "eENABLE_CCD_FRICTION",
                    "eENABLE_POSE_INTEGRATION_PREVIEW",
                    "eENABLE_SPECULATIVE_CCD",
                    "eENABLE_CCD_MAX_CONTACT_IMPULSE",
                ],
            )?;
            flag_property(
                &mut actor_xml,
                actor,
                "RigidDynamicLockFlags",
                &[
                    "eLOCK_LINEAR_X",
                    "eLOCK_LINEAR_Y",
                    "eLOCK_LINEAR_Z",
                    "eLOCK_ANGULAR_X",
                    "eLOCK_ANGULAR_Y",
                    "eLOCK_ANGULAR_Z",
                ],
            )?;
            actor_xml.push_str("<SolverIterationCounts>\n");
            for field in ["minPositionIters", "minVelocityIters"] {
                tag(
                    &mut actor_xml,
                    field,
                    &value_text(value(actor, &format!("SolverIterationCounts.{field}"))?)?,
                )?;
            }
            actor_xml.push_str("</SolverIterationCounts>\n");
            property(&mut actor_xml, actor, "ContactReportThreshold")?;
        }
        writeln!(actor_xml, "</{}>", kind(actor)).unwrap();
    }
    let mut xml = String::from(
        "<PhysX30Collection version=\"3.4.2\">\n<UpVector>0 0 0</UpVector>\n<Scale>\n",
    );
    for field in ["Length", "Mass", "Speed"] {
        if real(physics, &format!("TolerancesScale.{field}"))? <= 0.0 {
            return Err(error("RepX requires positive SDK tolerance scales"));
        }
        tag(
            &mut xml,
            field,
            &value_text(value(physics, &format!("TolerancesScale.{field}"))?)?,
        )?;
    }
    xml.push_str("</Scale>\n");
    for material in materials.values() {
        for field in ["DynamicFriction", "StaticFriction"] {
            if real(material, field)? < 0.0 {
                return Err(error("RepX friction must be non-negative"));
            }
        }
        if !(0.0..=1.0).contains(&real(material, "Restitution")?) {
            return Err(error("RepX restitution must be between zero and one"));
        }
        validate_fields(
            material,
            &[
                "ConcreteTypeName",
                "DynamicFriction",
                "Flags",
                "FrictionCombineMode",
                "Physics",
                "ReferenceCount",
                "Restitution",
                "RestitutionCombineMode",
                "StaticFriction",
            ],
            &[],
        )?;
        xml.push_str("<PxMaterial>\n");
        tag(&mut xml, "Id", &material.id.to_string())?;
        for field in ["DynamicFriction", "StaticFriction", "Restitution"] {
            property(&mut xml, material, field)?;
        }
        flag_property(
            &mut xml,
            material,
            "Flags",
            &["eDISABLE_FRICTION", "eDISABLE_STRONG_FRICTION"],
        )?;
        for field in ["FrictionCombineMode", "RestitutionCombineMode"] {
            let mode = integer(material, field)? as usize;
            tag(
                &mut xml,
                field,
                ["eAVERAGE", "eMIN", "eMULTIPLY", "eMAX"]
                    .get(mode)
                    .ok_or_else(|| error("Unknown material combine mode"))?,
            )?;
        }
        xml.push_str("</PxMaterial>\n");
    }
    for mesh in meshes.values() {
        xml.push_str(mesh);
    }
    for (&id, shape) in &shape_plan.shapes {
        if !shape.exclusive {
            xml.push_str("<PxShape>\n");
            tag(&mut xml, "Id", &id.to_string())?;
            xml.push_str(&shape_xml.remove(&id).unwrap());
            xml.push_str("</PxShape>\n");
        }
    }
    xml.push_str(&actor_xml);
    for articulation in &articulations {
        articulation::write(&mut xml, state, articulation, &link_xml)?;
    }
    for object in &particles {
        particle::write(&mut xml, state, object, &mut mesh_budget)?;
    }
    for joint in &joints {
        write_joint(&mut xml, state, joint, &actor_joints)?;
    }
    for aggregate in &aggregates {
        aggregate::write(&mut xml, state, aggregate)?;
    }
    xml.push_str("</PhysX30Collection>\n");
    Ok(Export {
        xml,
        actors: actors.len(),
        kinematic_actors,
        shapes: shape_plan.shapes.len(),
        materials: materials.len(),
        joints: joints.len(),
        aggregates: aggregates.len(),
        articulations: articulations.len(),
        particle_systems: particles.len(),
    })
}

fn write_actor_common(
    xml: &mut String,
    actor: &Object,
    shape_plan: &ShapePlan<'_>,
    shape_xml: &mut BTreeMap<u64, String>,
) -> Result<(), Error> {
    property(xml, actor, "Name")?;
    flag_property(
        xml,
        actor,
        "ActorFlags",
        &[
            "eVISUALIZATION",
            "eDISABLE_GRAVITY",
            "eSEND_SLEEP_NOTIFIES",
            "eDISABLE_SIMULATION",
        ],
    )?;
    flag_property(
        xml,
        actor,
        "ClientBehaviorFlags",
        &[
            "eREPORT_TO_FOREIGN_CLIENTS_TRIGGER_NOTIFY",
            "eREPORT_TO_FOREIGN_CLIENTS_CONTACT_NOTIFY",
            "eREPORT_TO_FOREIGN_CLIENTS_CONSTRAINT_BREAK_NOTIFY",
            "eREPORT_TO_FOREIGN_CLIENTS_SCENE_QUERY",
        ],
    )?;
    for field in ["DominanceGroup", "OwnerClient", "GlobalPose"] {
        property(xml, actor, field)?;
    }
    xml.push_str("<Shapes>\n");
    for id in &shape_plan.actors[&actor.id] {
        let shape = &shape_plan.shapes[id];
        if shape.exclusive {
            xml.push_str("<PxShape>\n");
            xml.push_str(&shape_xml.remove(id).unwrap());
            xml.push_str("</PxShape>\n");
        } else {
            tag(xml, "PxShapeRef", &id.to_string())?;
        }
    }
    xml.push_str("</Shapes>\n");
    Ok(())
}

struct ShapeDependency<'a> {
    object: &'a Object,
    exclusive: bool,
    non_kinematic_dynamic: bool,
}

struct ShapePlan<'a> {
    shapes: BTreeMap<u64, ShapeDependency<'a>>,
    actors: BTreeMap<u64, Vec<u64>>,
}

impl<'a> ShapePlan<'a> {
    fn new(state: &'a SceneState, physics: &Object, actors: &[&Object]) -> Result<Self, Error> {
        let mut plan = Self {
            shapes: BTreeMap::new(),
            actors: BTreeMap::new(),
        };
        let mut shared_registrations = None;
        for actor in actors {
            let non_kinematic_dynamic = kind(actor) == "PxArticulationLink"
                || kind(actor) == "PxRigidDynamic" && integer(actor, "RigidBodyFlags")? & 1 == 0;
            let mut ids = Vec::new();
            let mut seen = BTreeSet::new();
            for shape in references(state, actor, "Shapes")? {
                if kind(shape) != "PxShape" || !seen.insert(shape.id) {
                    return Err(error(
                        "RepX Actor Shapes must contain distinct PxShape references",
                    ));
                }
                let exclusive = match value(shape, "IsExclusive")? {
                    Value::Bool(exclusive) => *exclusive,
                    _ => return Err(error("RepX Shape IsExclusive must be Boolean")),
                };
                if exclusive {
                    let owners = references(state, shape, "Actor")?;
                    if owners.len() != 1
                        || owners[0].id != actor.id
                        || plan.shapes.contains_key(&shape.id)
                    {
                        return Err(error("RepX exclusive Shape ownership is inconsistent"));
                    }
                } else {
                    // SDK PVD keeps the first attachment in Shape.Actor after shared owners change.
                    if !matches!(value(shape, "Actor")?, Value::Ref(_)) {
                        return Err(error("RepX shared Shape Actor hint must be a reference"));
                    }
                    if shared_registrations.is_none() {
                        let Value::Array(shared) = value(physics, "SharedShapes")? else {
                            return Err(error("RepX Physics SharedShapes must be an array"));
                        };
                        let mut registrations = BTreeMap::<u64, usize>::new();
                        for entry in shared.iter() {
                            let Value::Ref(id) = entry else {
                                return Err(error(
                                    "RepX Physics SharedShapes contains a non-reference",
                                ));
                            };
                            *registrations.entry(*id).or_default() += 1;
                        }
                        shared_registrations = Some(registrations);
                    }
                    if shared_registrations.as_ref().unwrap().get(&shape.id) != Some(&1)
                        || state
                            .resolve_reference(physics.id, "SharedShapes", shape.id)
                            .is_none_or(|registered| registered.generation != shape.generation)
                    {
                        return Err(error(
                            "RepX shared Shape requires a current Physics registration",
                        ));
                    }
                }
                plan.shapes
                    .entry(shape.id)
                    .and_modify(|entry| {
                        entry.non_kinematic_dynamic |= non_kinematic_dynamic;
                    })
                    .or_insert(ShapeDependency {
                        object: shape,
                        exclusive,
                        non_kinematic_dynamic,
                    });
                ids.push(shape.id);
            }
            plan.actors.insert(actor.id, ids);
        }
        Ok(plan)
    }
}

fn write_shape<'a>(
    state: &'a SceneState,
    shape: &'a Object,
    non_kinematic_dynamic: bool,
    materials: &mut BTreeMap<u64, &'a Object>,
    meshes: &mut BTreeMap<u64, String>,
    mesh_budget: &mut usize,
) -> Result<String, Error> {
    validate_fields(
        shape,
        &[
            "Actor",
            "ConcreteTypeName",
            "ContactOffset",
            "Flags",
            "Geometry",
            "GeometryType",
            "IsExclusive",
            "LocalPose",
            "Materials",
            "Name",
            "QueryFilterData",
            "RestOffset",
            "SimulationFilterData",
        ],
        &[],
    )?;
    validate_shape(shape)?;
    let mut xml = String::new();
    xml.push_str("<Geometry>\n");
    let geometry = references(state, shape, "Geometry")?;
    if geometry.len() != 1 {
        return Err(error("Shape must have exactly one geometry"));
    }
    let geometry = geometry[0];
    let owners = references(state, geometry, "Shape")?;
    if owners.len() != 1 || owners[0].id != shape.id {
        return Err(error("RepX Shape geometry ownership is inconsistent"));
    }
    let shape_materials = references(state, shape, "Materials")?;
    let fields: &[&str] = match kind(geometry) {
        "PxPlaneGeometry" => &[],
        "PxBoxGeometry" => &["HalfExtents"],
        "PxSphereGeometry" => &["Radius"],
        "PxCapsuleGeometry" => &["Radius", "HalfHeight"],
        "PxConvexMeshGeometry" => &[
            "Scale.Scale",
            "Scale.Rotation",
            "ConvexMesh",
            "MeshFlags",
            "MaxMargin",
        ],
        "PxTriangleMeshGeometry" => &["Scale.Scale", "Scale.Rotation", "TriangleMesh", "MeshFlags"],
        "PxHeightFieldGeometry" => &[
            "HeightField",
            "HeightScale",
            "RowScale",
            "ColumnScale",
            "HeightFieldFlags",
        ],
        name => {
            return Err(error(format!(
                "RepX geometry {name} is unsupported; captured meshes are not replaced with primitives"
            )));
        }
    };
    validate_fields(geometry, fields, &["Shape"])?;
    validate_geometry(geometry, shape, non_kinematic_dynamic)?;
    writeln!(xml, "<{}>", kind(geometry)).unwrap();
    if kind(geometry) == "PxHeightFieldGeometry" {
        write_height_field_geometry(
            &mut xml,
            state,
            geometry,
            shape_materials.len(),
            meshes,
            mesh_budget,
        )?;
    } else if matches!(
        kind(geometry),
        "PxConvexMeshGeometry" | "PxTriangleMeshGeometry"
    ) {
        write_mesh_geometry(
            &mut xml,
            state,
            geometry,
            shape_materials.len(),
            meshes,
            mesh_budget,
        )?;
    } else {
        for field in fields {
            property(&mut xml, geometry, field)?;
        }
    }
    writeln!(xml, "</{}>\n</Geometry>", kind(geometry)).unwrap();
    for field in ["LocalPose", "SimulationFilterData", "QueryFilterData"] {
        property(&mut xml, shape, field)?;
    }
    xml.push_str("<Materials>\n");
    if shape_materials.is_empty()
        || shape_materials.len() > u16::MAX as usize
        || (!matches!(
            kind(geometry),
            "PxTriangleMeshGeometry" | "PxHeightFieldGeometry"
        ) && shape_materials.len() != 1)
    {
        return Err(error(
            "RepX Shape has an invalid material count for its geometry",
        ));
    }
    for material in shape_materials {
        if kind(material) != "PxMaterial" {
            return Err(error("Shape material reference has an incompatible type"));
        }
        materials.insert(material.id, material);
        tag(&mut xml, "PxMaterialRef", &material.id.to_string())?;
    }
    xml.push_str("</Materials>\n");
    for field in ["ContactOffset", "RestOffset"] {
        property(&mut xml, shape, field)?;
    }
    flag_property(
        &mut xml,
        shape,
        "Flags",
        &[
            "eSIMULATION_SHAPE",
            "eSCENE_QUERY_SHAPE",
            "eTRIGGER_SHAPE",
            "eVISUALIZATION",
        ],
    )?;
    property(&mut xml, shape, "Name")?;
    Ok(xml)
}

fn validate_actor_joints(
    state: &SceneState,
    actor: &Object,
    joint_ids: &BTreeSet<u64>,
) -> Result<BTreeSet<u64>, Error> {
    if !actor.properties.contains_key("Joints") {
        return Ok(BTreeSet::new());
    }
    if !matches!(value(actor, "Joints")?, Value::Array(_)) {
        return Err(error("RepX Actor Joints must be a reference array"));
    }
    let mut seen = BTreeSet::new();
    for joint in references(state, actor, "Joints")? {
        if !joint_ids.contains(&joint.id) {
            return Err(error(format!(
                "RepX Actor 0x{:X} requires an unsupported or missing joint 0x{:X}",
                actor.id, joint.id
            )));
        }
        if !seen.insert(joint.id)
            || !["Actors.actor0", "Actors.actor1"].iter().any(|field| {
                matches!(joint.properties.get(*field), Some(Value::Ref(id)) if *id == actor.id)
            })
        {
            return Err(error("RepX Actor Joints relationships are inconsistent"));
        }
    }
    Ok(seen)
}

fn joint_actor<'a>(
    state: &'a SceneState,
    joint: &Object,
    name: &str,
) -> Result<Option<&'a Object>, Error> {
    let Value::Ref(id) = value(joint, name)? else {
        return Err(error(format!(
            "RepX joint {name} must be an Actor reference"
        )));
    };
    if *id == 0 {
        return Ok(None);
    }
    let actor = state
        .resolve_reference(joint.id, name, *id)
        .ok_or_else(|| {
            error(format!(
                "RepX refuses missing or stale joint reference 0x{:X}.{name} -> 0x{id:X}",
                joint.id
            ))
        })?;
    if !matches!(
        kind(actor),
        "PxRigidDynamic" | "PxRigidStatic" | "PxArticulationLink"
    ) {
        return Err(error("RepX joint requires rigid Actor dependencies"));
    }
    Ok(Some(actor))
}

const COMMON_JOINT_FIELDS: &[&str] = &[
    "Actors.actor0",
    "Actors.actor1",
    "LocalPose[eACTOR0]",
    "LocalPose[eACTOR1]",
    "BreakForce.force",
    "BreakForce.torque",
    "ConstraintFlags",
    "InvMassScale0",
    "InvInertiaScale0",
    "InvMassScale1",
    "InvInertiaScale1",
    "Name",
    "Parent",
    "RelativeTransform",
    "RelativeLinearVelocity",
    "RelativeAngularVelocity",
    "ConcreteTypeName",
];
const LIMIT_FIELDS: &[&str] = &[
    "Restitution",
    "BounceThreshold",
    "Stiffness",
    "Damping",
    "ContactDistance",
];
const D6_AXES: &[&str] = &["eX", "eY", "eZ", "eTWIST", "eSWING1", "eSWING2"];
const D6_DRIVES: &[&str] = &["eX", "eY", "eZ", "eSWING", "eTWIST", "eSLERP"];
const DISTANCE_FLAGS: &[(u64, &str)] = &[
    (2, "eMAX_DISTANCE_ENABLED"),
    (4, "eMIN_DISTANCE_ENABLED"),
    (8, "eSPRING_ENABLED"),
];
const REVOLUTE_FLAGS: &[(u64, &str)] = &[
    (1, "eLIMIT_ENABLED"),
    (2, "eDRIVE_ENABLED"),
    (4, "eDRIVE_FREESPIN"),
];
const CONSTRAINT_FLAGS: &[(u64, &str)] = &[
    (1 << 1, "ePROJECT_TO_ACTOR0"),
    (1 << 2, "ePROJECT_TO_ACTOR1"),
    (1 << 3, "eCOLLISION_ENABLED"),
    (1 << 4, "eVISUALIZATION"),
    (1 << 5, "eDRIVE_LIMITS_ARE_FORCES"),
    (1 << 7, "eIMPROVED_SLERP"),
    (1 << 8, "eDISABLE_PREPROCESSING"),
];

fn supported_joint(name: &str) -> bool {
    matches!(
        name,
        "PxSphericalJoint"
            | "PxFixedJoint"
            | "PxDistanceJoint"
            | "PxRevoluteJoint"
            | "PxPrismaticJoint"
            | "PxD6Joint"
    )
}

fn joint_extension_fields(name: &str) -> Vec<String> {
    let mut fields = Vec::<String>::new();
    let mut add = |names: &[&str]| fields.extend(names.iter().map(|name| (*name).to_owned()));
    if name != "PxDistanceJoint" {
        add(&["ProjectionLinearTolerance"]);
        if name != "PxSphericalJoint" {
            add(&["ProjectionAngularTolerance"]);
        }
    }
    match name {
        "PxSphericalJoint" => add(&["SphericalJointFlags"]),
        "PxDistanceJoint" => add(&[
            "Distance",
            "MinDistance",
            "MaxDistance",
            "Tolerance",
            "Stiffness",
            "Damping",
            "DistanceJointFlags",
        ]),
        "PxRevoluteJoint" => add(&[
            "Angle",
            "Velocity",
            "DriveVelocity",
            "DriveForceLimit",
            "DriveGearRatio",
            "RevoluteJointFlags",
        ]),
        "PxPrismaticJoint" => add(&["Position", "Velocity", "PrismaticJointFlags"]),
        "PxD6Joint" => add(&[
            "Twist",
            "SwingYAngle",
            "SwingZAngle",
            "DrivePosition",
            "DriveVelocity.linear",
            "DriveVelocity.angular",
        ]),
        _ => {}
    }
    let limits: &[(&str, &[&str])] = match name {
        "PxSphericalJoint" => &[("LimitCone", &["YAngle", "ZAngle"])],
        "PxRevoluteJoint" | "PxPrismaticJoint" => &[("Limit", &["Upper", "Lower"])],
        "PxD6Joint" => &[
            ("LinearLimit", &["Value"]),
            ("TwistLimit", &["Upper", "Lower"]),
            ("SwingLimit", &["YAngle", "ZAngle"]),
        ],
        _ => &[],
    };
    for (prefix, extra) in limits {
        fields.extend(
            LIMIT_FIELDS
                .iter()
                .chain(extra.iter())
                .map(|field| format!("{prefix}.{field}")),
        );
    }
    if name == "PxD6Joint" {
        fields.extend(D6_AXES.iter().map(|axis| format!("Motion[{axis}]")));
        for drive in D6_DRIVES {
            fields.extend(
                ["Stiffness", "Damping", "ForceLimit", "Flags"]
                    .map(|field| format!("Drive[{drive}].{field}")),
            );
        }
    }
    fields
}

fn write_joint(
    xml: &mut String,
    state: &SceneState,
    joint: &Object,
    actor_joints: &BTreeMap<u64, BTreeSet<u64>>,
) -> Result<(), Error> {
    let extension = joint_extension_fields(kind(joint));
    validate_fields(
        joint,
        COMMON_JOINT_FIELDS,
        &extension.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    validate_joint_properties(joint)?;
    let actors = [
        joint_actor(state, joint, "Actors.actor0")?,
        joint_actor(state, joint, "Actors.actor1")?,
    ];
    let ids = actors.map(|actor| actor.map_or(0, |actor| actor.id));
    if ids[0] == ids[1]
        || !actors
            .iter()
            .flatten()
            .any(|actor| matches!(kind(actor), "PxRigidDynamic" | "PxArticulationLink"))
    {
        return Err(error(
            "RepX joint requires distinct bodies and at least one dynamic Actor",
        ));
    }
    for actor in actors.iter().flatten() {
        if !actor_joints
            .get(&actor.id)
            .is_some_and(|joints| joints.contains(&joint.id))
        {
            return Err(error(
                "RepX joint is missing its Actor's matching Joints dependency",
            ));
        }
    }
    let parent = joint_actor(state, joint, "Parent")?;
    if parent.map(|actor| actor.id) != actors[0].or(actors[1]).map(|actor| actor.id) {
        return Err(error(
            "RepX joint Parent disagrees with its Actor dependencies",
        ));
    }
    xml.push_str(&format!("<{}>\n", kind(joint)));
    tag(xml, "Id", &joint.id.to_string())?;
    xml.push_str("<Actors>\n");
    tag(xml, "actor0", &ids[0].to_string())?;
    tag(xml, "actor1", &ids[1].to_string())?;
    xml.push_str("</Actors>\n<LocalPose>\n");
    for (name, property_name) in [
        ("eACTOR0", "LocalPose[eACTOR0]"),
        ("eACTOR1", "LocalPose[eACTOR1]"),
    ] {
        tag(xml, name, &value_text(value(joint, property_name)?)?)?;
    }
    xml.push_str("</LocalPose>\n<BreakForce>\n");
    for name in ["force", "torque"] {
        tag(
            xml,
            name,
            &value_text(value(joint, &format!("BreakForce.{name}"))?)?,
        )?;
    }
    xml.push_str("</BreakForce>\n");
    tag(
        xml,
        "ConstraintFlags",
        &joint_flags(joint, "ConstraintFlags", CONSTRAINT_FLAGS)?,
    )?;
    for name in [
        "InvMassScale0",
        "InvInertiaScale0",
        "InvMassScale1",
        "InvInertiaScale1",
        "Name",
    ] {
        property(xml, joint, name)?;
    }
    match kind(joint) {
        "PxSphericalJoint" => {
            write_joint_limit(xml, joint, "LimitCone", &["YAngle", "ZAngle"])?;
            tag(
                xml,
                "SphericalJointFlags",
                &joint_flags(joint, "SphericalJointFlags", &[(2, "eLIMIT_ENABLED")])?,
            )?;
        }
        "PxDistanceJoint" => {
            for field in [
                "MinDistance",
                "MaxDistance",
                "Tolerance",
                "Stiffness",
                "Damping",
            ] {
                property(xml, joint, field)?;
            }
            tag(
                xml,
                "DistanceJointFlags",
                &joint_flags(joint, "DistanceJointFlags", DISTANCE_FLAGS)?,
            )?;
        }
        "PxPrismaticJoint" | "PxRevoluteJoint" => {
            write_joint_limit(xml, joint, "Limit", &["Upper", "Lower"])?;
            if kind(joint) == "PxRevoluteJoint" {
                for field in ["DriveVelocity", "DriveForceLimit", "DriveGearRatio"] {
                    property(xml, joint, field)?;
                }
                tag(
                    xml,
                    "RevoluteJointFlags",
                    &joint_flags(joint, "RevoluteJointFlags", REVOLUTE_FLAGS)?,
                )?;
            } else {
                tag(
                    xml,
                    "PrismaticJointFlags",
                    &joint_flags(joint, "PrismaticJointFlags", &[(2, "eLIMIT_ENABLED")])?,
                )?;
            }
        }
        "PxD6Joint" => {
            xml.push_str("<Motion>\n");
            for axis in D6_AXES {
                let motion = integer(joint, &format!("Motion[{axis}]"))?;
                tag(xml, axis, ["eLOCKED", "eLIMITED", "eFREE"][motion as usize])?;
            }
            xml.push_str("</Motion>\n");
            write_joint_limit(xml, joint, "LinearLimit", &["Value"])?;
            write_joint_limit(xml, joint, "TwistLimit", &["Upper", "Lower"])?;
            write_joint_limit(xml, joint, "SwingLimit", &["YAngle", "ZAngle"])?;
            xml.push_str("<Drive>\n");
            for drive in D6_DRIVES {
                xml.push_str(&format!("<{drive}>\n"));
                for field in ["Stiffness", "Damping", "ForceLimit"] {
                    tag(
                        xml,
                        field,
                        &value_text(value(joint, &format!("Drive[{drive}].{field}"))?)?,
                    )?;
                }
                tag(
                    xml,
                    "Flags",
                    &joint_flags(
                        joint,
                        &format!("Drive[{drive}].Flags"),
                        &[(1, "eACCELERATION")],
                    )?,
                )?;
                xml.push_str(&format!("</{drive}>\n"));
            }
            xml.push_str("</Drive>\n");
            property(xml, joint, "DrivePosition")?;
            xml.push_str("<DriveVelocity>\n");
            for field in ["linear", "angular"] {
                tag(
                    xml,
                    field,
                    &value_text(value(joint, &format!("DriveVelocity.{field}"))?)?,
                )?;
            }
            xml.push_str("</DriveVelocity>\n");
        }
        "PxFixedJoint" => {}
        _ => return Err(error("RepX joint has no verified extension serializer")),
    }
    if kind(joint) != "PxDistanceJoint" {
        property(xml, joint, "ProjectionLinearTolerance")?;
        if kind(joint) != "PxSphericalJoint" {
            property(xml, joint, "ProjectionAngularTolerance")?;
        }
    }
    xml.push_str(&format!("</{}>\n", kind(joint)));
    Ok(())
}

fn write_joint_limit(
    xml: &mut String,
    joint: &Object,
    prefix: &str,
    extra: &[&str],
) -> Result<(), Error> {
    xml.push_str(&format!("<{prefix}>\n"));
    for field in LIMIT_FIELDS.iter().chain(extra.iter()) {
        tag(
            xml,
            field,
            &value_text(value(joint, &format!("{prefix}.{field}"))?)?,
        )?;
    }
    xml.push_str(&format!("</{prefix}>\n"));
    Ok(())
}

fn joint_flags(joint: &Object, field: &str, flags: &[(u64, &str)]) -> Result<String, Error> {
    let bits = integer(joint, field)?;
    let allowed = flags.iter().fold(0, |mask, (bit, _)| mask | bit);
    if bits & !allowed != 0 {
        return Err(error(format!(
            "RepX joint {field} contains unknown or unrestorable flag bits"
        )));
    }
    if bits == 0 {
        return Ok("0".into());
    }
    Ok(flags
        .iter()
        .filter_map(|(bit, name)| (bits & bit != 0).then_some(*name))
        .collect::<Vec<_>>()
        .join("|"))
}

fn non_negative(joint: &Object, field: &str) -> Result<(), Error> {
    if real(joint, field)? < 0.0 {
        return Err(error(format!("RepX joint {field} must be non-negative")));
    }
    Ok(())
}

fn validate_limit(joint: &Object, prefix: &str) -> Result<(), Error> {
    for field in &LIMIT_FIELDS[1..] {
        non_negative(joint, &format!("{prefix}.{field}"))?;
    }
    if !(0.0..=1.0).contains(&real(joint, &format!("{prefix}.Restitution"))?) {
        return Err(error("RepX joint restitution must be between zero and one"));
    }
    Ok(())
}

fn validate_cone(joint: &Object, prefix: &str) -> Result<(), Error> {
    validate_limit(joint, prefix)?;
    for field in ["YAngle", "ZAngle"] {
        let angle = real(joint, &format!("{prefix}.{field}"))?;
        if angle <= 0.0 || angle >= std::f32::consts::PI {
            return Err(error("RepX cone limit angles must be between zero and pi"));
        }
    }
    Ok(())
}

fn validate_pair(joint: &Object, prefix: &str, angular_bound: Option<f32>) -> Result<(), Error> {
    validate_limit(joint, prefix)?;
    let lower = real(joint, &format!("{prefix}.Lower"))?;
    let upper = real(joint, &format!("{prefix}.Upper"))?;
    let range = upper - lower;
    if !range.is_finite()
        || range < 0.0
        || real(joint, &format!("{prefix}.ContactDistance"))? > range
        || angular_bound.is_some_and(|bound| {
            lower <= -bound || upper >= bound || range >= std::f32::consts::TAU
        })
    {
        return Err(error("RepX joint limit pair violates SDK interval bounds"));
    }
    Ok(())
}

fn validate_joint_properties(joint: &Object) -> Result<(), Error> {
    if !matches!(value(joint, "Name")?, Value::String(_)) {
        return Err(error("RepX joint Name must be text"));
    }
    if !matches!(value(joint, "ConcreteTypeName")?, Value::String(name) if name == kind(joint)) {
        return Err(error("RepX joint concrete type is inconsistent"));
    }
    for name in [
        "LocalPose[eACTOR0]",
        "LocalPose[eACTOR1]",
        "RelativeTransform",
    ] {
        pose(joint, name)?;
    }
    for name in ["RelativeLinearVelocity", "RelativeAngularVelocity"] {
        vector(joint, name)?;
    }
    for name in [
        "BreakForce.force",
        "BreakForce.torque",
        "InvMassScale0",
        "InvInertiaScale0",
        "InvMassScale1",
        "InvInertiaScale1",
    ] {
        non_negative(joint, name)?;
    }
    joint_flags(joint, "ConstraintFlags", CONSTRAINT_FLAGS)?;
    if kind(joint) != "PxDistanceJoint" {
        non_negative(joint, "ProjectionLinearTolerance")?;
        if kind(joint) != "PxSphericalJoint"
            && !(0.0..=std::f32::consts::PI).contains(&real(joint, "ProjectionAngularTolerance")?)
        {
            return Err(error(
                "RepX joint angular projection tolerance must be between zero and pi",
            ));
        }
    }
    match kind(joint) {
        "PxSphericalJoint" => {
            validate_cone(joint, "LimitCone")?;
            joint_flags(joint, "SphericalJointFlags", &[(2, "eLIMIT_ENABLED")])?;
        }
        "PxFixedJoint" => {}
        "PxDistanceJoint" => {
            for field in [
                "Distance",
                "MinDistance",
                "MaxDistance",
                "Tolerance",
                "Stiffness",
                "Damping",
            ] {
                non_negative(joint, field)?;
            }
            joint_flags(joint, "DistanceJointFlags", DISTANCE_FLAGS)?;
            if real(joint, "MinDistance")? > real(joint, "MaxDistance")? {
                return Err(error("RepX distance joint minimum exceeds maximum"));
            }
        }
        "PxRevoluteJoint" | "PxPrismaticJoint" => {
            real(joint, "Velocity")?;
            if kind(joint) == "PxRevoluteJoint" {
                real(joint, "Angle")?;
                validate_pair(joint, "Limit", Some(std::f32::consts::PI))?;
                real(joint, "DriveVelocity")?;
                non_negative(joint, "DriveForceLimit")?;
                if real(joint, "DriveGearRatio")? <= 0.0 {
                    return Err(error("RepX revolute drive gear ratio must be positive"));
                }
                joint_flags(joint, "RevoluteJointFlags", REVOLUTE_FLAGS)?;
            } else {
                real(joint, "Position")?;
                validate_pair(joint, "Limit", None)?;
                joint_flags(joint, "PrismaticJointFlags", &[(2, "eLIMIT_ENABLED")])?;
            }
        }
        "PxD6Joint" => {
            for field in ["Twist", "SwingYAngle", "SwingZAngle"] {
                real(joint, field)?;
            }
            for axis in D6_AXES {
                if integer(joint, &format!("Motion[{axis}]"))? > 2 {
                    return Err(error("RepX D6 motion is unknown"));
                }
            }
            validate_limit(joint, "LinearLimit")?;
            if real(joint, "LinearLimit.Value")? <= 0.0 {
                return Err(error("RepX D6 linear limit must be positive"));
            }
            validate_pair(joint, "TwistLimit", Some(std::f32::consts::TAU))?;
            validate_cone(joint, "SwingLimit")?;
            for drive in D6_DRIVES {
                for field in ["Stiffness", "Damping", "ForceLimit"] {
                    non_negative(joint, &format!("Drive[{drive}].{field}"))?;
                }
                joint_flags(
                    joint,
                    &format!("Drive[{drive}].Flags"),
                    &[(1, "eACCELERATION")],
                )?;
            }
            pose(joint, "DrivePosition")?;
            vector(joint, "DriveVelocity.linear")?;
            vector(joint, "DriveVelocity.angular")?;
        }
        _ => return Err(error("RepX joint type is unsupported")),
    }
    Ok(())
}

fn kind(object: &Object) -> &str {
    object
        .type_name
        .rsplit("::")
        .next()
        .unwrap_or(&object.type_name)
}

fn write_mesh_geometry(
    xml: &mut String,
    state: &SceneState,
    geometry: &Object,
    material_count: usize,
    resources: &mut BTreeMap<u64, String>,
    budget: &mut usize,
) -> Result<(), Error> {
    let convex = kind(geometry) == "PxConvexMeshGeometry";
    let scale = vector(geometry, "Scale.Scale")?;
    if scale
        .iter()
        .any(|v| !(1e-6..=1e6).contains(&v.abs()) || (convex && *v <= 0.0))
    {
        return Err(error("RepX mesh scale violates SDK scale bounds"));
    }
    let Value::Quat(rotation) = value(geometry, "Scale.Rotation")? else {
        return Err(error("RepX mesh scale rotation must be a quaternion"));
    };
    if !rotation.iter().all(|v| v.is_finite())
        || (rotation.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() > 0.001
    {
        return Err(error(
            "RepX mesh scale rotation must be a finite unit quaternion",
        ));
    }
    let resource_field = if convex { "ConvexMesh" } else { "TriangleMesh" };
    let dependencies = references(state, geometry, resource_field)?;
    if dependencies.len() != 1 || kind(dependencies[0]) != format!("Px{resource_field}") {
        return Err(error("RepX mesh geometry requires one compatible resource"));
    }
    let mesh = dependencies[0];
    if !convex {
        if let Some(indices) = mesh.properties.get("MaterialIndices") {
            let Value::Array(indices) = indices else {
                return Err(error("RepX mesh material indices require an array"));
            };
            if indices.len() != integer(mesh, "NbTriangles")? as usize
                || indices
                    .iter()
                    .any(|v| !matches!(v, Value::U64(index) if *index < material_count as u64))
            {
                return Err(error(
                    "RepX triangle material indices are incomplete or outside the Shape material table",
                ));
            }
        } else if material_count != 1 {
            return Err(error(
                "RepX multi-material triangle mesh lacks captured per-triangle indices",
            ));
        }
    }
    if let std::collections::btree_map::Entry::Vacant(entry) = resources.entry(mesh.id) {
        entry.insert(serialize_mesh(mesh, convex, budget)?);
    }
    xml.push_str("<Scale>\n");
    tag(xml, "Scale", &floats(&scale)?)?;
    tag(xml, "Rotation", &floats(rotation)?)?;
    xml.push_str("</Scale>\n");
    tag(xml, resource_field, &mesh.id.to_string())?;
    let flags = integer(geometry, "MeshFlags")?;
    let allowed = if convex { 1 } else { 2 };
    if flags & !allowed != 0 {
        return Err(error("RepX mesh geometry contains unknown flags"));
    }
    if flags != 0 {
        tag(
            xml,
            "MeshFlags",
            if convex {
                "eTIGHT_BOUNDS"
            } else {
                "eDOUBLE_SIDED"
            },
        )?;
    }
    if convex {
        if real(geometry, "MaxMargin")? < 0.0 {
            return Err(error("RepX convex maximum margin must be non-negative"));
        }
        property(xml, geometry, "MaxMargin")?;
    }
    Ok(())
}

fn write_height_field_geometry(
    xml: &mut String,
    state: &SceneState,
    geometry: &Object,
    material_count: usize,
    resources: &mut BTreeMap<u64, String>,
    budget: &mut usize,
) -> Result<(), Error> {
    let dependencies = references(state, geometry, "HeightField")?;
    if dependencies.len() != 1 || kind(dependencies[0]) != "PxHeightField" {
        return Err(error(
            "RepX height field geometry requires one compatible resource",
        ));
    }
    let field = dependencies[0];
    validate_fields(
        field,
        &[
            "NbRows",
            "NbColumns",
            "Format",
            "Samples",
            "Thickness",
            "ConvexEdgeThreshold",
            "Flags",
            "Physics",
        ],
        &[],
    )?;
    let rows = integer(field, "NbRows")?;
    let columns = integer(field, "NbColumns")?;
    let count = rows
        .checked_mul(columns)
        .ok_or_else(|| error("RepX height field dimensions overflow"))?;
    if rows < 2 || columns < 2 || count > 1_000_000 || integer(field, "Format")? != 1 {
        return Err(error(
            "RepX height field dimensions or sample format are unsupported",
        ));
    }
    let samples = mesh_array(field, "Samples")?;
    if samples.len() != count as usize {
        return Err(error(
            "RepX height field sample count differs from row/column dimensions",
        ));
    }
    let new_resource = !resources.contains_key(&field.id);
    if new_resource {
        *budget = budget
            .checked_sub(samples.len() * 16 + 1024)
            .ok_or_else(|| error("RepX mesh XML exceeds the 128 MiB export budget"))?;
    }
    let mut packed = Vec::with_capacity(if new_resource { samples.len() } else { 0 });
    for sample in samples {
        let Value::Array(values) = sample else {
            return Err(error("RepX height field sample requires three fields"));
        };
        if values.len() != 3 {
            return Err(error("RepX height field sample requires three fields"));
        }
        let Value::I64(height) = values[0] else {
            return Err(error("RepX height field sample height must be signed"));
        };
        let height = i16::try_from(height)
            .map_err(|_| error("RepX height field height exceeds signed 16-bit range"))?;
        let a = mesh_index(&values[1])?;
        let b = mesh_index(&values[2])?;
        if [a, b]
            .iter()
            .any(|index| *index > 255 || (*index & 127 != 127 && *index & 127 >= material_count))
        {
            return Err(error(
                "RepX height field material index is outside the Shape material table",
            ));
        }
        if new_resource {
            packed.push(u32::from(height as u16) | ((a as u32) << 16) | ((b as u32) << 24));
        }
    }
    if let std::collections::btree_map::Entry::Vacant(entry) = resources.entry(field.id) {
        if real(field, "ConvexEdgeThreshold")? < 0.0
            || real(field, "Thickness")?.abs() > f32::MAX * 0.25
        {
            return Err(error(
                "RepX height field thickness or convex edge threshold is invalid",
            ));
        }
        let mut resource = String::from("<PxHeightField>\n");
        tag(&mut resource, "Id", &field.id.to_string())?;
        property(&mut resource, field, "NbRows")?;
        property(&mut resource, field, "NbColumns")?;
        tag(&mut resource, "Format", "eS16_TM")?;
        property(&mut resource, field, "Thickness")?;
        property(&mut resource, field, "ConvexEdgeThreshold")?;
        flag_property(&mut resource, field, "Flags", &["eNO_BOUNDARY_EDGES"])?;
        resource.push_str("<samples>");
        for value in packed {
            write!(resource, " {value}").unwrap();
        }
        resource.push_str("</samples>\n");
        resource.push_str("</PxHeightField>\n");
        entry.insert(resource);
    }
    tag(xml, "HeightField", &field.id.to_string())?;
    for (name, minimum) in [
        ("HeightScale", 0.0001 / 65535.0),
        ("RowScale", 1e-8),
        ("ColumnScale", 1e-8),
    ] {
        if real(geometry, name)? < minimum {
            return Err(error("RepX height field scale violates SDK minimum scale"));
        }
        property(xml, geometry, name)?;
    }
    let flags = integer(geometry, "HeightFieldFlags")?;
    if flags & !2 != 0 {
        return Err(error("RepX height field geometry contains unknown flags"));
    }
    if flags != 0 {
        tag(xml, "HeightFieldFlags", "eDOUBLE_SIDED")?;
    }
    Ok(())
}

fn mesh_array<'a>(object: &'a Object, field: &str) -> Result<&'a [Value], Error> {
    match value(object, field)? {
        Value::Array(values) => Ok(values),
        _ => Err(error(format!("RepX mesh {field} must be an array"))),
    }
}

fn mesh_index(value: &Value) -> Result<usize, Error> {
    match value {
        Value::U64(index) if *index <= u32::MAX as u64 => Ok(*index as usize),
        _ => Err(error("RepX mesh index must be an unsigned 32-bit integer")),
    }
}

fn mesh_values<'a>(
    mesh: &'a Object,
    name: &str,
) -> Result<impl ExactSizeIterator<Item = std::borrow::Cow<'a, Value>> + Clone, Error> {
    value(mesh, name)?
        .array_items()
        .ok_or_else(|| error(format!("RepX mesh {name} must be an array")))
}

fn serialize_mesh(mesh: &Object, convex: bool, budget: &mut usize) -> Result<String, Error> {
    validate_fields(
        mesh,
        if convex {
            &[
                "Points",
                "HullPolygons",
                "PolygonIndexes",
                "Mass",
                "LocalInertia",
                "LocalCenterOfMass",
                "Physics",
            ]
        } else {
            &[
                "Points",
                "Triangles",
                "NbTriangles",
                "MaterialIndices",
                "Physics",
            ]
        },
        &[],
    )?;
    let points = mesh_values(mesh, "Points")?;
    if points.len() < if convex { 4 } else { 3 }
        || points.len() > if convex { 255 } else { 1_000_000 }
    {
        return Err(error("RepX mesh vertex count is outside supported bounds"));
    }
    let indices = if convex {
        None
    } else {
        Some(mesh_values(mesh, "Triangles")?)
    };
    if let Some(indices) = &indices
        && (indices.len() == 0
            || indices.len() > 3_000_000
            || !indices.len().is_multiple_of(3)
            || indices.len() / 3 != integer(mesh, "NbTriangles")? as usize)
    {
        return Err(error(
            "RepX triangle index count disagrees with captured triangle count",
        ));
    }
    let vertices = points
        .clone()
        .map(|point| match point.as_ref() {
            Value::Vec3(v) if v.iter().all(|v| v.is_finite()) => Ok(*v),
            _ => Err(error(
                "RepX mesh points must be finite three-dimensional vectors",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if convex {
        if real(mesh, "Mass")? <= 0.0 {
            return Err(error("RepX convex mass information must be positive"));
        }
        vector(mesh, "LocalCenterOfMass")?;
        let inertia = mesh_array(mesh, "LocalInertia")?;
        if inertia.len() != 9
            || inertia
                .iter()
                .any(|v| !matches!(v, Value::F32(v) if v.is_finite()))
        {
            return Err(error(
                "RepX convex inertia must contain nine finite scalars",
            ));
        }
        validate_convex_hull(mesh, &vertices)?;
    } else if let Some(mut values) = indices.clone() {
        while let Some(first) = values.next() {
            let triangle = [
                mesh_index(&first)?,
                mesh_index(&values.next().unwrap())?,
                mesh_index(&values.next().unwrap())?,
            ];
            if triangle.iter().any(|i| *i >= vertices.len())
                || triangle[0] == triangle[1]
                || triangle[0] == triangle[2]
                || triangle[1] == triangle[2]
            {
                return Err(error(
                    "RepX triangle contains an invalid or repeated vertex index",
                ));
            }
            if face_normal(
                vertices[triangle[0]],
                vertices[triangle[1]],
                vertices[triangle[2]],
            )
            .is_none()
            {
                return Err(error("RepX triangle vertices are collinear"));
            }
        }
    }
    // PVD supplies topology without the cooked BVH; portable triangle resources use the SDK BVH33 cooker.
    let tag_name = if convex {
        "PxConvexMesh"
    } else {
        "PxBVH33TriangleMesh"
    };
    let mut xml = MeshXml {
        text: String::new(),
        limit: *budget,
    };
    writeln!(xml, "<{tag_name}>\n<Id>{}</Id>", mesh.id).map_err(mesh_xml_error)?;
    numeric_mesh_property(&mut xml, if convex { "points" } else { "Points" }, points)?;
    if let Some(indices) = indices {
        numeric_mesh_property(&mut xml, "Triangles", indices)?;
        if mesh.properties.contains_key("MaterialIndices") {
            numeric_mesh_property(
                &mut xml,
                "materialIndices",
                mesh_values(mesh, "MaterialIndices")?,
            )?;
        }
    }
    writeln!(xml, "</{tag_name}>").map_err(mesh_xml_error)?;
    *budget -= xml.text.len();
    xml.text.shrink_to_fit();
    Ok(xml.text)
}

struct MeshXml {
    text: String,
    limit: usize,
}

fn mesh_xml_error(_: std::fmt::Error) -> Error {
    error("RepX mesh XML exceeds the 128 MiB export budget")
}

impl Write for MeshXml {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if text.len() > self.limit - self.text.len() {
            return Err(std::fmt::Error);
        }
        let needed = self.text.len() + text.len();
        if needed > self.text.capacity() {
            let capacity = self
                .text
                .capacity()
                .saturating_mul(2)
                .max(needed)
                .min(self.limit);
            self.text.reserve_exact(capacity - self.text.len());
        }
        self.text.push_str(text);
        Ok(())
    }
}

fn numeric_mesh_property<'a>(
    xml: &mut MeshXml,
    name: &str,
    values: impl Iterator<Item = std::borrow::Cow<'a, Value>>,
) -> Result<(), Error> {
    write!(xml, "<{name}>").map_err(mesh_xml_error)?;
    for value in values {
        match value.as_ref() {
            Value::Vec3(v) if v.iter().all(|v| v.is_finite()) => {
                write!(xml, " {} {} {}", v[0], v[1], v[2]).map_err(mesh_xml_error)?
            }
            Value::U64(v) if *v <= u32::MAX as u64 => {
                write!(xml, " {v}").map_err(mesh_xml_error)?
            }
            _ => return Err(error("RepX mesh numeric buffer contains an invalid value")),
        }
    }
    writeln!(xml, "</{name}>").map_err(mesh_xml_error)?;
    Ok(())
}

fn validate_convex_hull(mesh: &Object, vertices: &[[f32; 3]]) -> Result<(), Error> {
    let polygons = mesh_array(mesh, "HullPolygons")?;
    let indices = mesh_array(mesh, "PolygonIndexes")?;
    if !(4..=255).contains(&polygons.len()) || indices.len() > 255 * 255 {
        return Err(error(
            "RepX convex polygon count is outside supported bounds",
        ));
    }
    let mut edges = BTreeMap::<(usize, usize), usize>::new();
    let mut used = BTreeSet::new();
    for polygon in polygons {
        let Value::Array(fields) = polygon else {
            return Err(error("RepX convex polygon requires metadata"));
        };
        if fields.len() != 2 {
            return Err(error("RepX convex polygon metadata has unexpected fields"));
        }
        let count = mesh_index(&fields[0])?;
        let start = mesh_index(&fields[1])?;
        let polygon = indices
            .get(
                start
                    ..start
                        .checked_add(count)
                        .ok_or_else(|| error("RepX convex polygon range overflow"))?,
            )
            .ok_or_else(|| error("RepX convex polygon exceeds its index buffer"))?;
        if !(3..=255).contains(&count) {
            return Err(error("RepX convex polygon has an invalid vertex count"));
        }
        let polygon = polygon
            .iter()
            .map(mesh_index)
            .collect::<Result<Vec<_>, _>>()?;
        if polygon.iter().any(|i| *i >= vertices.len())
            || polygon.iter().copied().collect::<BTreeSet<_>>().len() != count
        {
            return Err(error(
                "RepX convex polygon contains invalid or repeated indices",
            ));
        }
        for i in 0..count {
            *edges
                .entry((polygon[i], polygon[(i + 1) % count]))
                .or_default() += 1;
            used.insert(polygon[i]);
        }
        let origin = vertices[polygon[0]].map(f64::from);
        let normal = (1..count - 1)
            .find_map(|i| {
                face_normal(
                    vertices[polygon[0]],
                    vertices[polygon[i]],
                    vertices[polygon[i + 1]],
                )
            })
            .ok_or_else(|| error("RepX convex polygon is degenerate"))?;
        let extent = vertices
            .iter()
            .flat_map(|v| {
                v.iter()
                    .enumerate()
                    .map(|(i, v)| (f64::from(*v) - origin[i]).abs())
            })
            .fold(0.0f64, f64::max);
        let tolerance = (extent * 1e-5).max(1e-9);
        let distance = |point: [f32; 3]| {
            point
                .iter()
                .enumerate()
                .map(|(i, v)| (f64::from(*v) - origin[i]) * normal[i])
                .sum::<f64>()
        };
        if polygon
            .iter()
            .any(|i| distance(vertices[*i]).abs() > tolerance)
        {
            return Err(error("RepX convex polygon is not planar"));
        }
        let positive = vertices.iter().any(|v| distance(*v) > tolerance);
        let negative = vertices.iter().any(|v| distance(*v) < -tolerance);
        if positive == negative {
            return Err(error("RepX convex polygons do not enclose a convex volume"));
        }
    }
    if used.len() != vertices.len()
        || edges
            .iter()
            .any(|(&(a, b), count)| *count != 1 || edges.get(&(b, a)) != Some(&1))
    {
        return Err(error(
            "RepX convex polygons must form a closed, consistently oriented hull",
        ));
    }
    Ok(())
}

fn face_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Option<[f64; 3]> {
    let ab: [f64; 3] = std::array::from_fn(|i| f64::from(b[i]) - f64::from(a[i]));
    let ac: [f64; 3] = std::array::from_fn(|i| f64::from(c[i]) - f64::from(a[i]));
    let normal = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    let length = normal.iter().map(|v| v * v).sum::<f64>().sqrt();
    (length > 0.0 && length.is_finite()).then(|| normal.map(|v| v / length))
}

fn real(object: &Object, name: &str) -> Result<f32, Error> {
    match value(object, name)? {
        Value::F32(number) if number.is_finite() => Ok(*number),
        _ => Err(error(format!(
            "RepX {}.{name} requires a finite PhysX scalar",
            object.type_name
        ))),
    }
}

fn vector(object: &Object, name: &str) -> Result<[f32; 3], Error> {
    match value(object, name)? {
        Value::Vec3(vector) if vector.iter().all(|value| value.is_finite()) => Ok(*vector),
        _ => Err(error(format!(
            "RepX {}.{name} requires a finite three-dimensional vector",
            object.type_name
        ))),
    }
}

fn pose(object: &Object, name: &str) -> Result<(), Error> {
    let Value::Transform { position, rotation } = value(object, name)? else {
        return Err(error(format!("RepX {name} requires a typed transform")));
    };
    let squared_norm: f32 = rotation.iter().map(|value| value * value).sum();
    if !position
        .iter()
        .chain(rotation)
        .all(|value| value.is_finite())
        || (squared_norm - 1.0).abs() > 0.001
    {
        return Err(error(format!(
            "RepX {name} requires finite coordinates and a normalized quaternion"
        )));
    }
    Ok(())
}

fn validate_actor(actor: &Object, dynamic: bool) -> Result<(), Error> {
    if !matches!(value(actor, "Name")?, Value::String(_)) {
        return Err(error("RepX Actor Name must be text"));
    }
    pose(actor, "GlobalPose")?;
    if integer(actor, "DominanceGroup")? > 31 || integer(actor, "OwnerClient")? > 255 {
        return Err(error("RepX Actor dominance/client value is out of range"));
    }
    if dynamic {
        pose(actor, "CMassLocalPose")?;
        for name in ["LinearVelocity", "AngularVelocity"] {
            vector(actor, name)?;
        }
        if vector(actor, "MassSpaceInertiaTensor")?
            .iter()
            .any(|value| *value < 0.0)
        {
            return Err(error("RepX inertia components must be non-negative"));
        }
        for name in [
            "Mass",
            "MaxDepenetrationVelocity",
            "MaxContactImpulse",
            "LinearDamping",
            "AngularDamping",
            "MaxAngularVelocity",
            "SleepThreshold",
            "StabilizationThreshold",
            "WakeCounter",
            "ContactReportThreshold",
        ] {
            if real(actor, name)? < 0.0 {
                return Err(error(format!("RepX {name} must be non-negative")));
            }
        }
        if !(0.0..=1.0).contains(&real(actor, "MinCCDAdvanceCoefficient")?) {
            return Err(error("RepX CCD coefficient must be between zero and one"));
        }
        let position_iterations = integer(actor, "SolverIterationCounts.minPositionIters")?;
        let velocity_iterations = integer(actor, "SolverIterationCounts.minVelocityIters")?;
        if !(1..=255).contains(&position_iterations) || velocity_iterations > 255 {
            return Err(error("RepX solver iteration count is out of range"));
        }
    }
    Ok(())
}

fn validate_shape(shape: &Object) -> Result<(), Error> {
    pose(shape, "LocalPose")?;
    if !matches!(value(shape, "Name")?, Value::String(_)) {
        return Err(error("RepX Shape Name must be text"));
    }
    let contact = real(shape, "ContactOffset")?;
    if contact <= 0.0 || real(shape, "RestOffset")? >= contact {
        return Err(error(
            "RepX Shape offsets violate PhysX contact/rest bounds",
        ));
    }
    let flags = integer(shape, "Flags")?;
    if flags & 5 == 5 {
        return Err(error(
            "A RepX Shape cannot be both a trigger and a simulation shape",
        ));
    }
    for name in ["SimulationFilterData", "QueryFilterData"] {
        let Value::Array(values) = value(shape, name)? else {
            return Err(error("RepX filter data must contain four u32 words"));
        };
        if values.len() != 4
            || values
                .iter()
                .any(|value| !matches!(value, Value::U64(number) if *number <= u32::MAX as u64))
        {
            return Err(error("RepX filter data must contain four u32 words"));
        }
    }
    Ok(())
}

fn validate_geometry(
    geometry: &Object,
    shape: &Object,
    non_kinematic_dynamic: bool,
) -> Result<(), Error> {
    let shape_flags = integer(shape, "Flags")?;
    if matches!(
        kind(geometry),
        "PxTriangleMeshGeometry" | "PxHeightFieldGeometry"
    ) && shape_flags & 4 != 0
    {
        return Err(error(
            "RepX triangle mesh and height field triggers are unsupported",
        ));
    }
    let expected = match kind(geometry) {
        "PxSphereGeometry" => {
            if real(geometry, "Radius")? <= 0.0 {
                return Err(error("RepX sphere radius must be positive"));
            }
            0
        }
        "PxPlaneGeometry" => {
            if non_kinematic_dynamic && shape_flags & 1 != 0 {
                return Err(error(
                    "RepX non-kinematic dynamic actors cannot simulate plane geometry",
                ));
            }
            1
        }
        "PxCapsuleGeometry" => {
            if real(geometry, "Radius")? <= 0.0 || real(geometry, "HalfHeight")? < 0.0 {
                return Err(error("RepX capsule dimensions are invalid"));
            }
            2
        }
        "PxBoxGeometry" => {
            if vector(geometry, "HalfExtents")?
                .iter()
                .any(|value| *value <= 0.0)
            {
                return Err(error("RepX box half extents must be positive"));
            }
            3
        }
        "PxConvexMeshGeometry" => 4,
        "PxTriangleMeshGeometry" => {
            if non_kinematic_dynamic && shape_flags & 1 != 0 {
                return Err(error(
                    "RepX non-kinematic dynamic actors cannot simulate triangle meshes",
                ));
            }
            5
        }
        "PxHeightFieldGeometry" => {
            if non_kinematic_dynamic && shape_flags & 1 != 0 {
                return Err(error(
                    "RepX non-kinematic dynamic actors cannot simulate height fields",
                ));
            }
            6
        }
        _ => return Err(error("RepX geometry lacks a validated serializer")),
    };
    if integer(shape, "GeometryType")? != expected {
        return Err(error(
            "RepX Shape geometry type disagrees with its resource",
        ));
    }
    Ok(())
}
fn value<'a>(object: &'a Object, name: &str) -> Result<&'a Value, Error> {
    object.properties.get(name).ok_or_else(|| {
        error(format!(
            "RepX requires {}.{} on 0x{:X}",
            object.type_name, name, object.id
        ))
    })
}
fn integer(object: &Object, name: &str) -> Result<u64, Error> {
    match value(object, name)? {
        Value::U64(value) => Ok(*value),
        Value::I64(value) if *value >= 0 => Ok(*value as u64),
        _ => Err(error(format!(
            "RepX property {name} must be an unsigned integer"
        ))),
    }
}
fn references<'a>(
    state: &'a SceneState,
    owner: &Object,
    name: &str,
) -> Result<Vec<&'a Object>, Error> {
    let values: Vec<&Value> = match value(owner, name)? {
        value @ Value::Ref(_) => vec![value],
        Value::Array(values) => values.iter().collect(),
        _ => return Err(error(format!("RepX {name} requires references"))),
    };
    values
        .into_iter()
        .map(|value| {
            let Value::Ref(id) = value else {
                return Err(error("RepX reference array contains a non-reference"));
            };
            state.resolve_reference(owner.id, name, *id).ok_or_else(|| {
                error(format!(
                    "RepX refuses missing or stale reference 0x{:X}.{name} -> 0x{id:X}",
                    owner.id
                ))
            })
        })
        .collect()
}
fn references_any(value: &Value, ids: &BTreeSet<u64>) -> bool {
    match value {
        Value::Ref(id) => ids.contains(id),
        Value::Array(values) => values.iter().any(|v| references_any(v, ids)),
        _ => false,
    }
}
fn validate_fields(object: &Object, fields: &[&str], extra: &[&str]) -> Result<(), Error> {
    for name in object.properties.keys() {
        if !fields.contains(&name.as_str()) && !extra.contains(&name.as_str()) {
            return Err(error(format!(
                "RepX property {}.{name} has no validated serializer",
                object.type_name
            )));
        }
    }
    Ok(())
}
fn property(xml: &mut String, object: &Object, name: &str) -> Result<(), Error> {
    tag(xml, name, &value_text(value(object, name)?)?)
}
fn value_text(value: &Value) -> Result<String, Error> {
    match value {
        Value::F32(number) if number.is_finite() => Ok(number.to_string()),
        Value::F64(number) if number.is_finite() => Ok(number.to_string()),
        Value::U64(number) => Ok(number.to_string()),
        Value::I64(number) => Ok(number.to_string()),
        Value::String(text) => Ok(text.clone()),
        Value::Vec3(values) => floats(values),
        Value::Quat(values) => floats(values),
        Value::Transform { position, rotation } => {
            Ok(format!("{} {}", floats(rotation)?, floats(position)?))
        }
        Value::Array(values) => values
            .iter()
            .map(value_text)
            .collect::<Result<Vec<_>, _>>()
            .map(|items| items.join(" ")),
        _ => Err(error(
            "RepX requires finite scalar, vector, transform or text values",
        )),
    }
}
fn floats(values: &[f32]) -> Result<String, Error> {
    if values.iter().any(|v| !v.is_finite()) {
        return Err(error("RepX rejects non-finite coordinates"));
    }
    Ok(values
        .iter()
        .map(f32::to_string)
        .collect::<Vec<_>>()
        .join(" "))
}
fn flag_property(
    xml: &mut String,
    object: &Object,
    name: &str,
    names: &[&str],
) -> Result<(), Error> {
    let bits = integer(object, name)?;
    if bits >> names.len() != 0 {
        return Err(error(format!(
            "RepX {name} has unknown flag bits 0x{bits:X}"
        )));
    }
    // 3.4.2 读取器跳过空文本；非空零值使构造时的默认标志可以清零。
    if bits == 0 {
        return tag(xml, name, "0");
    }
    let flags = names
        .iter()
        .enumerate()
        .filter_map(|(i, name)| (bits & (1 << i) != 0).then_some(*name))
        .collect::<Vec<_>>()
        .join("|");
    tag(xml, name, &flags)
}
fn tag(xml: &mut String, name: &str, text: &str) -> Result<(), Error> {
    if text.chars().any(|c| {
        ((c as u32) < 32 && !matches!(c, '\n' | '\r' | '\t'))
            || matches!(c, '\u{fffe}' | '\u{ffff}')
    }) {
        return Err(error(
            "RepX text contains an XML-incompatible control character",
        ));
    }
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;");
    writeln!(xml, "<{name}>{escaped}</{name}>").unwrap();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_mesh_xml_obeys_the_exact_byte_budget() {
        let values = Value::Array(vec![Value::U64(2), Value::U64(100)].into());
        let expected = "<Triangles> 2 100</Triangles>\n";
        for limit in [expected.len() - 1, expected.len()] {
            let mut xml = MeshXml {
                text: String::new(),
                limit,
            };
            let result =
                numeric_mesh_property(&mut xml, "Triangles", values.array_items().unwrap());
            assert!(xml.text.len() <= limit);
            assert_eq!(result.is_ok(), limit == expected.len());
            if result.is_ok() {
                assert_eq!(xml.text, expected);
            }
        }
    }

    fn spherical_property_fixture() -> Object {
        let mut joint = Object {
            id: 1,
            generation: 0,
            type_name: "physx3::PxSphericalJoint".into(),
            properties: BTreeMap::new(),
        };
        for name in [
            "BreakForce.force",
            "BreakForce.torque",
            "InvMassScale0",
            "InvInertiaScale0",
            "InvMassScale1",
            "InvInertiaScale1",
            "ProjectionLinearTolerance",
            "LimitCone.BounceThreshold",
            "LimitCone.Stiffness",
            "LimitCone.Damping",
            "LimitCone.ContactDistance",
            "LimitCone.Restitution",
            "LimitCone.YAngle",
            "LimitCone.ZAngle",
        ] {
            joint.properties.insert(name.into(), Value::F32(0.5));
        }
        for name in [
            "LocalPose[eACTOR0]",
            "LocalPose[eACTOR1]",
            "RelativeTransform",
        ] {
            joint.properties.insert(
                name.into(),
                Value::Transform {
                    position: [0.0; 3],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                },
            );
        }
        for name in ["RelativeLinearVelocity", "RelativeAngularVelocity"] {
            joint.properties.insert(name.into(), Value::Vec3([0.0; 3]));
        }
        joint
            .properties
            .insert("Name".into(), Value::String("sphere".into()));
        joint.properties.insert(
            "ConcreteTypeName".into(),
            Value::String("PxSphericalJoint".into()),
        );
        joint
            .properties
            .insert("ConstraintFlags".into(), Value::U64(0));
        joint
            .properties
            .insert("SphericalJointFlags".into(), Value::U64(0));
        joint
    }

    #[test]
    fn spherical_flag_gaps_and_sdk_limit_boundaries_are_enforced() {
        let mut joint = spherical_property_fixture();
        assert!(validate_joint_properties(&joint).is_ok());
        assert_eq!(
            joint_flags(&joint, "ConstraintFlags", CONSTRAINT_FLAGS).unwrap(),
            "0"
        );
        assert_eq!(
            joint_flags(&joint, "SphericalJointFlags", &[(2, "eLIMIT_ENABLED")]).unwrap(),
            "0"
        );
        for flags in [0, 2, 4, 6, 8, 16, 32, 128, 256, 446] {
            joint
                .properties
                .insert("ConstraintFlags".into(), Value::U64(flags));
            assert!(
                validate_joint_properties(&joint).is_ok(),
                "rejected legal flags {flags}"
            );
        }
        for flags in [1, 64, 512, u64::MAX] {
            joint
                .properties
                .insert("ConstraintFlags".into(), Value::U64(flags));
            assert!(
                validate_joint_properties(&joint).is_err(),
                "accepted unrestorable flags {flags}"
            );
        }
        joint
            .properties
            .insert("ConstraintFlags".into(), Value::U64(0));
        joint
            .properties
            .insert("SphericalJointFlags".into(), Value::U64(2));
        assert!(validate_joint_properties(&joint).is_ok());
        for flags in [1, 3, 4, u64::MAX] {
            joint
                .properties
                .insert("SphericalJointFlags".into(), Value::U64(flags));
            assert!(validate_joint_properties(&joint).is_err());
        }
        joint
            .properties
            .insert("SphericalJointFlags".into(), Value::U64(0));
        for (name, invalid) in [
            ("LimitCone.YAngle", 0.0),
            ("LimitCone.ZAngle", std::f32::consts::PI),
            ("LimitCone.Restitution", 1.0001),
            ("LimitCone.ContactDistance", -f32::EPSILON),
            ("LimitCone.Damping", f32::NAN),
            ("InvMassScale1", f32::INFINITY),
        ] {
            let previous = joint
                .properties
                .insert(name.into(), Value::F32(invalid))
                .unwrap();
            assert!(
                validate_joint_properties(&joint).is_err(),
                "accepted invalid {name}"
            );
            joint.properties.insert(name.into(), previous);
        }
    }

    #[test]
    fn xml_text_and_nonfinite_coordinates_have_explicit_boundaries() {
        let mut xml = String::new();
        tag(&mut xml, "Name", "A < B & \"C\"").unwrap();
        assert!(xml.contains("A &lt; B &amp; &quot;C&quot;"));
        assert!(tag(&mut xml, "Name", "bad\u{0}name").is_err());
        assert!(floats(&[f32::NAN]).is_err());
        assert!(floats(&[f32::INFINITY]).is_err());
    }

    #[test]
    #[ignore = "requires the official PhysX 3.4.2 baseline capture"]
    fn corrupt_real_capture_properties_are_rejected_before_export() {
        let path = std::env::var_os("PVD_SDK34_CAPTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../captures/physx-3.4.pvdstream")
            });
        let recording = crate::Recording::from_path(path).unwrap();
        let original = recording.state_at(60).unwrap();
        assert!(export(&original, Selection::Frame).is_ok());
        let actor = original
            .objects
            .values()
            .find(|o| kind(o) == "PxRigidDynamic")
            .unwrap()
            .id;
        let geometry = original
            .objects
            .values()
            .find(|o| kind(o) == "PxBoxGeometry")
            .unwrap()
            .id;
        let shape = original
            .objects
            .values()
            .find(|o| kind(o) == "PxShape")
            .unwrap()
            .id;
        for replacement in [
            Value::Vec3([f32::NAN, 1.0, 1.0]),
            Value::Vec3([-1.0, 1.0, 1.0]),
            Value::String("1 1 1".into()),
        ] {
            let mut state = original.clone();
            state
                .objects
                .get_mut(&geometry)
                .unwrap()
                .properties
                .insert("HalfExtents".into(), replacement);
            assert!(export(&state, Selection::Frame).is_err());
        }
        let mut missing = original.clone();
        missing
            .objects
            .get_mut(&actor)
            .unwrap()
            .properties
            .remove("Mass");
        assert!(export(&missing, Selection::Frame).is_err());
        for replacement in [
            Value::F32(f32::INFINITY),
            Value::String("1.0".into()),
            Value::F32(-1.0),
        ] {
            let mut state = original.clone();
            state
                .objects
                .get_mut(&actor)
                .unwrap()
                .properties
                .insert("Mass".into(), replacement);
            assert!(export(&state, Selection::Frame).is_err());
        }
        let mut bad_pose = original.clone();
        bad_pose.objects.get_mut(&actor).unwrap().properties.insert(
            "GlobalPose".into(),
            Value::Transform {
                position: [0.0; 3],
                rotation: [0.0; 4],
            },
        );
        assert!(export(&bad_pose, Selection::Frame).is_err());
        let mut bad_flags = original.clone();
        bad_flags
            .objects
            .get_mut(&actor)
            .unwrap()
            .properties
            .insert("ActorFlags".into(), Value::U64(1 << 60));
        assert!(export(&bad_flags, Selection::Frame).is_err());
        let mut bad_filter = original.clone();
        bad_filter
            .objects
            .get_mut(&shape)
            .unwrap()
            .properties
            .insert(
                "QueryFilterData".into(),
                Value::Array(vec![Value::U64(u64::MAX); 4].into()),
            );
        assert!(export(&bad_filter, Selection::Frame).is_err());
    }
}
