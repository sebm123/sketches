//! Flat-earth geometry. Based on cheap-ruler.

use super::{Point, EARTH_RADIUS};

/// Inverse flattening of earth sphere
const FE: f32 = 1.0 / 298.257223563;
const E2: f32 = FE * (2.0 - FE);

lazy_static! {
    static ref RULERS: Vec<Ruler> = (0..=90).map(|deg| Ruler::new(deg as f32)).collect();
}

#[derive(PartialEq, Debug)]
struct Ruler {
    k_lng: f32,
    k_lat: f32,
}

impl Ruler {
    fn for_lat(lat: f32) -> &'static Self {
        let lat = lat.abs() as usize;
        &RULERS[lat]
    }

    fn new(lat: f32) -> Self {
        let m = EARTH_RADIUS.to_radians();
        let cos_lat = (lat.to_radians()).cos();
        let w2 = 1.0 / (1.0 - E2 * (1.0 - cos_lat * cos_lat));
        let w = w2.sqrt();

        Self {
            k_lng: m * w * cos_lat,
            k_lat: m * w * w2 * (1.0 - E2),
        }
    }

    fn dist_cheap(&self, a: &Point, b: &Point) -> f32 {
        let d_lng = wrap_degree(b.lng - a.lng) * self.k_lng;
        let d_lat = (b.lat - a.lat) * self.k_lat;

        d_lng.hypot(d_lat)
    }
}

#[inline]
fn wrap_degree(mut deg: f32) -> f32 {
    while deg < -180.0 {
        deg += 360.0
    }

    while deg > 180.0 {
        deg -= 360.0
    }
    deg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanity_check() {
        let ruler = Ruler::new(0.0);

        let a = &Point {
            lng: 180.0,
            lat: 0.0,
        };
        let b = &Point { lng: 0.0, lat: 0.0 };

        let earth_circ = EARTH_RADIUS * 2.0 * std::f32::consts::PI;
        assert_eq!(ruler.dist_cheap(a, b), earth_circ / 2.0);
    }

    #[test]
    fn haversine_equivalence() {
        let base = &Point {
            lng: 96.920341,
            lat: 32.838261,
        };

        let points: Vec<Point> = [
            (96.920341, 32.838261),
            (96.920421, 32.838295),
            (96.920421, 32.838295),
            (96.920536, 32.838297),
            (96.920684, 32.838293),
            (96.920818, 32.838342),
        ]
        .into_iter()
        .map(|(lng, lat)| Point { lng, lat })
        .collect();

        let ruler = Ruler::new(32.8351);

        for pt in points.iter() {
            let haver_dist = base.dist_to(pt);
            let cheap_dist = ruler.dist_cheap(base, pt);

            let delta = (haver_dist - cheap_dist).abs();

            println!("Haversine: {}\nFlat earth: {}", haver_dist, cheap_dist);

            // TODO: Half a meter delta is quite poor, we should be able to do better.
            assert!(delta < 0.5);
        }
    }

    #[test]
    fn get_for_lat() {
        let ruler1 = Ruler::new(90.0);
        let ruler2 = Ruler::for_lat(90.5);

        assert_eq!(ruler1, *ruler2);
    }
}
