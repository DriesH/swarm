//! The Rust sprites against frames dumped from the generator on the design canvas.

#![expect(clippy::expect_used, reason = "test helpers panic on failure")]

use serde::Deserialize;
use swarm_creatures::{COUNT, Frame, Motion, SIDE, sprite};
use swarm_protocol::creatures::CREATURES;

#[derive(Deserialize)]
struct Fixture {
    alphabet: String,
    creatures: Vec<Expected>,
}

#[derive(Deserialize)]
struct Expected {
    slug: String,
    fly: Option<Vec<Packed>>,
    crawl: Vec<Packed>,
    still: Vec<Packed>,
}

/// One frame: a color list and 64 rows, one letter for each pixel. `.` is empty.
#[derive(Deserialize)]
struct Packed {
    palette: Vec<String>,
    rows: Vec<String>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/frames.json")).expect("the fixture is valid JSON")
}

fn color(hex: &str) -> [u8; 3] {
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).expect("a hex color");

    [channel(0), channel(2), channel(4)]
}

/// The first pixel where the frame differs from the expected one.
fn first_difference(alphabet: &str, expected: &Packed, frame: &Frame) -> Option<String> {
    for (row, line) in expected.rows.iter().enumerate() {
        for (column, letter) in line.chars().enumerate() {
            let want = match letter {
                '.' => None,
                letter => {
                    let at = alphabet.find(letter).expect("a letter of the alphabet");

                    Some(color(&expected.palette[at]))
                }
            };

            if frame.pixel(column, row) != want {
                return Some(format!(
                    "pixel {column},{row}: {:?} instead of {want:?}",
                    frame.pixel(column, row)
                ));
            }
        }
    }

    None
}

fn check(
    alphabet: &str,
    slug: &str,
    motion: Motion,
    expected: &[Packed],
    frames: &[Frame],
    differences: &mut Vec<String>,
) {
    if expected.len() != frames.len() {
        differences.push(format!(
            "{slug} {motion:?}: {} frames instead of {}",
            frames.len(),
            expected.len()
        ));

        return;
    }

    for (number, (want, frame)) in expected.iter().zip(frames).enumerate() {
        if let Some(difference) = first_difference(alphabet, want, frame) {
            differences.push(format!("{slug} {motion:?} frame {number}, {difference}"));
        }
    }
}

#[test]
fn the_fixture_lists_the_creatures_in_the_order_of_the_field_guide() {
    let fixture = fixture();
    let slugs: Vec<&str> = fixture.creatures.iter().map(|c| c.slug.as_str()).collect();
    let listed: Vec<&str> = CREATURES.iter().map(|creature| creature.slug).collect();

    assert_eq!(slugs, listed);
    assert_eq!(COUNT, CREATURES.len());
}

#[test]
fn every_frame_of_every_creature_matches_the_canvas_pixel_for_pixel() {
    let fixture = fixture();
    let mut differences = Vec::new();

    for (index, expected) in fixture.creatures.iter().enumerate() {
        let creature = sprite(index).expect("a sprite for every creature");

        if let Some(fly) = &expected.fly {
            check(
                &fixture.alphabet,
                &expected.slug,
                Motion::Fly,
                fly,
                creature.frames(Motion::Fly),
                &mut differences,
            );
        }
        check(
            &fixture.alphabet,
            &expected.slug,
            Motion::Crawl,
            &expected.crawl,
            creature.frames(Motion::Crawl),
            &mut differences,
        );
        check(
            &fixture.alphabet,
            &expected.slug,
            Motion::Still,
            &expected.still,
            creature.frames(Motion::Still),
            &mut differences,
        );
    }

    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

#[test]
fn only_the_velvet_ant_has_no_wings() {
    let wingless: Vec<&str> = CREATURES
        .iter()
        .enumerate()
        .filter(|(index, _)| !sprite(*index).expect("a sprite").flies())
        .map(|(_, creature)| creature.slug)
        .collect();

    assert_eq!(wingless, ["mutilla-europaea"]);
}

#[test]
fn a_creature_without_wings_crawls_where_it_should_fly() {
    let velvet_ant = CREATURES
        .iter()
        .position(|creature| creature.slug == "mutilla-europaea")
        .expect("in the list");
    let sprite = sprite(velvet_ant).expect("a sprite");

    assert_eq!(sprite.motion(Motion::Fly), Motion::Crawl);
    assert_eq!(sprite.frames(Motion::Fly).len(), 4);
}

#[test]
fn frames_per_motion_are_two_four_and_one() {
    let bee = sprite(0).expect("a sprite");

    assert_eq!(bee.frames(Motion::Fly).len(), 2);
    assert_eq!(bee.frames(Motion::Crawl).len(), 4);
    assert_eq!(bee.frames(Motion::Still).len(), 1);
}

#[test]
fn there_is_no_sprite_past_the_last_creature() {
    assert!(sprite(COUNT).is_none());
    assert_eq!(SIDE, 64);
}

#[test]
fn runs_cover_the_filled_pixels_of_a_row() {
    let bee = sprite(0).expect("a sprite");
    let frame = &bee.frames(Motion::Still)[0];

    for row in 0..SIDE {
        let covered: usize = frame.runs(row).iter().map(|run| run.length).sum();
        let filled = (0..SIDE).filter(|&c| frame.pixel(c, row).is_some()).count();

        assert_eq!(covered, filled, "row {row}");
    }
}
