//! Pure 2D puppet-rig math shared by native Canvas and the web kernel bridge.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;

pub type Affine2D = [f64; 6];
pub type Pose = BTreeMap<String, f64>;
pub const IDENTITY: Affine2D = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SkinWeight {
    pub bone: usize,
    pub w: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SkinVertex {
    pub x: f64,
    pub y: f64,
    pub u: f64,
    pub v: f64,
    pub weights: Vec<SkinWeight>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SkinMesh {
    pub vertices: Vec<SkinVertex>,
    pub triangles: Vec<[usize; 3]>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BoneDef {
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
    pub pivot: [f64; 2],
    pub tip: [f64; 2],
}

pub fn compose_affine(a: Affine2D, b: Affine2D) -> Affine2D {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

pub fn invert_affine(m: Affine2D) -> Affine2D {
    let det = m[0] * m[3] - m[1] * m[2];
    if !det.is_finite() || det.abs() < 1e-12 {
        return IDENTITY;
    }
    let inv = 1.0 / det;
    [
        m[3] * inv,
        -m[1] * inv,
        -m[2] * inv,
        m[0] * inv,
        (m[2] * m[5] - m[3] * m[4]) * inv,
        (m[1] * m[4] - m[0] * m[5]) * inv,
    ]
}

pub fn apply_affine(m: Affine2D, x: f64, y: f64) -> [f64; 2] {
    [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]]
}

fn rotation_about(pivot: [f64; 2], degrees: f64) -> Affine2D {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let [x, y] = pivot;
    [
        cos,
        sin,
        -sin,
        cos,
        x - cos * x + sin * y,
        y - sin * x - cos * y,
    ]
}

pub fn compute_world_matrices(bones: &[BoneDef], pose: &Pose) -> Vec<Affine2D> {
    fn resolve(
        i: usize,
        bones: &[BoneDef],
        pose: &Pose,
        cache: &mut [Option<Affine2D>],
        visiting: &mut [bool],
    ) -> Affine2D {
        if let Some(m) = cache[i] {
            return m;
        }
        if visiting[i] {
            return IDENTITY;
        }
        visiting[i] = true;
        let bone = &bones[i];
        let parent = bone
            .parent
            .as_ref()
            .and_then(|name| bones.iter().rposition(|b| &b.name == name))
            .map(|p| resolve(p, bones, pose, cache, visiting))
            .unwrap_or(IDENTITY);
        let local = rotation_about(bone.pivot, pose.get(&bone.name).copied().unwrap_or(0.0));
        let matrix = compose_affine(parent, local);
        visiting[i] = false;
        cache[i] = Some(matrix);
        matrix
    }
    let mut cache = std::vec![None; bones.len()];
    let mut visiting = std::vec![false; bones.len()];
    (0..bones.len())
        .map(|i| resolve(i, bones, pose, &mut cache, &mut visiting))
        .collect()
}

pub fn skin_vertices(mesh: &SkinMesh, bind: &[Affine2D], current: &[Affine2D]) -> Vec<[f64; 2]> {
    mesh.vertices
        .iter()
        .map(|v| {
            if v.weights.is_empty() {
                return [v.x, v.y];
            }
            let mut out = [0.0, 0.0];
            for weight in &v.weights {
                let bind_matrix = bind.get(weight.bone).copied().unwrap_or(IDENTITY);
                let current_matrix = current.get(weight.bone).copied().unwrap_or(IDENTITY);
                let point = apply_affine(
                    compose_affine(current_matrix, invert_affine(bind_matrix)),
                    v.x,
                    v.y,
                );
                out[0] += weight.w * point[0];
                out[1] += weight.w * point[1];
            }
            out
        })
        .collect()
}

pub fn triangle_affine(src: [[f64; 2]; 3], dst: [[f64; 2]; 3]) -> Option<Affine2D> {
    let [s0, s1, s2] = src;
    let det = s0[0] * (s1[1] - s2[1]) + s1[0] * (s2[1] - s0[1]) + s2[0] * (s0[1] - s1[1]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let [d0, d1, d2] = dst;
    let row = |axis: usize| -> [f64; 3] {
        [
            (d0[axis] * (s1[1] - s2[1]) + d1[axis] * (s2[1] - s0[1]) + d2[axis] * (s0[1] - s1[1]))
                / det,
            (d0[axis] * (s2[0] - s1[0]) + d1[axis] * (s0[0] - s2[0]) + d2[axis] * (s1[0] - s0[0]))
                / det,
            (d0[axis] * (s1[0] * s2[1] - s2[0] * s1[1])
                + d1[axis] * (s2[0] * s0[1] - s0[0] * s2[1])
                + d2[axis] * (s0[0] * s1[1] - s1[0] * s0[1]))
                / det,
        ]
    };
    let x = row(0);
    let y = row(1);
    Some([x[0], y[0], x[1], y[1], x[2], y[2]])
}

pub fn vertex_bounds(points: &[[f64; 2]]) -> Option<[f64; 4]> {
    let first = *points.first()?;
    Some(
        points
            .iter()
            .skip(1)
            .fold([first[0], first[1], first[0], first[1]], |mut b, p| {
                b[0] = b[0].min(p[0]);
                b[1] = b[1].min(p[1]);
                b[2] = b[2].max(p[0]);
                b[3] = b[3].max(p[1]);
                b
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fk_and_triangle_affine_share_one_math_source() {
        let bones = vec![BoneDef {
            name: "root".into(),
            parent: None,
            pivot: [0.0, 0.0],
            tip: [1.0, 0.0],
        }];
        let world = compute_world_matrices(&bones, &Pose::from([("root".into(), 90.0)]));
        let point = apply_affine(world[0], 1.0, 0.0);
        assert!(point[0].abs() < 1e-12);
        assert!((point[1] - 1.0).abs() < 1e-12);
        let src = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let dst = [[2.0, 3.0], [4.0, 3.0], [2.0, 5.0]];
        let transform = triangle_affine(src, dst).unwrap();
        assert_eq!(apply_affine(transform, 1.0, 0.0), dst[1]);
    }
}
