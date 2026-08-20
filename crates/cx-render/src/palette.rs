//! The palette atlas (S12/M1).
//!
//! Meshes carry a **slot** per vertex and instances carry a **row**. The colour
//! is whatever sits at that intersection of a small shared texture. Nothing
//! carries a material.
//!
//! # Why this shape
//!
//! S12's consequence, stated directly: *one material, one pipeline, one bind
//! group for the vast majority of scene geometry.* A thousand differently
//! coloured objects are one draw call, because the only thing that differs
//! between them is an integer in the instance buffer.
//!
//! The two axes are not interchangeable. The slot is a property of the *mesh* —
//! which part of a thing this triangle belongs to, its roof or its walls — and
//! the row is a property of the *instance*, which variant of the thing this
//! particular one is. Collapsing them into one index would mean a mesh could
//! only ever have one colour, or every instance of a mesh the same one.
//!
//! # Looked up, not sampled
//!
//! The shader uses `textureLoad` with integer coordinates rather than a sampler.
//! A palette must never blend between entries: filtering across a slot boundary
//! would produce a colour that is in no palette at all, and the artefact appears
//! as a thin seam along a mesh's material edges that no amount of staring at the
//! texture explains.
//!
//! This is hand-authored, per M1. The asset pipeline that generates one is M3.

/// Colour slots per row. A mesh's vertex slot indexes this axis.
pub const SLOTS: u32 = 8;

/// Rows in the atlas. An instance's palette row indexes this axis.
pub const ROWS: u32 = 8;

/// A colour, as it is stored.
pub type Colour = [u8; 4];

/// A small shared colour table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    entries: Vec<Colour>,
}

impl Palette {
    /// An all-black palette.
    pub fn empty() -> Self {
        Self {
            entries: vec![[0, 0, 0, 255]; (SLOTS * ROWS) as usize],
        }
    }

    /// Sets one entry.
    ///
    /// Out-of-range coordinates are ignored rather than wrapping. Wrapping would
    /// silently overwrite a different row's colour, and the symptom — one
    /// variant of a building turning the wrong colour — is a long way from the
    /// line that caused it.
    pub fn set(&mut self, slot: u32, row: u32, colour: Colour) -> &mut Self {
        if slot < SLOTS
            && row < ROWS
            && let Some(entry) = self.entries.get_mut((row * SLOTS + slot) as usize)
        {
            *entry = colour;
        }
        self
    }

    /// Reads one entry, or black if out of range.
    pub fn get(&self, slot: u32, row: u32) -> Colour {
        if slot >= SLOTS || row >= ROWS {
            return [0, 0, 0, 255];
        }
        self.entries
            .get((row * SLOTS + slot) as usize)
            .copied()
            .unwrap_or([0, 0, 0, 255])
    }

    /// The raw bytes, row-major, as the texture upload wants them.
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.entries)
    }

    /// The hand-authored default (M1).
    ///
    /// Four rows of a three-slot scheme — top, side, and shadowed underside —
    /// which is enough for the placeholder geometry to read as lit solid objects
    /// in a handful of distinguishable colours. Slot 0 is the lit top, 1 the
    /// side, 2 the underside; the rest of each row repeats the side so an
    /// out-of-range slot in a future mesh reads as a plausible colour rather
    /// than as black.
    pub fn placeholder() -> Self {
        let mut palette = Self::empty();

        // Row, then (top, side, under). Chosen to stay distinguishable in a
        // readback: each row differs in more than brightness, so a test can tell
        // them apart without depending on the lighting term.
        const SCHEME: [(u32, Colour, Colour, Colour); 4] = [
            (
                0,
                [196, 200, 208, 255],
                [128, 134, 148, 255],
                [72, 76, 88, 255],
            ),
            (
                1,
                [214, 158, 96, 255],
                [150, 104, 60, 255],
                [84, 58, 34, 255],
            ),
            (
                2,
                [120, 190, 130, 255],
                [78, 132, 88, 255],
                [44, 74, 50, 255],
            ),
            (
                3,
                [110, 150, 220, 255],
                [72, 100, 154, 255],
                [40, 56, 88, 255],
            ),
        ];

        for (row, top, side, under) in SCHEME {
            palette.set(0, row, top);
            palette.set(1, row, side);
            palette.set(2, row, under);
            for slot in 3..SLOTS {
                palette.set(slot, row, side);
            }
        }

        palette
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::placeholder()
    }
}

/// The palette as a GPU texture and its bind group.
pub(crate) struct PaletteTexture {
    bind_group: wgpu::BindGroup,
}

impl PaletteTexture {
    /// Uploads a palette.
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        palette: &Palette,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cx-render palette"),
            size: wgpu::Extent3d {
                width: SLOTS,
                height: ROWS,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Unorm, not Srgb. The shader multiplies the palette by a lighting
            // term and writes to an sRGB target, which converts on write — so an
            // sRGB source would apply the curve twice and every colour would
            // come out washed out.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            palette.as_bytes(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SLOTS * 4),
                rows_per_image: Some(ROWS),
            },
            wgpu::Extent3d {
                width: SLOTS,
                height: ROWS,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            bind_group: device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cx-render palette bind group"),
                layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            }),
        }
    }

    /// The bind group, for group 1 of the scene pipeline.
    pub(crate) const fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// The layout the scene pipeline and this texture share.
    ///
    /// No sampler: the shader uses `textureLoad`, so there is nothing to
    /// configure and nothing that can be configured to filter across entries.
    pub(crate) fn layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cx-render palette layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        })
    }
}

impl std::fmt::Debug for PaletteTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaletteTexture").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_round_trips() {
        let mut palette = Palette::empty();
        palette.set(3, 5, [1, 2, 3, 4]);

        assert_eq!(palette.get(3, 5), [1, 2, 3, 4]);
        assert_eq!(
            palette.get(5, 3),
            [0, 0, 0, 255],
            "the axes are not symmetric"
        );
    }

    #[test]
    fn out_of_range_writes_are_dropped_rather_than_wrapping() {
        // Wrapping would silently overwrite a different row, and the symptom —
        // one variant of a thing turning the wrong colour — is a long way from
        // the line that caused it.
        let mut palette = Palette::empty();
        palette.set(SLOTS, 0, [255, 0, 0, 255]);
        palette.set(0, ROWS, [255, 0, 0, 255]);

        for row in 0..ROWS {
            for slot in 0..SLOTS {
                assert_eq!(
                    palette.get(slot, row),
                    [0, 0, 0, 255],
                    "({slot}, {row}) was written by an out-of-range set"
                );
            }
        }
    }

    #[test]
    fn out_of_range_reads_are_black_rather_than_a_panic() {
        let palette = Palette::placeholder();
        assert_eq!(palette.get(SLOTS + 10, 0), [0, 0, 0, 255]);
        assert_eq!(palette.get(0, ROWS + 10), [0, 0, 0, 255]);
    }

    #[test]
    fn the_byte_layout_is_row_major_and_complete() {
        // The upload copies this straight into a texture. A short buffer is a
        // validation error; a wrongly ordered one is a diagram of the palette
        // transposed, which looks like the mesh's slots being wrong.
        let mut palette = Palette::empty();
        palette.set(1, 0, [10, 20, 30, 40]);

        let bytes = palette.as_bytes();
        assert_eq!(bytes.len(), (SLOTS * ROWS * 4) as usize);
        assert_eq!(&bytes[4..8], &[10, 20, 30, 40], "slot 1 of row 0 is second");
    }

    #[test]
    fn the_placeholder_rows_are_distinguishable() {
        // A test that reads pixels back needs to tell rows apart by more than
        // brightness, because the lighting term also changes brightness.
        let palette = Palette::placeholder();

        for row in 0..4 {
            let colour = palette.get(0, row);
            for other in 0..4 {
                if other == row {
                    continue;
                }
                let against = palette.get(0, other);
                let difference = (0..3)
                    .map(|channel| i32::from(colour[channel]) - i32::from(against[channel]))
                    .map(i32::abs)
                    .sum::<i32>();

                assert!(
                    difference > 60,
                    "rows {row} and {other} are too close to tell apart: \
                     {colour:?} against {against:?}"
                );
            }
        }
    }

    #[test]
    fn every_slot_of_a_used_row_has_a_colour() {
        // An unset slot reads as black, which looks like a hole in the geometry
        // rather than like a missing palette entry.
        let palette = Palette::placeholder();
        for row in 0..4 {
            for slot in 0..SLOTS {
                assert_ne!(
                    palette.get(slot, row),
                    [0, 0, 0, 255],
                    "slot {slot} of row {row} is unset"
                );
            }
        }
    }

    #[test]
    fn a_top_is_lighter_than_its_underside() {
        // The convention the mesh's slot assignment depends on. If it inverted,
        // objects would read as lit from below.
        let palette = Palette::placeholder();
        for row in 0..4 {
            let top: u32 = palette.get(0, row)[..3].iter().map(|c| u32::from(*c)).sum();
            let under: u32 = palette.get(2, row)[..3].iter().map(|c| u32::from(*c)).sum();
            assert!(top > under, "row {row} is lit from below");
        }
    }
}
