//! The bees and wasps whose Latin names Swarm gives to tasks without a name.
//!
//! Lives in the protocol crate so the daemon and the app share one list.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher as _, Hasher as _};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Common,
    Rare,
    UltraRare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Creature {
    pub english: &'static str,
    /// The binomial. The worker, drone and queen honey bee share it.
    pub latin: &'static str,
    /// Lowercase Latin name with `-`; unique, and a valid branch name part.
    pub slug: &'static str,
    pub tier: Tier,
    pub wikipedia: &'static str,
}

const fn creature(
    english: &'static str,
    latin: &'static str,
    slug: &'static str,
    tier: Tier,
    wikipedia: &'static str,
) -> Creature {
    Creature {
        english,
        latin,
        slug,
        tier,
        wikipedia,
    }
}

pub const CREATURES: [Creature; 20] = [
    creature(
        "Western honey bee",
        "Apis mellifera",
        "apis-mellifera",
        Tier::Common,
        "https://en.wikipedia.org/wiki/Western_honey_bee",
    ),
    creature(
        "Honey bee drone",
        "Apis mellifera",
        "apis-mellifera-drone",
        Tier::Common,
        "https://en.wikipedia.org/wiki/Drone_(bee)",
    ),
    creature(
        "Buff-tailed bumblebee",
        "Bombus terrestris",
        "bombus-terrestris",
        Tier::Common,
        "https://en.wikipedia.org/wiki/Bombus_terrestris",
    ),
    creature(
        "Violet carpenter bee",
        "Xylocopa violacea",
        "xylocopa-violacea",
        Tier::Common,
        "https://en.wikipedia.org/wiki/Xylocopa_violacea",
    ),
    creature(
        "Red mason bee",
        "Osmia bicornis",
        "osmia-bicornis",
        Tier::Common,
        "https://en.wikipedia.org/wiki/Osmia_bicornis",
    ),
    creature(
        "Patchwork leafcutter bee",
        "Megachile centuncularis",
        "megachile-centuncularis",
        Tier::Common,
        "https://en.wikipedia.org/wiki/Megachile_centuncularis",
    ),
    creature(
        "Orange-legged furrow bee",
        "Halictus rubicundus",
        "halictus-rubicundus",
        Tier::Common,
        "https://en.wikipedia.org/wiki/Halictus_rubicundus",
    ),
    creature(
        "Tawny mining bee",
        "Andrena fulva",
        "andrena-fulva",
        Tier::Common,
        "https://en.wikipedia.org/wiki/Andrena_fulva",
    ),
    creature(
        "Common wasp",
        "Vespula vulgaris",
        "vespula-vulgaris",
        Tier::Common,
        "https://en.wikipedia.org/wiki/Vespula_vulgaris",
    ),
    creature(
        "European paper wasp",
        "Polistes dominula",
        "polistes-dominula",
        Tier::Common,
        "https://en.wikipedia.org/wiki/Polistes_dominula",
    ),
    creature(
        "Blue-banded bee",
        "Amegilla cingulata",
        "amegilla-cingulata",
        Tier::Rare,
        "https://en.wikipedia.org/wiki/Amegilla_cingulata",
    ),
    creature(
        "Green orchid bee",
        "Euglossa dilemma",
        "euglossa-dilemma",
        Tier::Rare,
        "https://en.wikipedia.org/wiki/Euglossa_dilemma",
    ),
    creature(
        "Ruby-tailed wasp",
        "Chrysis ignita",
        "chrysis-ignita",
        Tier::Rare,
        "https://en.wikipedia.org/wiki/Chrysis_ignita",
    ),
    creature(
        "Black-and-yellow mud dauber",
        "Sceliphron caementarium",
        "sceliphron-caementarium",
        Tier::Rare,
        "https://en.wikipedia.org/wiki/Sceliphron_caementarium",
    ),
    creature(
        "European hornet",
        "Vespa crabro",
        "vespa-crabro",
        Tier::Rare,
        "https://en.wikipedia.org/wiki/European_hornet",
    ),
    creature(
        "Large velvet ant",
        "Mutilla europaea",
        "mutilla-europaea",
        Tier::Rare,
        "https://en.wikipedia.org/wiki/Mutilla_europaea",
    ),
    creature(
        "Queen bee",
        "Apis mellifera",
        "apis-mellifera-regina",
        Tier::UltraRare,
        "https://en.wikipedia.org/wiki/Queen_bee",
    ),
    creature(
        "Asian giant hornet",
        "Vespa mandarinia",
        "vespa-mandarinia",
        Tier::UltraRare,
        "https://en.wikipedia.org/wiki/Vespa_mandarinia",
    ),
    creature(
        "Tarantula hawk",
        "Pepsis grossa",
        "pepsis-grossa",
        Tier::UltraRare,
        "https://en.wikipedia.org/wiki/Pepsis_grossa",
    ),
    creature(
        "Wallace's giant bee",
        "Megachile pluto",
        "megachile-pluto",
        Tier::UltraRare,
        "https://en.wikipedia.org/wiki/Megachile_pluto",
    ),
];

/// Whether `name` is the slug of a creature.
pub fn is_creature_slug(name: &str) -> bool {
    CREATURES.iter().any(|creature| creature.slug == name)
}

/// A number that differs from run to run, for picking a creature.
pub fn random_seed() -> u64 {
    RandomState::new().build_hasher().finish()
}

/// `seed` as an index below `len`; 0 when `len` is 0.
fn index(seed: u64, len: usize) -> usize {
    let len = u64::try_from(len).unwrap_or(u64::MAX).max(1);

    usize::try_from(seed % len).unwrap_or(0)
}

/// The slug `seed` picks among the creatures that `taken` does not refuse. When all are
/// taken, the pick gets `-2`, `-3`, … until `taken` accepts it.
pub fn pick_slug<E>(
    seed: u64,
    mut taken: impl FnMut(&str) -> Result<bool, E>,
) -> Result<String, E> {
    let mut free = Vec::new();
    for creature in &CREATURES {
        if !taken(creature.slug)? {
            free.push(creature.slug);
        }
    }

    if let Some(slug) = free.get(index(seed, free.len())) {
        return Ok((*slug).to_owned());
    }

    let base = CREATURES[index(seed, CREATURES.len())].slug;
    let mut number = 2_u32;
    let mut candidate = format!("{base}-{number}");
    while taken(&candidate)? {
        number += 1;
        candidate = format!("{base}-{number}");
    }

    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::convert::Infallible;

    use super::*;

    fn pick(seed: u64, used: &[&str]) -> String {
        pick_slug(seed, |slug| Ok::<_, Infallible>(used.contains(&slug))).unwrap_or_default()
    }

    #[test]
    fn there_are_twenty_creatures_with_unique_slugs() {
        let slugs: HashSet<&str> = CREATURES.iter().map(|creature| creature.slug).collect();

        assert_eq!(slugs.len(), 20);
    }

    #[test]
    fn the_tiers_hold_ten_six_and_four() {
        let count = |tier| CREATURES.iter().filter(|c| c.tier == tier).count();

        assert_eq!(
            (
                count(Tier::Common),
                count(Tier::Rare),
                count(Tier::UltraRare)
            ),
            (10, 6, 4)
        );
    }

    #[test]
    fn a_slug_is_the_latin_name_in_lowercase_with_dashes() {
        for creature in &CREATURES {
            let base = creature.latin.to_lowercase().replace(' ', "-");

            assert!(creature.slug.starts_with(&base), "{}", creature.slug);
        }
    }

    #[test]
    fn a_slug_is_a_valid_branch_name_part() {
        for creature in &CREATURES {
            assert!(
                creature
                    .slug
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-'),
                "{}",
                creature.slug
            );
        }
    }

    #[test]
    fn every_creature_links_to_its_english_wikipedia_page() {
        for creature in &CREATURES {
            assert!(
                creature
                    .wikipedia
                    .starts_with("https://en.wikipedia.org/wiki/"),
                "{}",
                creature.english
            );
        }
    }

    #[test]
    fn the_pick_is_a_creature_slug() {
        assert!(is_creature_slug(&pick(7, &[])));
    }

    #[test]
    fn the_pick_skips_taken_slugs() {
        let used: Vec<&str> = CREATURES.iter().skip(1).map(|c| c.slug).collect();

        for seed in 0..40 {
            assert_eq!(pick(seed, &used), CREATURES[0].slug);
        }
    }

    #[test]
    fn different_seeds_pick_different_creatures() {
        let picks: HashSet<String> = (0..20).map(|seed| pick(seed, &[])).collect();

        assert_eq!(picks.len(), 20);
    }

    #[test]
    fn when_all_are_taken_a_number_is_added() {
        let mut used: Vec<String> = CREATURES.iter().map(|c| c.slug.to_owned()).collect();
        let first = pick_slug(3, |slug| {
            Ok::<_, Infallible>(used.iter().any(|used| used == slug))
        })
        .unwrap_or_default();
        used.push(first.clone());
        let second = pick_slug(3, |slug| {
            Ok::<_, Infallible>(used.iter().any(|used| used == slug))
        })
        .unwrap_or_default();

        assert_eq!(
            (first.ends_with("-2"), second.ends_with("-3")),
            (true, true)
        );
    }

    #[test]
    fn a_taken_check_that_fails_fails_the_pick() {
        let result = pick_slug(0, |_| Err::<bool, _>("git failed"));

        assert_eq!(result, Err("git failed"));
    }
}
