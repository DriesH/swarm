//! File-type icons from vscode-icons (icons CC BY-SA 4.0), unchanged, in
//! `assets/file-icons`. `assets/file-icons/LICENSE.md` lists every file and why it is not a
//! logo. Product and company logos are left out: a file with only a logo icon in vscode-icons
//! gets the default file icon.
//!
//! The name tables come from vscode-icons' `supportedExtensions.ts` and
//! `supportedFolders.ts`, restricted to the icons that are in the assets.

use crate::theme::ThemeKind;

macro_rules! file_icons {
    ($($variant:ident => $dark:literal $(, $light:literal)?;)+) => {
        /// A file-type icon. Pick one with [`FileIconName::for_file`] or
        /// [`FileIconName::for_folder`].
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum FileIconName {
            $($variant,)+
        }

        impl FileIconName {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// The SVG file in `assets/file-icons` for a dark theme, without `.svg`.
            pub fn asset(self) -> &'static str {
                match self {
                    $(Self::$variant => $dark,)+
                }
            }

            /// The SVG file for a light theme, when the icon has its own.
            pub fn light_asset(self) -> Option<&'static str> {
                match self {
                    $(Self::$variant => file_icons!(@light_asset $($light)?),)+
                }
            }

            fn dark_svg(self) -> &'static str {
                match self {
                    $(Self::$variant => include_str!(
                        concat!("../assets/file-icons/", $dark, ".svg")
                    ),)+
                }
            }

            fn light_svg(self) -> Option<&'static str> {
                match self {
                    $(Self::$variant => file_icons!(@light_svg $($light)?),)+
                }
            }
        }
    };
    (@light_asset) => { None };
    (@light_asset $light:literal) => { Some($light) };
    (@light_svg) => { None };
    (@light_svg $light:literal) => {
        Some(include_str!(concat!("../assets/file-icons/", $light, ".svg")))
    };
}

file_icons! {
    File => "default_file";
    Audio => "file_type_audio";
    Binary => "file_type_binary";
    Cert => "file_type_cert";
    Config => "file_type_config", "file_type_light_config";
    Csv => "file_type_csv";
    Db => "file_type_db", "file_type_light_db";
    Diff => "file_type_diff";
    Dotenv => "file_type_dotenv";
    Font => "file_type_font", "file_type_light_font";
    Image => "file_type_image";
    Ini => "file_type_ini", "file_type_light_ini";
    Json => "file_type_json", "file_type_light_json";
    Key => "file_type_key";
    License => "file_type_license";
    Log => "file_type_log";
    Markdown => "file_type_markdown";
    Patch => "file_type_patch";
    Readme => "file_type_readme";
    Shell => "file_type_shell";
    Sql => "file_type_sql";
    Text => "file_type_text";
    Toml => "file_type_toml", "file_type_light_toml";
    Video => "file_type_video";
    Xml => "file_type_xml";
    Yaml => "file_type_yaml", "file_type_light_yaml";
    Zip => "file_type_zip";
    Folder => "default_folder";
    FolderOpen => "default_folder_opened";
    FolderAssets => "folder_type_asset";
    FolderAssetsOpen => "folder_type_asset_opened";
    FolderBuild => "folder_type_dist";
    FolderBuildOpen => "folder_type_dist_opened";
    FolderConfig => "folder_type_config";
    FolderConfigOpen => "folder_type_config_opened";
    FolderDocs => "folder_type_docs";
    FolderDocsOpen => "folder_type_docs_opened";
    FolderImages => "folder_type_images";
    FolderImagesOpen => "folder_type_images_opened";
    FolderLib => "folder_type_library";
    FolderLibOpen => "folder_type_library_opened";
    FolderScripts => "folder_type_script";
    FolderScriptsOpen => "folder_type_script_opened";
    FolderSrc => "folder_type_src";
    FolderSrcOpen => "folder_type_src_opened";
    FolderTest => "folder_type_test";
    FolderTestOpen => "folder_type_test_opened";
}

impl FileIconName {
    /// The icon for a file name. Exact names come first, then extensions, ignoring case.
    /// `a.tar.gz` tries `tar.gz`, then `gz`. A name with no icon of its own gets the plain
    /// file icon.
    pub fn for_file(name: &str) -> Self {
        let name = name.to_ascii_lowercase();

        if let Some(icon) = by_name(&name) {
            return icon;
        }

        let mut rest = name.as_str();
        while let Some((_, after)) = rest.split_once('.') {
            if let Some(icon) = by_extension(after) {
                return icon;
            }

            rest = after;
        }

        Self::File
    }

    /// The icon for a folder name, ignoring case. A name with folders in it, such as
    /// `crates/term/src`, uses the last one. `open` picks the open or the closed icon.
    pub fn for_folder(name: &str, open: bool) -> Self {
        let name = name.trim_end_matches('/');
        let name = name.rsplit_once('/').map_or(name, |(_, last)| last);
        let (closed, opened) = by_folder(&name.to_ascii_lowercase());

        if open { opened } else { closed }
    }

    /// The SVG for a theme: its light file in a light theme when it has one.
    pub(crate) fn svg(self, kind: ThemeKind) -> &'static str {
        match kind {
            ThemeKind::Light => self.light_svg().unwrap_or_else(|| self.dark_svg()),
            ThemeKind::Dark => self.dark_svg(),
        }
    }

    /// The asset file that [`FileIconName::svg`] returns.
    pub(crate) fn asset_for(self, kind: ThemeKind) -> &'static str {
        match kind {
            ThemeKind::Light => self.light_asset().unwrap_or_else(|| self.asset()),
            ThemeKind::Dark => self.asset(),
        }
    }
}

fn by_name(name: &str) -> Option<FileIconName> {
    let icon = match name {
        ".bash_aliases" | ".bash_profile" | ".bashrc" | ".profile" => FileIconName::Shell,
        ".tool-versions" => FileIconName::Config,
        ".actionscriptproperties" | ".flexlibproperties" | ".flexproperties" | ".project" => {
            FileIconName::Xml
        }
        _ => return by_document_name(name),
    };

    Some(icon)
}

/// README, LICENSE and the like, with or without a document extension.
fn by_document_name(name: &str) -> Option<FileIconName> {
    let stem = ["md", "rst", "txt", "markdown"]
        .iter()
        .find_map(|extension| {
            name.strip_suffix(extension)
                .and_then(|stem| stem.strip_suffix('.'))
        })
        .unwrap_or(name);

    match stem {
        "copying" | "copying.lesser" | "licence" | "license" | "license-agpl"
        | "license-apache" | "license-gpl" | "license-mit" => Some(FileIconName::License),
        "index" | "readme" => Some(FileIconName::Readme),
        _ => None,
    }
}

fn by_extension(extension: &str) -> Option<FileIconName> {
    let icon = match extension {
        "aac" | "act" | "aiff" | "amr" | "ape" | "au" | "dct" | "dss" | "dvf" | "flac" | "gsm"
        | "iklax" | "ivs" | "m4a" | "m4b" | "m4p" | "mmf" | "mogg" | "mp3" | "mpc" | "msv"
        | "oga" | "ogg" | "opus" | "ra" | "raw" | "tta" | "vox" | "wav" | "wma" => {
            FileIconName::Audio
        }
        "a" | "app" | "bin" | "cma" | "cmi" | "cmo" | "cmx" | "cmxa" | "dll" | "exe" | "hl"
        | "ilk" | "lib" | "n" | "ndll" | "o" | "obj" | "pdb" | "pyc" | "pyd" | "pyo" | "scpt"
        | "scptd" | "so" => FileIconName::Binary,
        "cer" | "crl" | "crt" | "csr" | "der" | "p12" | "p7b" | "p7r" | "pfx" | "src" | "sst"
        | "stl" => FileIconName::Cert,
        "plist" | "properties" => FileIconName::Config,
        "csv" | "tsv" => FileIconName::Csv,
        "db" => FileIconName::Db,
        "diff" => FileIconName::Diff,
        "env" => FileIconName::Dotenv,
        "eot" | "otf" | "pfa" | "pfb" | "sfd" | "ttf" | "woff" | "woff2" => FileIconName::Font,
        "bmp" | "gif" | "heic" | "icns" | "ico" | "jpeg" | "jpg" | "png" | "tiff" | "webp" => {
            FileIconName::Image
        }
        "ini" => FileIconName::Ini,
        "json" | "json-tmlanguage" | "jsonc" | "jsonl" | "ndjson" => FileIconName::Json,
        "key" | "pem" => FileIconName::Key,
        "enc" | "lic" | "license" => FileIconName::License,
        "log" | "tlg" => FileIconName::Log,
        "markdown" | "md" | "mdown" | "mkd" => FileIconName::Markdown,
        "patch" => FileIconName::Patch,
        "bash" | "fish" | "ksh" | "sh" | "zsh" => FileIconName::Shell,
        "sql" => FileIconName::Sql,
        "txt" => FileIconName::Text,
        "toml" => FileIconName::Toml,
        "3g2" | "3gp" | "amv" | "asf" | "avi" | "divx" | "f4a" | "f4b" | "f4p" | "f4v" | "flv"
        | "m2v" | "m4v" | "mk3d" | "mkv" | "mov" | "mp2" | "mp4" | "mpe" | "mpeg" | "mpeg2"
        | "mpg" | "mpv" | "nsv" | "ogv" | "qt" | "rm" | "rmvb" | "svi" | "vob" | "webm" | "wmv" => {
            FileIconName::Video
        }
        "pex" | "tmlanguage" | "xml" => FileIconName::Xml,
        "yaml" | "yaml-tmlanguage" | "yml" => FileIconName::Yaml,
        "7z" | "br" | "bz" | "bz2" | "bzip2" | "gz" | "rar" | "tar" | "tgz" | "whl" | "xz"
        | "zip" | "zipx" => FileIconName::Zip,
        _ => return None,
    };

    Some(icon)
}

/// The closed and the open icon of a folder.
fn by_folder(name: &str) -> (FileIconName, FileIconName) {
    use FileIconName::{
        Folder, FolderAssets, FolderAssetsOpen, FolderBuild, FolderBuildOpen, FolderConfig,
        FolderConfigOpen, FolderDocs, FolderDocsOpen, FolderImages, FolderImagesOpen, FolderLib,
        FolderLibOpen, FolderOpen, FolderScripts, FolderScriptsOpen, FolderSrc, FolderSrcOpen,
        FolderTest, FolderTestOpen,
    };

    match name {
        ".assets" | "assets" => (FolderAssets, FolderAssetsOpen),
        ".build" | ".dist" | "build" | "builds" | "dist" | "dists" | "export" | "exports"
        | "out" | "outputs" | "output" | "outs" | "release" | "releases" | "target" | "targets" => {
            (FolderBuild, FolderBuildOpen)
        }
        ".cfg" | ".cfgs" | ".conf" | ".config" | ".configs" | ".configuration"
        | ".configurations" | ".ini" | ".initializers" | ".setting" | ".settings" | "cfg"
        | "cfgs" | "conf" | "config" | "configs" | "configuration" | "configurations" | "ini"
        | "initializers" | "setting" | "settings" => (FolderConfig, FolderConfigOpen),
        "doc" | "docs" | "document" | "documents" => (FolderDocs, FolderDocsOpen),
        ".fig" | ".figs" | ".img" | ".imgs" | "ico" | "icon" | "icons" | "image" | "images"
        | "img" | "imgs" | "screenshot" | "screenshots" | "svg" | "texture" | "textures" => {
            (FolderImages, FolderImagesOpen)
        }
        ".lib" | ".libs" | "__lib__" | "__libs__" | "lib" | "libraries" | "library" | "libs" => {
            (FolderLib, FolderLibOpen)
        }
        "script" | "scripts" => (FolderScripts, FolderScriptsOpen),
        "__src__" | "source" | "sources" | "src" => (FolderSrc, FolderSrcOpen),
        ".spec" | ".specs" | ".test" | ".tests" | "__test__" | "__tests__" | "integration"
        | "spec" | "specs" | "test" | "tests" => (FolderTest, FolderTestOpen),
        _ => (Folder, FolderOpen),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::FileIconName::{self, *};
    use super::*;

    fn assets() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/file-icons")
    }

    fn is_svg(name: &str) -> bool {
        Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
    }

    /// The file names in backticks in the license file that end in `.svg`.
    fn licensed_files() -> BTreeSet<String> {
        let license = fs::read_to_string(assets().join("LICENSE.md")).unwrap_or_default();

        license
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|token| is_svg(token))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn exact_names_come_before_extensions() {
        assert_eq!(FileIconName::for_file("README.md"), Readme);
        assert_eq!(FileIconName::for_file("notes.md"), Markdown);
        assert_eq!(FileIconName::for_file("LICENSE"), License);
        assert_eq!(FileIconName::for_file("LICENSE-MIT.txt"), License);
        assert_eq!(FileIconName::for_file("license.txt"), License);
        assert_eq!(FileIconName::for_file("notes.txt"), Text);
        assert_eq!(FileIconName::for_file(".bashrc"), Shell);
        assert_eq!(FileIconName::for_file(".tool-versions"), Config);
    }

    #[test]
    fn extensions_pick_the_icon() {
        for (name, icon) in [
            ("settings.json", Json),
            ("Cargo.toml", Toml),
            ("ci.yml", Yaml),
            ("ci.yaml", Yaml),
            ("data.csv", Csv),
            ("run.sh", Shell),
            ("page.xml", Xml),
            ("query.sql", Sql),
            ("server.log", Log),
            ("photo.png", Image),
            ("song.mp3", Audio),
            ("clip.mp4", Video),
            ("face.woff2", Font),
            ("site.zip", Zip),
            ("tool.exe", Binary),
            ("host.pem", Key),
            ("host.crt", Cert),
            ("app.ini", Ini),
            ("change.patch", Patch),
            ("change.diff", Diff),
            (".env", Dotenv),
            ("data.db", Db),
        ] {
            assert_eq!(FileIconName::for_file(name), icon, "{name}");
        }
    }

    #[test]
    fn the_last_extensions_decide() {
        assert_eq!(FileIconName::for_file("backup.tar.gz"), Zip);
        assert_eq!(FileIconName::for_file("tsconfig.build.json"), Json);
        assert_eq!(FileIconName::for_file("a.b.unknown"), File);
    }

    #[test]
    fn case_does_not_matter() {
        assert_eq!(FileIconName::for_file("PHOTO.PNG"), Image);
        assert_eq!(FileIconName::for_file("Readme.MD"), Readme);
        assert_eq!(FileIconName::for_file("license"), License);
        assert_eq!(FileIconName::for_folder("SRC", false), FolderSrc);
        assert_eq!(FileIconName::for_folder("Docs", true), FolderDocsOpen);
    }

    #[test]
    fn unknown_files_and_logo_only_files_get_the_plain_file_icon() {
        for name in [
            "",
            "Makefile",
            "notes",
            "file.",
            "main.rs",
            "app.ts",
            "script.js",
            "tool.py",
            "main.go",
            "Cargo.lock",
            "report.pdf",
            "Dockerfile",
            ".gitignore",
        ] {
            assert_eq!(FileIconName::for_file(name), File, "{name:?}");
        }
    }

    #[test]
    fn a_logo_file_name_uses_the_icon_of_its_extension() {
        assert_eq!(FileIconName::for_file("package.json"), Json);
        assert_eq!(FileIconName::for_file("Cargo.toml"), Toml);
        assert_eq!(FileIconName::for_file("docker-compose.yml"), Yaml);
    }

    #[test]
    fn folders_follow_their_name_and_state() {
        for (name, closed, open) in [
            ("src", FolderSrc, FolderSrcOpen),
            ("tests", FolderTest, FolderTestOpen),
            ("__tests__", FolderTest, FolderTestOpen),
            ("docs", FolderDocs, FolderDocsOpen),
            ("assets", FolderAssets, FolderAssetsOpen),
            ("images", FolderImages, FolderImagesOpen),
            ("config", FolderConfig, FolderConfigOpen),
            (".config", FolderConfig, FolderConfigOpen),
            ("dist", FolderBuild, FolderBuildOpen),
            ("build", FolderBuild, FolderBuildOpen),
            ("scripts", FolderScripts, FolderScriptsOpen),
            ("lib", FolderLib, FolderLibOpen),
            ("anything", Folder, FolderOpen),
            ("", Folder, FolderOpen),
        ] {
            assert_eq!(FileIconName::for_folder(name, false), closed, "{name:?}");
            assert_eq!(FileIconName::for_folder(name, true), open, "{name:?}");
        }
    }

    #[test]
    fn a_folder_with_folders_in_its_name_uses_the_last_one() {
        assert_eq!(
            FileIconName::for_folder("crates/swarm_term/src", true),
            FolderSrcOpen
        );
        assert_eq!(FileIconName::for_folder("docs/", false), FolderDocs);
        assert_eq!(FileIconName::for_folder("src/other", false), Folder);
    }

    #[test]
    fn a_logo_folder_gets_the_plain_folder() {
        for name in [
            "crates",
            "node_modules",
            ".github",
            ".git",
            "docker",
            "typescript",
        ] {
            assert_eq!(FileIconName::for_folder(name, false), Folder, "{name}");
        }
    }

    #[test]
    fn every_icon_has_a_file_in_the_assets() {
        for icon in FileIconName::ALL {
            for asset in [Some(icon.asset()), icon.light_asset()]
                .into_iter()
                .flatten()
            {
                let path = assets().join(format!("{asset}.svg"));

                assert!(path.is_file(), "{icon:?}: {} is missing", path.display());
            }
        }
    }

    #[test]
    fn every_icon_file_is_in_the_license_list() {
        let licensed = licensed_files();

        for icon in FileIconName::ALL {
            for asset in [Some(icon.asset()), icon.light_asset()]
                .into_iter()
                .flatten()
            {
                assert!(
                    licensed.contains(&format!("{asset}.svg")),
                    "{asset}.svg is not in LICENSE.md"
                );
            }
        }
    }

    #[test]
    fn no_asset_file_is_outside_the_license_list() {
        let licensed = licensed_files();
        let files: BTreeSet<String> = fs::read_dir(assets())
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| is_svg(name))
            .collect();

        assert!(!files.is_empty(), "no icons in {}", assets().display());
        assert_eq!(
            files.difference(&licensed).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "asset files that LICENSE.md does not list"
        );
        assert_eq!(
            licensed.difference(&files).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "files that LICENSE.md lists and the assets do not have"
        );
    }

    #[test]
    fn no_asset_file_goes_unused() {
        let used: BTreeSet<String> = FileIconName::ALL
            .iter()
            .flat_map(|icon| [Some(icon.asset()), icon.light_asset()])
            .flatten()
            .map(|asset| format!("{asset}.svg"))
            .collect();

        assert_eq!(used, licensed_files());
    }

    #[test]
    fn a_light_theme_uses_the_light_file_when_there_is_one() {
        assert_eq!(Json.asset_for(ThemeKind::Dark), "file_type_json");
        assert_eq!(Json.asset_for(ThemeKind::Light), "file_type_light_json");
        assert_eq!(Markdown.asset_for(ThemeKind::Light), "file_type_markdown");
        assert_ne!(Json.svg(ThemeKind::Dark), Json.svg(ThemeKind::Light));
        assert_eq!(
            Markdown.svg(ThemeKind::Dark),
            Markdown.svg(ThemeKind::Light)
        );
    }

    #[test]
    fn every_light_file_belongs_to_a_file_with_a_dark_one() {
        for icon in FileIconName::ALL {
            if let Some(light) = icon.light_asset() {
                assert!(light.contains("_light_"), "{light}");
                assert_eq!(light.replace("_light_", "_"), icon.asset());
            }
        }
    }
}
