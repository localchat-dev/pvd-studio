use super::*;

#[cfg(test)]
mod tests;

const ROOT_FIELDS: &[&str] = &[
    "MaxProjectionIterations",
    "SeparationTolerance",
    "InternalDriveIterations",
    "ExternalDriveIterations",
    "SolverIterationCounts.minPositionIters",
    "SolverIterationCounts.minVelocityIters",
    "SleepThreshold",
    "StabilizationThreshold",
    "WakeCounter",
    "Name",
    "ConcreteTypeName",
    "IsSleeping",
    "Scene",
    "Links",
    "Aggregate",
];
const LINK_FIELDS: &[&str] = &[
    "CMassLocalPose",
    "Mass",
    "MassSpaceInertiaTensor",
    "LinearVelocity",
    "AngularVelocity",
    "RigidBodyFlags",
    "MinCCDAdvanceCoefficient",
    "MaxDepenetrationVelocity",
    "MaxContactImpulse",
    "InvMass",
    "MassSpaceInvInertiaTensor",
    "Parent",
    "Links",
    "InboundJoint",
];
const JOINT_FIELDS: &[&str] = &[
    "ParentPose",
    "ChildPose",
    "TargetOrientation",
    "TargetVelocity",
    "DriveType",
    "Stiffness",
    "Damping",
    "InternalCompliance",
    "ExternalCompliance",
    "SwingLimit.zLimit",
    "SwingLimit.yLimit",
    "TangentialStiffness",
    "TangentialDamping",
    "SwingLimitContactDistance",
    "SwingLimitEnabled",
    "TwistLimit.lower",
    "TwistLimit.upper",
    "TwistLimitEnabled",
    "TwistLimitContactDistance",
    "ConcreteTypeName",
    "Link",
];

pub(super) fn owner<'a>(state: &'a SceneState, mut link: &'a Object) -> Result<&'a Object, Error> {
    let mut seen = BTreeSet::new();
    while kind(link) == "PxArticulationLink" {
        if !seen.insert(link.id) || seen.len() > 64 {
            return Err(error(
                "RepX Articulation parent chain is cyclic or exceeds 64 links",
            ));
        }
        if !matches!(value(link, "Parent")?, Value::Ref(_)) {
            return Err(error("RepX Articulation Parent must be a reference"));
        }
        let parents = references(state, link, "Parent")?;
        if parents.len() != 1 {
            return Err(error("RepX Articulation Link requires one Parent"));
        }
        link = parents[0];
    }
    if kind(link) != "PxArticulation" {
        return Err(error(
            "RepX Articulation Link has no valid Articulation owner",
        ));
    }
    Ok(link)
}

pub(super) fn inbound<'a>(
    state: &'a SceneState,
    link: &Object,
) -> Result<Option<&'a Object>, Error> {
    match link.properties.get("InboundJoint") {
        None | Some(Value::Ref(0)) => Ok(None),
        Some(Value::Ref(_)) => {
            let joints = references(state, link, "InboundJoint")?;
            let joint = joints[0];
            if kind(joint) != "PxArticulationJoint"
                || !matches!(joint.properties.get("Link"), Some(Value::Ref(id)) if *id == link.id)
                || state.resolve_reference(joint.id, "Link", link.id).is_none()
            {
                return Err(error("RepX Articulation Link and InboundJoint disagree"));
            }
            Ok(Some(joint))
        }
        _ => Err(error("RepX Articulation InboundJoint must be a reference")),
    }
}

pub(super) fn links<'a>(
    state: &'a SceneState,
    articulation: &'a Object,
) -> Result<Vec<&'a Object>, Error> {
    if !matches!(value(articulation, "Links")?, Value::Array(values) if values.len() == 1) {
        return Err(error(
            "RepX Articulation requires one root Link in its reference array",
        ));
    }
    let roots = references(state, articulation, "Links")?;
    if roots.len() != 1 {
        return Err(error(
            "RepX Articulation requires exactly one captured root Link",
        ));
    }
    let mut pending = vec![(articulation, roots[0])];
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    while let Some((parent, link)) = pending.pop() {
        if kind(link) != "PxArticulationLink" || !seen.insert(link.id) || seen.len() > 64 {
            return Err(error(
                "RepX Articulation tree has repeated, invalid or more than 64 Links",
            ));
        }
        if !matches!(link.properties.get("Parent"), Some(Value::Ref(id)) if *id == parent.id)
            || state
                .resolve_reference(link.id, "Parent", parent.id)
                .is_none()
        {
            return Err(error(
                "RepX Articulation Parent and Links relationships disagree",
            ));
        }
        if inbound(state, link)?.is_some() != (parent.id != articulation.id) {
            return Err(error(
                "RepX Articulation root must have no joint; other Links require an inbound joint",
            ));
        }
        ordered.push(link);
        if let Some(children) = link.properties.get("Links") {
            if !matches!(children, Value::Array(values) if values.len() <= 64) {
                return Err(error(
                    "RepX Articulation children must be a bounded reference array",
                ));
            }
            pending.extend(
                references(state, link, "Links")?
                    .into_iter()
                    .rev()
                    .map(|child| (link, child)),
            );
        }
    }
    let scene = references(state, articulation, "Scene")?;
    if scene.len() != 1
        || kind(scene[0]) != "PxScene"
        || !matches!(scene[0].properties.get("Articulations"), Some(Value::Array(values)) if values.iter().filter(|value| **value == Value::Ref(articulation.id)).count() == 1)
        || state
            .resolve_reference(scene[0].id, "Articulations", articulation.id)
            .is_none()
    {
        return Err(error("RepX Articulation and Scene relationships disagree"));
    }
    Ok(ordered)
}

pub(super) fn write_link(
    state: &SceneState,
    link: &Object,
    shapes: &ShapePlan<'_>,
    shape_xml: &mut BTreeMap<u64, String>,
) -> Result<String, Error> {
    validate_fields(link, ACTOR_FIELDS, LINK_FIELDS)?;
    validate_actor(link, false)?;
    if !matches!(value(link, "ConcreteTypeName")?, Value::String(name) if name == "PxArticulationLink")
    {
        return Err(error(
            "RepX Articulation Link has an incompatible concrete type",
        ));
    }
    pose(link, "CMassLocalPose")?;
    vector(link, "LinearVelocity")?;
    vector(link, "AngularVelocity")?;
    if real(link, "Mass")? <= 0.0
        || vector(link, "MassSpaceInertiaTensor")?
            .iter()
            .any(|v| *v <= 0.0)
        || real(link, "MaxDepenetrationVelocity")? < 0.0
        || real(link, "MaxContactImpulse")? < 0.0
        || !(0.0..=1.0).contains(&real(link, "MinCCDAdvanceCoefficient")?)
    {
        return Err(error(
            "RepX Articulation Link mass, inertia or contact limits violate SDK bounds",
        ));
    }
    let flags = integer(link, "RigidBodyFlags")?;
    if flags & !124 != 0 || flags & 36 == 36 {
        return Err(error(
            "RepX Articulation Link requires known non-kinematic flags without simultaneous CCD and speculative CCD",
        ));
    }
    let mut xml = String::new();
    write_actor_common(&mut xml, link, shapes, shape_xml)?;
    for field in &LINK_FIELDS[..5] {
        property(&mut xml, link, field)?;
    }
    flag_property(
        &mut xml,
        link,
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
    for field in &LINK_FIELDS[6..9] {
        property(&mut xml, link, field)?;
    }
    if let Some(joint) = inbound(state, link)? {
        write_joint(&mut xml, joint)?;
    }
    Ok(xml)
}

fn boolean(xml: &mut String, object: &Object, name: &str) -> Result<(), Error> {
    let Value::Bool(value) = value(object, name)? else {
        return Err(error(format!("RepX {name} must be Boolean")));
    };
    tag(xml, name, if *value { "true" } else { "false" })
}

fn pair(xml: &mut String, object: &Object, name: &str, fields: &[&str]) -> Result<(), Error> {
    writeln!(xml, "<{name}>").unwrap();
    for field in fields {
        tag(
            xml,
            field,
            &value_text(value(object, &format!("{name}.{field}"))?)?,
        )?;
    }
    writeln!(xml, "</{name}>").unwrap();
    Ok(())
}

fn write_joint(xml: &mut String, joint: &Object) -> Result<(), Error> {
    validate_fields(joint, JOINT_FIELDS, &[])?;
    if !matches!(value(joint, "ConcreteTypeName")?, Value::String(name) if name == "PxArticulationJoint")
    {
        return Err(error(
            "RepX Articulation Joint concrete type is inconsistent",
        ));
    }
    pose(joint, "ParentPose")?;
    pose(joint, "ChildPose")?;
    vector(joint, "TargetVelocity")?;
    let drive_type = integer(joint, "DriveType")?;
    if drive_type > 1 {
        return Err(error("RepX Articulation drive type is unsupported"));
    }
    let Value::Quat(target) = value(joint, "TargetOrientation")? else {
        return Err(error("RepX Articulation target must be a quaternion"));
    };
    if drive_type == 0 {
        if !target.iter().all(|v| v.is_finite())
            || (target.iter().map(|v| v * v).sum::<f32>().sqrt() - 1.0).abs() >= 1e-4
        {
            return Err(error(
                "RepX Articulation target must be a normalized quaternion",
            ));
        }
    } else {
        // 官方导入器先以 eTARGET 写入目标；内侧余量隔离 SDK 浮点重关联误差。
        let length = target
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            .sqrt();
        if !target.iter().all(|v| v.is_finite())
            || target[3].to_bits() & 0x7fff_ffff != 0
            || (length - 1.0).abs() >= 9e-5
        {
            return Err(error(
                "RepX eERROR target requires w=0 and length within 0.00009 of one",
            ));
        }
    }
    for field in [
        "Stiffness",
        "Damping",
        "TangentialStiffness",
        "TangentialDamping",
        "SwingLimitContactDistance",
        "TwistLimitContactDistance",
    ] {
        if real(joint, field)? < 0.0 {
            return Err(error(format!("RepX {field} must be non-negative")));
        }
    }
    for field in ["InternalCompliance", "ExternalCompliance"] {
        if real(joint, field)? <= 0.0 {
            return Err(error(format!("RepX {field} must be positive")));
        }
    }
    for field in ["SwingLimit.zLimit", "SwingLimit.yLimit"] {
        let limit = real(joint, field)?;
        if limit <= 0.0 || limit >= std::f32::consts::PI {
            return Err(error(
                "RepX Articulation swing limit must be between zero and pi",
            ));
        }
    }
    let lower = real(joint, "TwistLimit.lower")?;
    let upper = real(joint, "TwistLimit.upper")?;
    if lower >= upper || lower <= -std::f32::consts::PI || upper >= std::f32::consts::PI {
        return Err(error(
            "RepX Articulation twist limits must be ordered within (-pi, pi)",
        ));
    }
    xml.push_str("<Joint>\n");
    for field in &JOINT_FIELDS[..4] {
        property(xml, joint, field)?;
    }
    tag(
        xml,
        "DriveType",
        if drive_type == 0 { "eTARGET" } else { "eERROR" },
    )?;
    for field in &JOINT_FIELDS[5..9] {
        property(xml, joint, field)?;
    }
    pair(xml, joint, "SwingLimit", &["zLimit", "yLimit"])?;
    for field in &JOINT_FIELDS[11..14] {
        property(xml, joint, field)?;
    }
    boolean(xml, joint, "SwingLimitEnabled")?;
    pair(xml, joint, "TwistLimit", &["lower", "upper"])?;
    boolean(xml, joint, "TwistLimitEnabled")?;
    property(xml, joint, "TwistLimitContactDistance")?;
    xml.push_str("</Joint>\n");
    Ok(())
}

pub(super) fn write(
    xml: &mut String,
    state: &SceneState,
    articulation: &Object,
    link_xml: &BTreeMap<u64, String>,
) -> Result<(), Error> {
    validate_fields(articulation, ROOT_FIELDS, &[])?;
    if !matches!(value(articulation, "ConcreteTypeName")?, Value::String(name) if name == "PxArticulation")
        || !matches!(value(articulation, "Name")?, Value::String(_))
    {
        return Err(error("RepX Articulation name or concrete type is invalid"));
    }
    for field in [
        "MaxProjectionIterations",
        "InternalDriveIterations",
        "ExternalDriveIterations",
    ] {
        if integer(articulation, field)? > u32::MAX as u64 {
            return Err(error("RepX Articulation iteration count exceeds u32"));
        }
    }
    for field in [
        "SolverIterationCounts.minPositionIters",
        "SolverIterationCounts.minVelocityIters",
    ] {
        if !(1..=255).contains(&integer(articulation, field)?) {
            return Err(error(
                "RepX Articulation solver iterations must be within 1..255",
            ));
        }
    }
    for field in [
        "SeparationTolerance",
        "SleepThreshold",
        "StabilizationThreshold",
        "WakeCounter",
    ] {
        if real(articulation, field)? < 0.0 {
            return Err(error(format!("RepX {field} must be non-negative")));
        }
    }
    xml.push_str("<PxArticulation>\n");
    tag(xml, "Id", &articulation.id.to_string())?;
    for field in &ROOT_FIELDS[..4] {
        property(xml, articulation, field)?;
    }
    pair(
        xml,
        articulation,
        "SolverIterationCounts",
        &["minPositionIters", "minVelocityIters"],
    )?;
    for field in &ROOT_FIELDS[6..9] {
        property(xml, articulation, field)?;
    }
    xml.push_str("<Links>\n");
    for link in links(state, articulation)? {
        xml.push_str("<PxArticulationLink>\n");
        let Value::Ref(parent) = value(link, "Parent")? else {
            return Err(error("RepX Articulation Parent must be a reference"));
        };
        if *parent != articulation.id {
            tag(xml, "Parent", &parent.to_string())?;
        }
        tag(xml, "Id", &link.id.to_string())?;
        xml.push_str(
            link_xml
                .get(&link.id)
                .ok_or_else(|| error("RepX Articulation Link dependency is missing"))?,
        );
        xml.push_str("</PxArticulationLink>\n");
    }
    xml.push_str("</Links>\n");
    property(xml, articulation, "Name")?;
    xml.push_str("</PxArticulation>\n");
    Ok(())
}
