// Small pose / quaternion / raycast helpers (openxr Posef-based).
use openxr as xr;

pub fn quat_rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    let tx = 2.0 * (y * v[2] - z * v[1]);
    let ty = 2.0 * (z * v[0] - x * v[2]);
    let tz = 2.0 * (x * v[1] - y * v[0]);
    [
        v[0] + w * tx + (y * tz - z * ty),
        v[1] + w * ty + (z * tx - x * tz),
        v[2] + w * tz + (x * ty - y * tx),
    ]
}

pub fn q_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let (ax, ay, az, aw) = (a[0], a[1], a[2], a[3]);
    let (bx, by, bz, bw) = (b[0], b[1], b[2], b[3]);
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

pub fn qf(q: &xr::Quaternionf) -> [f32; 4] {
    [q.x, q.y, q.z, q.w]
}

pub fn vec3f(v: [f32; 3]) -> xr::Vector3f {
    xr::Vector3f { x: v[0], y: v[1], z: v[2] }
}

pub fn quatf(q: [f32; 4]) -> xr::Quaternionf {
    xr::Quaternionf { x: q[0], y: q[1], z: q[2], w: q[3] }
}

/// Quaternion [x,y,z,w] from yaw(Y), pitch(X), roll(Z) in degrees (applied q = qY*qX*qZ).
pub fn quat_from_euler_deg(yaw: f32, pitch: f32, roll: f32) -> [f32; 4] {
    let d = std::f32::consts::PI / 180.0;
    let (cy, sy) = ((yaw * d * 0.5).cos(), (yaw * d * 0.5).sin());
    let (cp, sp) = ((pitch * d * 0.5).cos(), (pitch * d * 0.5).sin());
    let (cr, sr) = ((roll * d * 0.5).cos(), (roll * d * 0.5).sin());
    q_mul(q_mul([0.0, sy, 0.0, cy], [sp, 0.0, 0.0, cp]), [0.0, 0.0, sr, cr])
}

pub fn pose_compose(a: &xr::Posef, b: &xr::Posef) -> xr::Posef {
    let q = q_mul(qf(&a.orientation), qf(&b.orientation));
    let rp = quat_rotate(qf(&a.orientation), [b.position.x, b.position.y, b.position.z]);
    xr::Posef {
        orientation: quatf(q),
        position: vec3f([a.position.x + rp[0], a.position.y + rp[1], a.position.z + rp[2]]),
    }
}

pub fn pose_invert(a: &xr::Posef) -> xr::Posef {
    let iq = [-a.orientation.x, -a.orientation.y, -a.orientation.z, a.orientation.w];
    let ip = quat_rotate(iq, [a.position.x, a.position.y, a.position.z]);
    xr::Posef { orientation: quatf(iq), position: vec3f([-ip[0], -ip[1], -ip[2]]) }
}

pub fn locate_pose(aim: &xr::Space, base: &xr::Space, time: xr::Time) -> Option<xr::Posef> {
    let loc = aim.locate(base, time).ok()?;
    let need = xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;
    if loc.location_flags.contains(need) {
        Some(loc.pose)
    } else {
        None
    }
}

/// Raycast a controller aim pose onto a quad; returns (u, v, distance) on hit.
pub fn raycast(pose: &xr::Posef, quad: &xr::Posef, size_m: (f32, f32)) -> Option<(f32, f32, f32)> {
    let o = [pose.position.x, pose.position.y, pose.position.z];
    let q = qf(&pose.orientation);
    let qq = qf(&quad.orientation);
    let dir = quat_rotate(q, [0.0, 0.0, -1.0]);
    let normal = quat_rotate(qq, [0.0, 0.0, 1.0]);
    let axis_x = quat_rotate(qq, [1.0, 0.0, 0.0]);
    let axis_y = quat_rotate(qq, [0.0, 1.0, 0.0]);
    let c = [quad.position.x, quad.position.y, quad.position.z];

    let denom = dir[0] * normal[0] + dir[1] * normal[1] + dir[2] * normal[2];
    if denom.abs() < 1e-6 {
        return None;
    }
    let co = [c[0] - o[0], c[1] - o[1], c[2] - o[2]];
    let t = (co[0] * normal[0] + co[1] * normal[1] + co[2] * normal[2]) / denom;
    if t <= 0.0 {
        return None;
    }
    let p = [o[0] + dir[0] * t, o[1] + dir[1] * t, o[2] + dir[2] * t];
    let off = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
    let lx = off[0] * axis_x[0] + off[1] * axis_x[1] + off[2] * axis_x[2];
    let ly = off[0] * axis_y[0] + off[1] * axis_y[1] + off[2] * axis_y[2];
    if lx.abs() > size_m.0 * 0.5 || ly.abs() > size_m.1 * 0.5 {
        return None;
    }
    Some((lx / size_m.0 + 0.5, 0.5 - ly / size_m.1, t))
}

/// Forward direction (-Z) of a pose, normalised.
pub fn forward(pose: &xr::Posef) -> [f32; 3] {
    quat_rotate(qf(&pose.orientation), [0.0, 0.0, -1.0])
}

pub fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-6 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 0.0, 1.0]
    }
}

pub fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Quaternion from orthonormal basis columns (rotation mapping ex->x, ey->y, ez->z).
pub fn quat_from_axes(x: [f32; 3], y: [f32; 3], z: [f32; 3]) -> [f32; 4] {
    let (m00, m10, m20) = (x[0], x[1], x[2]);
    let (m01, m11, m21) = (y[0], y[1], y[2]);
    let (m02, m12, m22) = (z[0], z[1], z[2]);
    let tr = m00 + m11 + m22;
    if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [(m21 - m12) / s, (m02 - m20) / s, (m10 - m01) / s, 0.25 * s]
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [0.25 * s, (m01 + m10) / s, (m02 + m20) / s, (m21 - m12) / s]
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [(m01 + m10) / s, 0.25 * s, (m12 + m21) / s, (m02 - m20) / s]
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [(m02 + m20) / s, (m12 + m21) / s, 0.25 * s, (m10 - m01) / s]
    }
}
