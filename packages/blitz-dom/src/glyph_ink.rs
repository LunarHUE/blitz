//! Glyph ink: the exact bounds of each glyph's unhinted outline, read from
//! the font and cached across frames. Ink overflow uses it to bound text,
//! and paint can share it.
//!
//! Adapted from the outline-bounds cache in the simplify hint
//! (blitz-paint `simplify.rs`, LunarHUE/blitz#11).

use std::collections::HashMap;

use crate::node::TextBrush;

/// A glyph's ink in font units at a run's variation coordinates: its
/// outline's exact bounds, y up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GlyphInk {
    /// `[x0, y0, x1, y1]`.
    Box([f32; 4]),
    /// No outline to paint, as for a space.
    Empty,
    /// No outline could be read, as for a bitmap glyph.
    Unknown,
}

/// Font, face index and variation coordinates, hashed.
type FaceKey = (u64, u32, u64);

/// Ink of every glyph seen, kept across frames so a zoom reads it from
/// here rather than from the font, up to this many glyphs.
const INK_CACHE_LIMIT: usize = 1 << 16;

/// Units per em of every face seen, up to this many faces.
const UPEM_CACHE_LIMIT: usize = 1 << 10;

/// Inserts into a cache of at most `limit` entries, emptying it first when
/// full. Fonts and documents come and go, so both caches drop everything
/// rather than track what is still in use.
fn bounded_insert<K: std::hash::Hash + Eq, V>(
    cache: &mut HashMap<K, V>,
    limit: usize,
    key: K,
    value: V,
) {
    if cache.len() >= limit && !cache.contains_key(&key) {
        cache.clear();
    }
    cache.insert(key, value);
}

thread_local! {
    static INK_CACHE: std::cell::RefCell<HashMap<(FaceKey, u32), GlyphInk>> =
        std::cell::RefCell::new(HashMap::new());
    static UPEM_CACHE: std::cell::RefCell<HashMap<(u64, u32), u16>> =
        std::cell::RefCell::new(HashMap::new());
}

/// The units per em of the face `id`, read with `read` when not cached.
fn units_per_em(id: (u64, u32), read: impl FnOnce() -> u16) -> u16 {
    if let Some(upem) = UPEM_CACHE.with(|c| c.borrow().get(&id).copied()) {
        return upem;
    }
    let upem = read();
    UPEM_CACHE.with(|c| bounded_insert(&mut c.borrow_mut(), UPEM_CACHE_LIMIT, id, upem));
    upem
}

/// A run's face, read from the font only for glyphs not yet cached.
pub struct Face<'a> {
    key: FaceKey,
    data: &'a [u8],
    index: u32,
    coords: &'a [i16],
    units_per_em: u16,
}

impl<'a> Face<'a> {
    pub fn of(run: &'a parley::Run<'a, TextBrush>) -> Face<'a> {
        use std::hash::{Hash as _, Hasher as _};
        let font = run.font();
        let coords: &[i16] = bytemuck::cast_slice(run.normalized_coords());
        let mut h = std::collections::hash_map::DefaultHasher::new();
        coords.hash(&mut h);
        let id = (font.data.id(), font.index);
        let units_per_em = units_per_em(id, || {
            use skrifa::raw::TableProvider as _;
            skrifa::FontRef::from_index(font.data.as_ref(), font.index)
                .ok()
                .and_then(|f| f.head().ok())
                .map_or(0, |head| head.units_per_em())
        });
        Face {
            key: (id.0, id.1, h.finish()),
            data: font.data.as_ref(),
            index: font.index,
            coords,
            units_per_em,
        }
    }

    /// Zero when the face has no readable `head` table.
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    pub fn ink(&self, glyph: u32) -> GlyphInk {
        if self.units_per_em == 0 {
            return GlyphInk::Unknown;
        }
        let key = (self.key, glyph);
        if let Some(ink) = INK_CACHE.with(|c| c.borrow().get(&key).copied()) {
            return ink;
        }
        let ink = self.read(glyph);
        INK_CACHE.with(|c| bounded_insert(&mut c.borrow_mut(), INK_CACHE_LIMIT, key, ink));
        ink
    }

    fn read(&self, glyph: u32) -> GlyphInk {
        use skrifa::MetadataProvider as _;
        use skrifa::instance::{LocationRef, Size};
        use skrifa::outline::DrawSettings;
        let Ok(font) = skrifa::FontRef::from_index(self.data, self.index) else {
            return GlyphInk::Unknown;
        };
        let Some(outline) = font.outline_glyphs().get(skrifa::GlyphId::new(glyph)) else {
            return GlyphInk::Unknown;
        };
        let coords: Vec<skrifa::raw::types::F2Dot14> = self
            .coords
            .iter()
            .map(|&c| skrifa::raw::types::F2Dot14::from_bits(c))
            .collect();
        let mut pen = PathPen(kurbo::BezPath::new());
        let settings = DrawSettings::unhinted(Size::unscaled(), LocationRef::new(&coords));
        if outline.draw(settings, &mut pen).is_err() {
            return GlyphInk::Unknown;
        }
        if pen.0.elements().is_empty() {
            return GlyphInk::Empty;
        }
        // Exact: kurbo bounds curves by their extrema, not control points.
        let b = kurbo::Shape::bounding_box(&pen.0);
        if b.width() <= 0.0 || b.height() <= 0.0 {
            return GlyphInk::Empty;
        }
        GlyphInk::Box([b.x0 as f32, b.y0 as f32, b.x1 as f32, b.y1 as f32])
    }
}

struct PathPen(kurbo::BezPath);

impl skrifa::outline::OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to((f64::from(x), f64::from(y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to((f64::from(x), f64::from(y)));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.quad_to(
            (f64::from(cx0), f64::from(cy0)),
            (f64::from(x), f64::from(y)),
        );
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.curve_to(
            (f64::from(cx0), f64::from(cy0)),
            (f64::from(cx1), f64::from(cy1)),
            (f64::from(x), f64::from(y)),
        );
    }
    fn close(&mut self) {
        self.0.close_path();
    }
}
