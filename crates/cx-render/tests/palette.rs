//! The palette atlas, in pixels (S12/M1).
//!
//! S12's claim is not that a palette produces colours — anything does. It is
//! that **one draw call** produces differently coloured objects, because the
//! only thing distinguishing them is an integer in the instance buffer. That is
//! what these check: the colours differ, the draw-call count stays at one, and
//! the colours are the ones the palette actually contains.

use cx_core::math::{Quat, Vec3};
use cx_render::testing::device_or_skip;
use cx_render::{Camera, InstancedRenderer, MeshData, Palette, Readback, Rgba};
use cx_view::ExtractedInstance;

/// Size of the readback target.
const SIZE: u32 = 96;

/// A dark clear colour, so anything drawn stands out from it.
const CLEAR: Rgba = [0.02, 0.02, 0.03, 1.0];

fn instance(position: Vec3, palette: u32) -> ExtractedInstance {
    ExtractedInstance {
        position,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
        palette,
    }
}

/// Renders instances head-on and returns the frame.
fn render(instances: &[ExtractedInstance]) -> Option<(Readback, u32)> {
    let device = device_or_skip()?;
    let mut renderer = InstancedRenderer::new(&device, &MeshData::unit_cube())
        .expect("the unit cube is a valid mesh");

    // Slightly above, so the top face — palette slot 0 — is visible along with
    // the sides. A dead-on view would only ever exercise one slot.
    let camera = Camera::looking_at(Vec3::new(0.0, 1.2, 4.0), Vec3::new(0.0, 0.0, 0.0));

    let (readback, stats) = renderer
        .render(&device, SIZE, SIZE, &camera, instances, CLEAR)
        .expect("the frame should render");

    Some((readback, stats.draw_calls))
}

/// The most saturated pixel in a column band, which is the cube's own colour
/// rather than the background.
fn dominant(readback: &Readback, from_x: u32, to_x: u32) -> [u8; 4] {
    let mut best = [0u8; 4];
    let mut best_sum = 0u32;

    for y in 0..SIZE {
        for x in from_x..to_x {
            let pixel = readback.pixel(x, y).expect("in bounds");
            let sum = u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2]);
            if sum > best_sum {
                best_sum = sum;
                best = pixel;
            }
        }
    }

    best
}

/// Writes a frame out as a PPM when `CX_DUMP_FRAME` names a directory.
///
/// Pixel assertions say the colours *differ* and come from the palette. They
/// cannot say the result looks right, and looking at rendered output has caught
/// things in this project that reasoning about it did not. PPM because it needs
/// no encoder — `sips` and every image viewer read it.
fn dump(readback: &Readback, name: &str) {
    let Ok(directory) = std::env::var("CX_DUMP_FRAME") else {
        return;
    };

    let mut ppm = format!("P6\n{SIZE} {SIZE}\n255\n").into_bytes();
    for y in 0..SIZE {
        for x in 0..SIZE {
            let pixel = readback.pixel(x, y).unwrap_or([0, 0, 0, 255]);
            ppm.extend_from_slice(&pixel[..3]);
        }
    }

    let path = std::path::Path::new(&directory).join(format!("{name}.ppm"));
    if let Err(error) = std::fs::write(&path, ppm) {
        eprintln!("could not write {}: {error}", path.display());
    }
}

/// **The claim S12 makes:** different colours, one draw call.
#[test]
fn instances_with_different_palette_rows_differ_in_one_draw_call() {
    let Some((readback, draw_calls)) = render(&[
        instance(Vec3::new(-1.1, 0.0, 0.0), 1),
        instance(Vec3::new(1.1, 0.0, 0.0), 3),
    ]) else {
        return;
    };

    assert_eq!(
        draw_calls, 1,
        "two colours must not cost two draw calls — that is the whole technique"
    );

    dump(&readback, "two_rows");

    let left = dominant(&readback, 0, SIZE / 2);
    let right = dominant(&readback, SIZE / 2, SIZE);

    // Row 1 is warm, row 3 is blue. Comparing the red-versus-blue balance rather
    // than absolute values, because the lighting term scales everything.
    let warmth = |pixel: [u8; 4]| i32::from(pixel[0]) - i32::from(pixel[2]);

    assert!(warmth(left) > 20, "row 1 should read warm, got {left:?}");
    assert!(warmth(right) < -20, "row 3 should read cool, got {right:?}");
}

/// The same instance drawn with different rows produces different pixels.
///
/// The test above could pass if the two cubes differed for some reason other
/// than the palette — a lighting difference from their positions, say. This one
/// changes nothing but the row.
#[test]
fn changing_only_the_row_changes_the_colour() {
    let Some((first, _)) = render(&[instance(Vec3::ZERO, 1)]) else {
        return;
    };
    let Some((second, _)) = render(&[instance(Vec3::ZERO, 2)]) else {
        return;
    };

    let a = dominant(&first, 0, SIZE);
    let b = dominant(&second, 0, SIZE);

    let difference: i32 = (0..3)
        .map(|channel| (i32::from(a[channel]) - i32::from(b[channel])).abs())
        .sum();

    assert!(
        difference > 40,
        "the same cube in two palette rows produced {a:?} and {b:?}"
    );
}

/// The colour drawn is one the palette actually contains.
///
/// Without this, the two tests above would pass against a shader that derived a
/// colour from the row index arithmetically and never read the texture at all.
#[test]
fn the_drawn_colour_comes_from_the_palette() {
    const ROW: u32 = 2;

    let Some((readback, _)) = render(&[instance(Vec3::ZERO, ROW)]) else {
        return;
    };

    let drawn = dominant(&readback, 0, SIZE);
    let palette = Palette::placeholder();

    // The brightest pixel is the top face, which is slot 0, modulated by the
    // lighting term. So the *hue* should match slot 0 of this row even though
    // the brightness will not.
    let expected = palette.get(0, ROW);

    let ratio = |pixel: [u8; 4], channel: usize| {
        let total = u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2]);
        if total == 0 {
            return 0.0;
        }
        f64::from(pixel[channel]) / f64::from(total)
    };

    for channel in 0..3 {
        let drawn_share = ratio(drawn, channel);
        let expected_share = ratio(expected, channel);
        assert!(
            (drawn_share - expected_share).abs() < 0.06,
            "channel {channel}: drawn {drawn:?} is not the hue of palette entry {expected:?}"
        );
    }
}

/// A row past the end of the palette draws black rather than reading garbage.
///
/// `textureLoad` with an out-of-range coordinate is defined to return zero in
/// WGSL, which is the behaviour worth relying on — the alternative would be
/// clamping into a real entry, and a bad row index would then be invisible.
#[test]
fn an_out_of_range_row_is_visibly_wrong_rather_than_plausible() {
    let Some((readback, draw_calls)) = render(&[instance(Vec3::ZERO, 999)]) else {
        return;
    };

    assert_eq!(draw_calls, 1, "it still draws");

    let drawn = dominant(&readback, 0, SIZE);
    let brightness = u32::from(drawn[0]) + u32::from(drawn[1]) + u32::from(drawn[2]);
    let background = readback.pixel(1, 1).expect("in bounds");
    let background_brightness =
        u32::from(background[0]) + u32::from(background[1]) + u32::from(background[2]);

    assert!(
        brightness <= background_brightness + 30,
        "a bad palette row drew something plausible ({drawn:?}) instead of black"
    );
}

/// A mesh's slots and an instance's row are different axes.
///
/// The cube's top and sides use different slots, so a single instance in a
/// single row must still show more than one colour. Collapsing the two axes into
/// one index would make a mesh single-coloured, and nothing else here would
/// notice.
#[test]
fn one_instance_shows_more_than_one_slot() {
    let Some((readback, _)) = render(&[instance(Vec3::ZERO, 1)]) else {
        return;
    };

    dump(&readback, "one_instance");

    let palette = Palette::placeholder();
    let top = palette.get(0, 1);
    let side = palette.get(1, 1);

    // The two differ in the palette, so if both are on screen the frame contains
    // at least two distinguishable non-background colours.
    assert_ne!(
        top, side,
        "the fixture needs two distinct slots to be a test"
    );

    let mut distinct: Vec<[u8; 4]> = Vec::new();
    for y in 0..SIZE {
        for x in 0..SIZE {
            let pixel = readback.pixel(x, y).expect("in bounds");
            let brightness = u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2]);
            if brightness < 40 {
                continue;
            }
            if !distinct
                .iter()
                .any(|seen: &[u8; 4]| (0..3).all(|c| seen[c].abs_diff(pixel[c]) < 12))
            {
                distinct.push(pixel);
            }
        }
    }

    assert!(
        distinct.len() >= 2,
        "one cube should show at least a top and a side, found {} distinct colours",
        distinct.len()
    );
}
