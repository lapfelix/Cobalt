//! Whether the accent layer can actually be drawn, rather than only decoded.
//!
//! This is a separate test binary on purpose. The typeface is installed once
//! per process into a `OnceLock`, and the library's own tests install it as a
//! side effect of building an [`AppRunner`], so a test that needs real type
//! measured cannot tell which regime it got. Here it is the first thing that
//! happens and nothing else runs.
//!
//! Without a typeface the fallback is a 5x7 bitmap of `A`-`Z`, digits and five
//! punctuation marks that draws everything else as a blank, and measures every
//! character as the same six pixels. A layer of accents checked under that
//! fallback would pass while rendering as holes.

use kobo_sdk::keyboard::{Keyboard, Layer};
use kobo_sdk::ScreenBuilder;
use kobo_ui::{
    Chrome, DisplayMetrics, Face, LayoutIssueKind, LayoutKind, Screen, TextScale, CLARA_BW_METRICS,
};

/// The panels the keyboard has to fit. The first two are the hardware gate
/// (`kobo_profile::SUPPORTED_PROFILES`); the Nia is the fewest pixels the
/// design system is built to reach, and a row of ten keys is a division.
const PANELS: [(&str, DisplayMetrics); 3] = [
    ("clara", CLARA_BW_METRICS),
    (
        "elipsa-2e",
        DisplayMetrics {
            width: 1404,
            height: 1872,
            pixels_per_inch: 227,
            text_scale: TextScale::Default,
        },
    ),
    (
        "nia",
        DisplayMetrics {
            width: 758,
            height: 1024,
            pixels_per_inch: 212,
            text_scale: TextScale::Default,
        },
    ),
];

fn typeface() {
    let _ignored = kobo_text::install(DisplayMetrics::default());
    assert!(
        kobo_ui::has_typesetter(),
        "these tests measure and draw real type, and there is none installed"
    );
}

fn drawn(layer: Layer, shifted: bool) -> Screen {
    let mut keyboard = Keyboard::new();
    match layer {
        Layer::Letters => {}
        Layer::Symbols => {
            keyboard.press(kobo_sdk::action_id("kb.layer"));
        }
        Layer::Accents => {
            keyboard.press(kobo_sdk::action_id("kb.accents"));
        }
    }
    if shifted {
        keyboard.press(kobo_sdk::action_id("kb.shift"));
    }
    assert_eq!(keyboard.layer(), layer);
    ScreenBuilder::new("keyboard")
        .heading("Search the libraries")
        .keyboard(&keyboard, "Search")
        .build()
}

const LAYERS: [Layer; 3] = [Layer::Letters, Layer::Symbols, Layer::Accents];

/// The one that matters on the device: Atkinson Hyperlegible carries about 340
/// characters, and `Ÿ` and `Œ` are outside Latin-1. A gap here would draw a
/// blank key, which the reader reads as a broken panel rather than as a missing
/// glyph.
#[test]
fn every_key_face_has_a_glyph_to_draw_it_with() {
    typeface();
    for layer in LAYERS {
        for shifted in [false, true] {
            for node in drawn(layer, shifted).layout_for(&CLARA_BW_METRICS).nodes {
                if !matches!(node.kind, LayoutKind::CellLabel) {
                    continue;
                }
                for line in &node.text_lines {
                    assert_eq!(
                        kobo_ui::undrawable_in(line, Face::Text),
                        None,
                        "{layer:?} (shifted {shifted}): {line:?} has no glyph"
                    );
                }
            }
        }
    }
}

/// Every key face, measured in the type that will draw it, inside the cell the
/// grid gives it. Guillemets and the ligatures are wider than a letter, and the
/// accent key carries three characters where the layer key carries four.
///
/// Measured on the Clara alone, and that is not a gap. The typeface is sized
/// from the panel it was installed for, once per process, so type and cells can
/// only be compared at one density per test binary. The Clara is the strict
/// case: both a cell and a letter scale with the pixel density, so what decides
/// whether five characters fit across a key is the panel's width in
/// millimetres, and the Clara is the narrowest the SDK draws for. The Nia has
/// fewer pixels but the same physical width, which is the whole point of
/// `column_counts_follow_physical_width_rather_than_resolution`.
#[test]
fn no_key_face_overflows_its_cell() {
    typeface();
    for layer in LAYERS {
        for shifted in [false, true] {
            let issues = drawn(layer, shifted)
                .diagnostics(&CLARA_BW_METRICS, &Chrome::with_back(true))
                .issues
                .into_iter()
                .filter(|issue| {
                    matches!(
                        issue.kind,
                        LayoutIssueKind::TextOverflow
                            | LayoutIssueKind::UnsupportedCharacter { .. }
                    )
                })
                .collect::<Vec<_>>();
            assert!(
                issues.is_empty(),
                "{layer:?}, shifted {shifted}: {issues:?}"
            );
        }
    }
}

/// A key too small to hit is a key that is not there. Geometry only, so unlike
/// the face widths this one is honest on every panel: a cell's size comes from
/// the panel's millimetres and never from the typeface.
#[test]
fn every_key_is_large_enough_to_hit_on_every_panel() {
    for (name, metrics) in PANELS {
        for layer in LAYERS {
            for shifted in [false, true] {
                let issues = drawn(layer, shifted)
                    .diagnostics(&metrics, &Chrome::with_back(true))
                    .issues
                    .into_iter()
                    .filter(|issue| {
                        matches!(issue.kind, LayoutIssueKind::TouchTargetTooSmall { .. })
                    })
                    .collect::<Vec<_>>();
                assert!(
                    issues.is_empty(),
                    "{name}, {layer:?}, shifted {shifted}: {issues:?}"
                );
            }
        }
    }
}
