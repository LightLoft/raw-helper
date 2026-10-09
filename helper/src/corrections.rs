//! A DNG's lens correction opcodes (DNG specification 1.4, chapter 7), read and checked, left
//! for the application to apply: GainMap from OpcodeList2 (lens shading, over the samples before
//! demosaicing), FixVignetteRadial and WarpRectilinear from OpcodeList3 (after it). The lists
//! come from the file: every count and size is checked, and an opcode that fails is skipped.

use loft_raw_protocol::{GainMap, RadialVignette, Warp};

use crate::opcodes::{each, Reader};

const WARP_RECTILINEAR: u32 = 1;
const FIX_VIGNETTE_RADIAL: u32 = 3;
const GAIN_MAP: u32 = 9;
/// Planes a sample may have.
const PLANES_MAX: u32 = 4;
/// Points of a gain map in each direction, its values in all, and maps in a list: far above what
/// cameras write (a phone: 4 maps of 40 × 30), far below what would cost memory.
const POINTS_MAX: u32 = 1024;
const VALUES_MAX: u64 = 1 << 20;
const MAPS_MAX: usize = 16;

/// The gain maps of OpcodeList2.
pub fn gain_maps(list: &[u8]) -> Vec<GainMap> {
    each(list)
        .into_iter()
        .filter(|(id, _)| *id == GAIN_MAP)
        .filter_map(|(_, body)| gain_map(body))
        .take(MAPS_MAX)
        .collect()
}

fn gain_map(body: &[u8]) -> Option<GainMap> {
    let mut r = Reader { bytes: body, at: 0 };
    let [top, left, bottom, right, plane, planes, row_pitch, column_pitch, points_v, points_h] =
        [(); 10].map(|_| r.u32());
    let area = [top?, left?, bottom?, right?];
    let [spacing_v, spacing_h, origin_v, origin_h] = [(); 4].map(|_| r.f64());
    let map_planes = r.u32()?;
    let (plane, planes, points) = (plane?, planes?, [points_v?, points_h?]);
    let pitch = [row_pitch?, column_pitch?];
    let spacing = [spacing_v?, spacing_h?];
    let origin = [origin_v?, origin_h?];
    let valid = area[0] < area[2]
        && area[1] < area[3]
        && plane < PLANES_MAX
        && (1..=PLANES_MAX).contains(&planes)
        && pitch.iter().all(|&p| p >= 1)
        && points.iter().all(|&n| (1..=POINTS_MAX).contains(&n))
        && (1..=PLANES_MAX).contains(&map_planes)
        && spacing.iter().chain(&origin).all(|v| v.is_finite())
        && spacing.iter().all(|&s| s >= 0.0);
    if !valid {
        return None;
    }
    let count = u64::from(points[0]) * u64::from(points[1]) * u64::from(map_planes);
    if count > VALUES_MAX {
        return None;
    }
    let gains: Option<Vec<f32>> = (0..count).map(|_| r.f32()).collect();
    let gains = gains.filter(|g| g.iter().all(|v| v.is_finite() && *v >= 0.0))?;
    Some(GainMap {
        area,
        plane,
        planes,
        pitch,
        points,
        spacing,
        origin,
        map_planes,
        gains,
    })
}

/// FixVignetteRadial and WarpRectilinear of OpcodeList3 (the last of each, when several).
pub fn lens(list: &[u8]) -> (Option<RadialVignette>, Option<Warp>) {
    let mut vignette = None;
    let mut warp = None;
    for (id, body) in each(list) {
        let mut r = Reader { bytes: body, at: 0 };
        match id {
            FIX_VIGNETTE_RADIAL => {
                let values = [(); 7].map(|_| r.f64());
                if let Some(v) = values.iter().copied().collect::<Option<Vec<f64>>>() {
                    if v.iter().all(|x| x.is_finite()) {
                        vignette = Some(RadialVignette {
                            k: [v[0], v[1], v[2], v[3], v[4]],
                            centre: [v[5], v[6]],
                        });
                    }
                }
            }
            WARP_RECTILINEAR => {
                let Some(n) = r.u32().filter(|n| (1..=PLANES_MAX).contains(n)) else {
                    continue;
                };
                let planes: Option<Vec<[f64; 6]>> = (0..n)
                    .map(|_| {
                        let c = [(); 6].map(|_| r.f64());
                        c.iter()
                            .all(Option::is_some)
                            .then(|| c.map(|v| v.unwrap_or(0.0)))
                    })
                    .collect();
                let centre = [r.f64(), r.f64()];
                if let (Some(planes), [Some(cx), Some(cy)]) = (planes, centre) {
                    let finite = planes
                        .iter()
                        .flatten()
                        .chain([&cx, &cy])
                        .all(|v| v.is_finite());
                    if finite {
                        warp = Some(Warp {
                            planes,
                            centre: [cx, cy],
                        });
                    }
                }
            }
            _ => {}
        }
    }
    (vignette, warp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(id: u32, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for v in [id, 0x0103_0000, 0, body.len() as u32] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(body);
        out
    }

    fn list(opcodes: &[Vec<u8>]) -> Vec<u8> {
        let mut out = (opcodes.len() as u32).to_be_bytes().to_vec();
        for op in opcodes {
            out.extend_from_slice(op);
        }
        out
    }

    fn u32s(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn f64s(values: &[f64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// A phone's map: one Bayer phase (pitch 2), 3 × 4 points over the image.
    fn phone_map(points: [u32; 2], gains: usize) -> Vec<u8> {
        let mut body = u32s(&[0, 1, 3000, 4000, 0, 1, 2, 2, points[0], points[1]]);
        body.extend(f64s(&[0.5, 1.0 / 3.0, 0.0, 0.0]));
        body.extend(u32s(&[1]));
        for i in 0..gains {
            body.extend_from_slice(&(1.0 + i as f32 / 10.0).to_be_bytes());
        }
        header(GAIN_MAP, &body)
    }

    #[test]
    fn a_phones_gain_maps_are_read() {
        let maps = gain_maps(&list(&[phone_map([3, 4], 12), header(8, &[0; 8])]));
        assert_eq!(maps.len(), 1);
        let m = &maps[0];
        assert_eq!(m.area, [0, 1, 3000, 4000]);
        assert_eq!((m.pitch, m.points, m.map_planes), ([2, 2], [3, 4], 1));
        assert_eq!(m.spacing, [0.5, 1.0 / 3.0]);
        assert_eq!(m.gains.len(), 12);
        assert!((m.gains[11] - 2.1).abs() < 1e-6);
    }

    #[test]
    fn lens_opcodes_are_read() {
        let vignette = header(
            FIX_VIGNETTE_RADIAL,
            &f64s(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.45, 0.55]),
        );
        let mut warp = u32s(&[3]);
        for plane in 0..3 {
            warp.extend(f64s(&[
                1.0 + f64::from(plane) / 100.0,
                0.01,
                0.0,
                0.0,
                0.0,
                0.0,
            ]));
        }
        warp.extend(f64s(&[0.5, 0.5]));
        let (v, w) = lens(&list(&[vignette, header(WARP_RECTILINEAR, &warp)]));
        let v = v.expect("vignette");
        assert_eq!(v.k, [0.1, 0.2, 0.3, 0.4, 0.5]);
        assert_eq!(v.centre, [0.45, 0.55]);
        let w = w.expect("warp");
        assert_eq!(w.planes.len(), 3);
        assert_eq!(w.planes[2][0], 1.02);
        assert_eq!(w.centre, [0.5, 0.5]);
    }

    #[test]
    fn hostile_or_partial_opcodes_are_skipped() {
        // Fewer gains than points, too many points, a size past the end.
        let short = phone_map([3, 4], 11);
        let huge = phone_map([2000, 2000], 0);
        let mut lying = phone_map([3, 4], 12);
        lying[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        for ops in [vec![short], vec![huge], vec![lying]] {
            assert!(gain_maps(&list(&ops)).is_empty());
        }
        // A warp claiming 1000 planes, a vignette cut short, a NaN centre.
        let mut planes = u32s(&[1000]);
        planes.extend(f64s(&[0.0; 8]));
        let cut = f64s(&[0.1, 0.2]);
        let mut nan = u32s(&[1]);
        nan.extend(f64s(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, f64::NAN, 0.5]));
        let (v, w) = lens(&list(&[
            header(WARP_RECTILINEAR, &planes),
            header(FIX_VIGNETTE_RADIAL, &cut),
            header(WARP_RECTILINEAR, &nan),
        ]));
        assert!(v.is_none() && w.is_none());
        assert!(gain_maps(&[]).is_empty());
        assert_eq!(lens(&[1, 2]), (None, None));
    }
}
