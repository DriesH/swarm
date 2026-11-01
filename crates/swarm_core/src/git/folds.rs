//! Finds the files of a diff that come folded by what they are: lock files and generated
//! files. A large diff is counted while the lines are read, see `Collector`.

use std::collections::HashMap;
use std::path::Path;

use swarm_protocol::Fold;

use super::{Error, execute_fed, failure};

/// Package manager lock files, by exact file name in any folder.
const LOCK_FILES: [&str; 22] = [
    "Cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "composer.lock",
    "Gemfile.lock",
    "poetry.lock",
    "Pipfile.lock",
    "uv.lock",
    "pdm.lock",
    "go.sum",
    "flake.lock",
    "Package.resolved",
    "Podfile.lock",
    "pubspec.lock",
    "mix.lock",
    "packages.lock.json",
    "gradle.lockfile",
    "deno.lock",
];

/// File name endings of minified files and source maps.
const GENERATED_ENDINGS: [&str; 6] = [
    ".min.js", ".min.mjs", ".min.css", ".js.map", ".mjs.map", ".css.map",
];

/// The files of `paths` that are a lock file or generated, and why. Reads
/// `.gitattributes` once, from the commit `source`, or from the worktree without one.
pub(super) fn marked(
    worktree: &Path,
    source: Option<&str>,
    paths: &[&[u8]],
) -> Result<HashMap<Vec<u8>, Fold>, Error> {
    if paths.is_empty() {
        return Ok(HashMap::new());
    }

    let attributes = generated_attributes(worktree, source, paths)?;

    Ok(paths
        .iter()
        .filter_map(|path| {
            let fold = fold_of(path, attributes.get(*path).copied())?;

            Some((path.to_vec(), fold))
        })
        .collect())
}

/// `generated` is the `linguist-generated` attribute: `Some(false)` turns every fold
/// off, `Some(true)` makes the file generated.
fn fold_of(path: &[u8], generated: Option<bool>) -> Option<Fold> {
    if generated == Some(false) {
        return None;
    }

    let name = path.rsplit(|byte| *byte == b'/').next().unwrap_or(path);

    if LOCK_FILES.iter().any(|lock| lock.as_bytes() == name) {
        return Some(Fold::Lock);
    }

    let by_name = GENERATED_ENDINGS
        .iter()
        .any(|ending| name.ends_with(ending.as_bytes()));

    (generated == Some(true) || by_name).then_some(Fold::Generated)
}

/// One `git check-attr` call for all paths. A path with no value is left out of the map.
fn generated_attributes(
    worktree: &Path,
    source: Option<&str>,
    paths: &[&[u8]],
) -> Result<HashMap<Vec<u8>, bool>, Error> {
    let input: Vec<u8> = paths
        .iter()
        .flat_map(|path| path.iter().copied().chain([0]))
        .collect();

    // An older git has no `--source` and fails on it. Then the attributes of the
    // worktree stand in for those of the commit.
    let printed = match source {
        Some(source) => check_attr(worktree, Some(source), input.clone())
            .or_else(|_| check_attr(worktree, None, input))?,
        None => check_attr(worktree, None, input)?,
    };

    // Triples of path, attribute and value, each ended by a NUL.
    let mut tokens = printed.split(|byte| *byte == 0);
    let mut attributes = HashMap::new();

    while let (Some(path), Some(_), Some(value)) = (tokens.next(), tokens.next(), tokens.next()) {
        match value {
            b"set" | b"true" => attributes.insert(path.to_vec(), true),
            b"unset" | b"false" => attributes.insert(path.to_vec(), false),
            _ => None,
        };
    }

    Ok(attributes)
}

fn check_attr(worktree: &Path, source: Option<&str>, input: Vec<u8>) -> Result<Vec<u8>, Error> {
    let mut arguments = vec!["check-attr".to_owned()];
    arguments.extend(source.map(|source| format!("--source={source}")));
    arguments.extend(["-z", "--stdin", "linguist-generated"].map(str::to_owned));

    let (arguments, output) = execute_fed(worktree, arguments, input)?;
    if !output.success {
        return Err(failure(&arguments, &output));
    }

    Ok(output.raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_files_are_found_by_exact_name_in_any_folder() {
        for name in LOCK_FILES {
            assert_eq!(fold_of(name.as_bytes(), None), Some(Fold::Lock), "{name}");
            assert_eq!(
                fold_of(format!("web/app/{name}").as_bytes(), None),
                Some(Fold::Lock),
                "{name} in a folder"
            );
        }

        for name in [
            "Cargo.lock.bak",
            "my-Cargo.lock",
            "cargo.lock",
            "yarn.lock/x",
        ] {
            assert_eq!(fold_of(name.as_bytes(), None), None, "{name}");
        }
    }

    #[test]
    fn minified_files_and_source_maps_are_generated() {
        for name in [
            "a.min.js",
            "dist/a.min.mjs",
            "b.min.css",
            "a.js.map",
            "a.mjs.map",
            "x/y.css.map",
        ] {
            assert_eq!(
                fold_of(name.as_bytes(), None),
                Some(Fold::Generated),
                "{name}"
            );
        }

        for name in ["a.js", "min.js.txt", "a.map", "a.minjs"] {
            assert_eq!(fold_of(name.as_bytes(), None), None, "{name}");
        }
    }

    #[test]
    fn the_attribute_decides_and_a_lock_file_stays_a_lock_file() {
        assert_eq!(fold_of(b"a.rs", Some(true)), Some(Fold::Generated));
        assert_eq!(fold_of(b"Cargo.lock", Some(true)), Some(Fold::Lock));
        assert_eq!(fold_of(b"a.min.js", Some(false)), None);
        assert_eq!(fold_of(b"Cargo.lock", Some(false)), None);
        assert_eq!(fold_of(b"a.rs", Some(false)), None);
    }
}
