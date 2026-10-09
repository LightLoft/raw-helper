//! A DNG's linearisation opcodes (OpcodeList2, DNG specification 1.4, chapter 7): MapPolynomial
//! and MapTable over the whole image, applied to the samples once scaled between black and
//! white. A lossy DNG stores 8-bit values that only become linear through them. They are turned
//! into tables the engine interpolates; the lens corrections are read by corrections.rs. The
//! list comes from the file: every count and size is checked.

use loft_raw_protocol::LINEARIZATION_POINTS;

const MAP_TABLE: u32 = 7;
const MAP_POLYNOMIAL: u32 = 8;
/// Highest degree a MapPolynomial may have (the specification's limit).
const DEGREE_MAX: u32 = 8;
/// Planes a sample may have.
const PLANES_MAX: u32 = 4;

/// Big-endian reader over the list (opcode lists are big-endian whatever the file's order).
pub(crate) struct Reader<'a> {
    pub bytes: &'a [u8],
    pub at: usize,
}

impl<'a> Reader<'a> {
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    pub fn f64(&mut self) -> Option<f64> {
        Some(f64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    pub fn f32(&mut self) -> Option<f32> {
        Some(f32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
}

/// The opcodes of a list, as (identifier, body), up to the first one whose size runs past it.
pub(crate) fn each(list: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut r = Reader { bytes: list, at: 0 };
    let Some(count) = r.u32() else {
        return out;
    };
    for _ in 0..count {
        let (Some(id), Some(_version), Some(_flags), Some(size)) =
            (r.u32(), r.u32(), r.u32(), r.u32())
        else {
            break;
        };
        let Some(body) = r.take(size as usize) else {
            break;
        };
        out.push((id, body));
    }
    out
}

/// The opcode's area and planes, when it covers the whole `width` × `height` image, every row
/// and column: the planes it maps.
fn whole_image(r: &mut Reader<'_>, width: u32, height: u32) -> Option<std::ops::Range<u32>> {
    let [top, left, bottom, right, plane, planes, row_pitch, column_pitch] =
        [(); 8].map(|_| r.u32()).map(|v| v.unwrap_or(u32::MAX));
    let whole = top == 0
        && left == 0
        && bottom >= height
        && right >= width
        && row_pitch == 1
        && column_pitch == 1
        && plane < PLANES_MAX
        && planes >= 1;
    whole.then(|| plane..plane.saturating_add(planes).min(PLANES_MAX))
}

/// The linearisation of an image of `width` × `height` samples by the opcode `list`: a table per
/// plane (see `SensorInfo::linearization`), empty when it holds no such opcode.
pub fn linearization(list: &[u8], width: u32, height: u32) -> Vec<Vec<f32>> {
    let identity: Vec<f32> = (0..LINEARIZATION_POINTS)
        .map(|i| i as f32 / (LINEARIZATION_POINTS - 1) as f32)
        .collect();
    let mut tables: Vec<Vec<f32>> = Vec::new();
    let mut r = Reader { bytes: list, at: 0 };
    let Some(count) = r.u32() else {
        return tables;
    };
    for _ in 0..count {
        let (Some(id), Some(_version), Some(_flags), Some(size)) =
            (r.u32(), r.u32(), r.u32(), r.u32())
        else {
            break;
        };
        let Some(body) = r.take(size as usize) else {
            break;
        };
        let mut b = Reader { bytes: body, at: 0 };
        // Each mapping takes a value in [0, 1] to another, clipped to [0, 1].
        match id {
            MAP_POLYNOMIAL => {
                let Some(planes) = whole_image(&mut b, width, height) else {
                    continue;
                };
                let Some(degree) = b.u32().filter(|&d| d <= DEGREE_MAX) else {
                    continue;
                };
                let coefficients: Option<Vec<f64>> = (0..=degree).map(|_| b.f64()).collect();
                let Some(c) = coefficients.filter(|c| c.iter().all(|v| v.is_finite())) else {
                    continue;
                };
                apply(&mut tables, &identity, planes, &|x| {
                    c.iter().rev().fold(0.0, |sum, k| sum * x + k)
                });
            }
            MAP_TABLE => {
                let Some(planes) = whole_image(&mut b, width, height) else {
                    continue;
                };
                let Some(n) = b.u32().filter(|&n| (1..=65536).contains(&n)) else {
                    continue;
                };
                let table: Option<Vec<u16>> = (0..n).map(|_| b.u16()).collect();
                let Some(table) = table else {
                    continue;
                };
                // The samples as 16-bit values index the table; past its end, its last entry.
                apply(&mut tables, &identity, planes, &|x| {
                    let index = ((x * 65535.0).round() as usize).min(table.len() - 1);
                    f64::from(table[index]) / 65535.0
                });
            }
            _ => {}
        }
    }
    tables
}

/// Maps the tables of `planes` (identity where none yet) through `map`, in the list's order.
fn apply(
    tables: &mut Vec<Vec<f32>>,
    identity: &[f32],
    planes: std::ops::Range<u32>,
    map: &dyn Fn(f64) -> f64,
) {
    for plane in planes {
        let plane = plane as usize;
        if tables.len() <= plane {
            tables.resize(plane + 1, identity.to_vec());
        }
        for v in &mut tables[plane] {
            *v = map(f64::from(*v)).clamp(0.0, 1.0) as f32;
        }
    }
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

    fn area(plane: u32, planes: u32) -> Vec<u8> {
        [0u32, 0, 3840, 5760, plane, planes, 1, 1]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect()
    }

    fn list(opcodes: &[Vec<u8>]) -> Vec<u8> {
        let mut out = (opcodes.len() as u32).to_be_bytes().to_vec();
        for op in opcodes {
            out.extend_from_slice(op);
        }
        out
    }

    fn polynomial(plane: u32, c: &[f64]) -> Vec<u8> {
        let mut body = area(plane, 1);
        body.extend_from_slice(&(c.len() as u32 - 1).to_be_bytes());
        for v in c {
            body.extend_from_slice(&v.to_be_bytes());
        }
        header(MAP_POLYNOMIAL, &body)
    }

    #[test]
    fn a_lossy_dngs_polynomials_become_tables() {
        // The cubic a lossy DNG stores for each plane.
        let cubic = [0.0, 0.0625, 0.0, 0.9375];
        let ops: Vec<Vec<u8>> = (0..3).map(|p| polynomial(p, &cubic)).collect();
        let tables = linearization(&list(&ops), 5760, 3840);
        assert_eq!(tables.len(), 3);
        for table in &tables {
            assert_eq!(table.len(), LINEARIZATION_POINTS);
            assert_eq!(table[0], 0.0);
            assert!((table[LINEARIZATION_POINTS - 1] - 1.0).abs() < 1e-6);
            let x = 64.0 / 127.0;
            let expected = 0.0625 * x + 0.9375 * x * x * x;
            assert!((f64::from(table[64]) - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn a_table_maps_and_composes_in_order() {
        // Plane 0 halved by a table, then squared by a polynomial.
        let mut body = area(0, 1);
        body.extend_from_slice(&65536u32.to_be_bytes());
        for i in 0..65536u32 {
            body.extend_from_slice(&((i / 2) as u16).to_be_bytes());
        }
        let ops = [header(MAP_TABLE, &body), polynomial(0, &[0.0, 0.0, 1.0])];
        let tables = linearization(&list(&ops), 5760, 3840);
        assert_eq!(tables.len(), 1);
        let last = f64::from(tables[0][LINEARIZATION_POINTS - 1]);
        assert!((last - 0.25).abs() < 1e-4, "{last}");
    }

    #[test]
    fn hostile_or_partial_lists_are_ignored() {
        let cubic = [0.0, 0.0625, 0.0, 0.9375];
        // A part of the image only, a size past the end, a huge degree, a count past the end.
        let mut part = vec![0u8; 0];
        for v in [0u32, 0, 100, 100, 0, 1, 1, 1, 1] {
            part.extend_from_slice(&v.to_be_bytes());
        }
        let mut lying = header(MAP_POLYNOMIAL, &area(0, 1));
        lying[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        let mut degree = area(0, 1);
        degree.extend_from_slice(&u32::MAX.to_be_bytes());
        for ops in [
            vec![header(MAP_POLYNOMIAL, &part)],
            vec![lying],
            vec![header(MAP_POLYNOMIAL, &degree)],
        ] {
            assert!(linearization(&list(&ops), 5760, 3840).is_empty());
        }
        let mut short = list(&[polynomial(0, &cubic)]);
        short[0..4].copy_from_slice(&1000u32.to_be_bytes());
        assert_eq!(linearization(&short, 5760, 3840).len(), 1);
        assert!(linearization(&[], 1, 1).is_empty());
        assert!(linearization(&[0, 0], 1, 1).is_empty());
    }
}
