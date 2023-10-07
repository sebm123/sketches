use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::Result;
use fitparser::de::{from_reader_with_options, DecodeOption};
use fitparser::profile::MesgNum;
use fitparser::Value;
use flate2::read::GzDecoder;
use geo::{EuclideanDistance, HaversineLength};
use geo_types::{Coord, LineString, MultiLineString, Point};
use rusqlite::params;
use time::OffsetDateTime;

use crate::db::{encode_line, SqlDateTime};
use crate::tile::{BBox, LngLat, Tile, WebMercator};
use crate::{DEFAULT_TILE_EXTENT, DEFAULT_ZOOM_LEVELS};

// TODO: not happy with the ergonomics of this.
struct TileClipper {
    zoom: u8,
    current: Option<(Tile, BBox)>,
    tiles: HashMap<Tile, Vec<LineString<u16>>>,
}

impl TileClipper {
    fn new(zoom: u8) -> Self {
        Self {
            zoom,
            tiles: HashMap::new(),
            current: None,
        }
    }

    fn bounding_tile(&self, pt: &WebMercator) -> (Tile, BBox) {
        let tile = pt.tile(self.zoom);
        let bbox = tile.xy_bounds();
        (tile, bbox)
    }

    fn last_line(&mut self, tile: &Tile) -> &mut LineString<u16> {
        let lines = self.tiles.entry(*tile).or_insert_with(Vec::new);

        if lines.is_empty() {
            lines.push(LineString::new(vec![]));
        }

        // TODO: avoid the unwrap
        lines.last_mut().unwrap()
    }

    fn add_line_segment(&mut self, start: WebMercator, end: WebMercator) {
        let (tile, bbox) = match self.current {
            Some(pair) => pair,
            None => {
                let pair = self.bounding_tile(&start);
                self.current = Some(pair);
                pair
            }
        };

        match bbox.clip_line(&start, &end) {
            // [start, end] doesn't intersect with the current tile at all, reposition it.
            None => {
                self.finish_segment();
                self.current = Some(self.bounding_tile(&start));
                // todo: should we add new segment after shifting bbox?
                // self.add_line_segment(start, end, c+1);
            }

            // [start, end] is at least partially contained within the current tile.
            Some((a, b)) => {
                let line = self.last_line(&tile);
                if line.0.is_empty() {
                    line.0
                        .push(a.to_pixel(&bbox, DEFAULT_TILE_EXTENT as u16).into());
                }

                line.0
                    .push(b.to_pixel(&bbox, DEFAULT_TILE_EXTENT as u16).into());

                // If we've modified the end point, we've left the current tile.
                if b != end {
                    self.finish_segment();

                    let (next_tile, next_bbox) = self.bounding_tile(&end);
                    if next_tile != tile {
                        self.current = Some((next_tile, next_bbox));
                        self.add_line_segment(b, end);
                    }
                }
            }
        }
    }

    fn finish_segment(&mut self) {
        if let Some((tile, _)) = self.current {
            self.tiles.entry(tile).and_modify(|lines| {
                lines.push(LineString::new(vec![]));
            });
        }
    }
}

pub struct ClippedTiles(Vec<TileClipper>);

impl ClippedTiles {
    pub fn iter(&self) -> impl Iterator<Item = (&Tile, &LineString<u16>)> {
        self.0
            .iter()
            .flat_map(|clip| clip.tiles.iter())
            .filter(|(_, lines)| !lines.is_empty())
            .flat_map(|(tile, lines)| lines.iter().map(move |line| (tile, line)))
    }
}

#[derive(Clone)]
pub struct RawActivity {
    pub title: Option<String>,
    pub start_time: Option<SqlDateTime>,
    pub duration_secs: Option<u64>,
    pub tracks: MultiLineString,
}

impl RawActivity {
    /// How far apart two points can be before we consider them to be
    /// a separate line segment.
    const MAX_POINT_DISTANCE: f64 = 5000.0;

    pub fn length(&self) -> f64 {
        self.tracks.iter().map(LineString::haversine_length).sum()
    }

    pub fn clip_to_tiles(&self, zooms: &[u8], trim_dist: f64) -> ClippedTiles {
        let mut clippers: Vec<_> = zooms.iter().map(|zoom| TileClipper::new(*zoom)).collect();

        for line in self.tracks.iter() {
            let points: Vec<_> = line
                .points()
                .map(LngLat::from)
                .filter_map(|pt| pt.xy())
                .collect();

            if points.len() < 2 {
                continue;
            }

            let first = &points[0].0;
            let last = &points[points.len() - 1].0;

            // Find points which are >= trim_dist away from start/end
            let start_idx = points
                .iter()
                .enumerate()
                .find(|(_, pt)| pt.0.euclidean_distance(first) >= trim_dist)
                .map(|(i, _)| i);

            let end_idx = points
                .iter()
                .rev()
                .enumerate()
                .find(|(_, pt)| pt.0.euclidean_distance(last) >= trim_dist)
                .map(|(i, _)| points.len() - 1 - i);

            if let Some((i, j)) = start_idx.zip(end_idx) {
                if i >= j {
                    continue;
                }

                let mut pairs = points[i..j].windows(2);
                while let Some(&[p0, p1]) = pairs.next() {
                    // Skip over large jumps
                    let len = p0.0.euclidean_distance(&p1.0);
                    if len > Self::MAX_POINT_DISTANCE {
                        continue;
                    }

                    for clip in clippers.iter_mut() {
                        clip.add_line_segment(p0, p1);
                    }
                }

                for clip in clippers.iter_mut() {
                    clip.finish_segment();
                }
            }
        }

        ClippedTiles(clippers)
    }
}

pub enum FileType {
    Gpx,
    Fit,
    Tcx,
}

pub enum CompressionType {
    None,
    Gzip,
}

pub fn read<R>(rdr: R, kind: FileType, comp: CompressionType) -> Result<Option<RawActivity>>
where
    R: Read + 'static,
{
    let mut reader: BufReader<Box<dyn Read>> = BufReader::new(match comp {
        CompressionType::None => Box::new(rdr),
        CompressionType::Gzip => Box::new(GzDecoder::new(rdr)),
    });

    match kind {
        FileType::Gpx => parse_gpx(&mut reader),
        FileType::Fit => parse_fit(&mut reader),
        // TODO: implement TCX
        FileType::Tcx => Ok(None),
    }
}

// TODO: should return a Result
pub fn read_file(p: &Path) -> Option<RawActivity> {
    let file_name = p.file_name()?;
    match get_file_type(file_name.to_str()?) {
        Some((file_type, comp)) => {
            let file = File::open(p).expect("open file");
            match read(file, file_type, comp) {
                Ok(activity) => activity,
                Err(e) => {
                    println!("Error reading {:?}: {:?}", p, e);
                    None
                }
            }
        }

        _ => None,
    }
}

fn parse_fit<R: Read>(r: &mut R) -> Result<Option<RawActivity>> {
    const SCALE_FACTOR: f64 = (1u64 << 32) as f64 / 360.0;

    let opts = [
        DecodeOption::SkipDataCrcValidation,
        DecodeOption::SkipHeaderCrcValidation,
    ]
    .into();

    let (mut start_time, mut duration_secs) = (None, None);
    let mut points = vec![];
    for data in from_reader_with_options(r, &opts)? {
        match data.kind() {
            MesgNum::FileId => {
                for f in data.fields() {
                    // Skip over virtual rides
                    // TODO: not an exhaustive check
                    if f.name() == "manufacturer" {
                        match f.value() {
                            Value::String(val) if val.as_str() == "zwift" => return Ok(None),
                            _ => {}
                        }
                    }
                }
            }
            MesgNum::Record => {
                let mut lat: Option<i64> = None;
                let mut lng: Option<i64> = None;

                for f in data.fields() {
                    match f.name() {
                        "position_lat" => lat = f.value().try_into().ok(),
                        "position_long" => lng = f.value().try_into().ok(),
                        "timestamp" => {
                            let ts: i64 = f.value().try_into()?;

                            match start_time {
                                None => start_time = Some(ts),
                                Some(t) => duration_secs = Some((ts - t) as u64),
                            }
                        }
                        _ => {}
                    }
                }

                if let (Some(lat), Some(lng)) = (lat, lng) {
                    let pt = Point::new(lng as f64, lat as f64) / SCALE_FACTOR;
                    points.push(pt);
                }
            }
            _ => {}
        }
    }

    if points.is_empty() {
        return Ok(None);
    }

    let line = points.into_iter().collect::<LineString>();
    Ok(Some(RawActivity {
        duration_secs,
        title: None,
        start_time: start_time
            .map(|ts| OffsetDateTime::from_unix_timestamp(ts).unwrap())
            .map(SqlDateTime),
        tracks: MultiLineString::from(line),
    }))
}

fn parse_gpx<R: Read>(reader: &mut R) -> Result<Option<RawActivity>> {
    let gpx = gpx::read(reader)?;

    // Just take the first track (generally the only one).
    let Some(track) = gpx.tracks.first() else {
        return Ok(None);
    };

    let start_time = gpx.metadata.and_then(|m| m.time).map(OffsetDateTime::from);

    // Grab the timestamp from the last point to calculate duration
    let end_time = track
        .segments
        .last()
        .and_then(|seg| seg.points.last())
        .and_then(|wpt| wpt.time)
        .map(|t| OffsetDateTime::from(t).unix_timestamp() as u64);

    let duration_secs = start_time
        .map(|t| t.unix_timestamp() as u64)
        .zip(end_time)
        .filter(|(start, end)| end > start)
        .map(|(start, end)| end - start);

    Ok(Some(RawActivity {
        duration_secs,
        start_time: start_time.map(SqlDateTime),
        title: track.name.clone(),
        tracks: track.multilinestring(),
    }))
}

/// Allows us to treat `bar.gpx.gz` the same as `bar.gpx`.
pub fn get_file_type(file_name: &str) -> Option<(FileType, CompressionType)> {
    let mut exts = file_name.rsplit('.');

    let (comp, ext) = match exts.next()? {
        "gz" => (CompressionType::Gzip, exts.next()?),
        ext => (CompressionType::None, ext),
    };

    match ext {
        "gpx" => Some((FileType::Gpx, comp)),
        "fit" => Some((FileType::Fit, comp)),
        "tcx" => Some((FileType::Tcx, comp)),
        _ => None,
    }
}

pub fn upsert(
    conn: &mut rusqlite::Connection,
    name: &str,
    activity: &RawActivity,
    trim_dist: f64,
) -> Result<i64> {
    let mut insert_coords = conn.prepare_cached(
        "\
        INSERT INTO activity_tiles (activity_id, z, x, y, coords) \
        VALUES (?, ?, ?, ?, ?)",
    )?;

    // TODO: The `OR REPLACE` works for activities, but we'd still end up inserting the
    //   tiles again.
    conn.execute(
        "\
        INSERT OR REPLACE \
        INTO activities (file, title, start_time, duration_secs, dist_meters)\
        VALUES (?, ?, ?, ?, ?)",
        params![
            name,
            activity.title,
            activity.start_time,
            activity.duration_secs,
            activity.length(),
        ],
    )?;

    let activity_id = conn.last_insert_rowid();

    // TODO: encode multiline strings together in same blob?
    let tiles = activity.clip_to_tiles(&DEFAULT_ZOOM_LEVELS, trim_dist);
    for (tile, line) in tiles.iter() {
        // TODO: can consider storing post rasterization for faster renders.
        let simplified = simplify(&line.0, 4.0);
        let encoded = encode_line(&simplified)?;

        insert_coords.insert(params![activity_id, tile.z, tile.x, tile.y, encoded])?;
    }

    Ok(activity_id)
}

// TODO: just reuse the thing from `geo`
/// Ramer–Douglas–Peucker algorithm
pub fn simplify(line: &[Coord<u16>], epsilon: f32) -> Vec<Coord<u16>> {
    if line.len() < 3 {
        return line.to_vec();
    }

    fn simplify_inner(line: &[Coord<u16>], epsilon: f32, buffer: &mut Vec<Coord<u16>>) {
        if let [start, rest @ .., end] = line {
            let mut max_dist = 0.0;
            let mut max_idx = 0;

            for (idx, pt) in rest.iter().enumerate() {
                let dist = point_to_line_dist(pt, start, end);
                if dist > max_dist {
                    max_dist = dist;
                    max_idx = idx + 1;
                }
            }

            if max_dist > epsilon {
                simplify_inner(&line[..=max_idx], epsilon, buffer);
                buffer.push(line[max_idx]);
                simplify_inner(&line[max_idx..], epsilon, buffer);
            }
        }
    }

    let mut buf = vec![line[0]];
    simplify_inner(line, epsilon, &mut buf);
    buf.push(line[line.len() - 1]);

    buf
}

// FIXME: casts gone mad! let's stick to a data type.
fn point_to_line_dist(pt: &Coord<u16>, start: &Coord<u16>, end: &Coord<u16>) -> f32 {
    let (sx, sy) = (start.x as f32, start.y as f32);
    let (ex, ey) = (end.x as f32, end.y as f32);
    let (px, py) = (pt.x as f32, pt.y as f32);

    let dx = ex - sx;
    let dy = ey - sy;

    // Line start and ends on same point, so just return euclidean distance to that point.
    if dx == 0.0 && dy == 0.0 {
        return (sx - px).hypot(sy - py);
    }

    let dist = (dx * (sy - py)) - (dy * (sx - px));
    dist.abs() / (dx * dx + dy * dy).sqrt()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_simplify() {
        let line = vec![
            Coord { x: 0, y: 0 },
            Coord { x: 1, y: 1 },
            Coord { x: 2, y: 2 },
            Coord { x: 3, y: 3 },
            Coord { x: 4, y: 4 },
            Coord { x: 5, y: 5 },
            Coord { x: 6, y: 6 },
            Coord { x: 7, y: 7 },
            Coord { x: 8, y: 8 },
            Coord { x: 9, y: 9 },
        ];

        let simplified = simplify(&line, 0.5);
        assert_eq!(simplified.len(), 2);
        assert_eq!(simplified[0], Coord { x: 0, y: 0 });
        assert_eq!(simplified[1], Coord { x: 9, y: 9 });
    }

    #[test]
    fn test_simplify_retains_points() {
        let line = vec![
            Coord { x: 0, y: 0 },
            Coord { x: 5, y: 5 },
            Coord { x: 0, y: 0 },
            Coord { x: 1, y: 1 },
            Coord { x: 0, y: 0 },
        ];

        let simplified = simplify(&line, 2.0);
        assert_eq!(simplified.len(), 3);
        assert_eq!(simplified[0], Coord { x: 0, y: 0 });
        assert_eq!(simplified[1], Coord { x: 5, y: 5 });
        assert_eq!(simplified[2], Coord { x: 0, y: 0 });
    }

    #[test]
    fn test_point_to_line_dist() {
        let start = Coord { x: 0, y: 0 };
        let end = Coord { x: 10, y: 10 };

        assert_eq!(point_to_line_dist(&Coord { x: 5, y: 5 }, &start, &end), 0.0);
        assert_eq!(
            point_to_line_dist(&Coord { x: 5, y: 0 }, &start, &end),
            (5.0 * 2.0_f32.sqrt()) / 2.0
        );
        assert_eq!(
            point_to_line_dist(&Coord { x: 0, y: 5 }, &start, &end),
            (5.0 * 2.0_f32.sqrt()) / 2.0
        );
        assert_eq!(
            point_to_line_dist(&Coord { x: 0, y: 10 }, &start, &end),
            (10.0 * 2.0_f32.sqrt()) / 2.0
        );
        assert_eq!(
            point_to_line_dist(&Coord { x: 10, y: 0 }, &start, &end),
            (10.0 * 2.0_f32.sqrt()) / 2.0
        );
    }

    #[test]
    fn test_point_to_line_same_point() {
        let start = Coord { x: 0, y: 0 };
        let end = Coord { x: 0, y: 0 };

        assert_eq!(point_to_line_dist(&Coord { x: 0, y: 0 }, &start, &end), 0.0);
        assert_eq!(
            point_to_line_dist(&Coord { x: 1, y: 1 }, &start, &end),
            (2_f32).sqrt()
        );
    }
}
