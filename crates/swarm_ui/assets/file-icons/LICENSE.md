# File icons

The SVG files in this folder are icons from vscode-icons.

- **Authors:** the vscode-icons contributors.
- **Source:** https://github.com/vscode-icons/vscode-icons (master branch, downloaded 2026-11-01).
- **License:** Creative Commons Attribution-ShareAlike 4.0 International (CC BY-SA 4.0): https://creativecommons.org/licenses/by-sa/4.0/
- **Terms:** the repository README says the icons are under CC BY-SA and that branded icons are under their own copyright license. The vscode-icons source code is MIT. Swarm uses no vscode-icons code.

## What changed

- **The files:** nothing. Each SVG is a byte-for-byte copy of the file with the same name in the `icons` folder of the repository.
- **How Swarm draws them:** it rasterizes each SVG to the size of the row on screen. The SVG files are not edited.
- **Which icons:** Swarm uses a small subset, chosen by hand.

## License of these files

These files stay under CC BY-SA 4.0. The license of the Swarm source code does not apply to them. Anyone who changes and shares them has to keep the same license and say what changed.

## Which icons are in

Only generic glyphs: no product, company or project logo. A file that vscode-icons shows with a logo, such as a Rust, TypeScript, Python or Docker file, gets the default file icon in Swarm. A folder that has only a logo icon, such as `crates`, `node_modules` or `.github`, gets the default folder icon.

Left out although it is a generic format: `pdf` (it shows the Adobe Acrobat mark). There is no generic lock-file icon in vscode-icons, so lock files get the default file icon.

## Files and why they are not logos

Dark-theme files are used in the dark theme, and `light` files in the light theme.

### Files

- `default_file.svg`: a plain page outline.
- `file_type_audio.svg`: a loudspeaker.
- `file_type_binary.svg`: a page with the digits 0 1 1 0.
- `file_type_cert.svg`: a page with a padlock and a ribbon seal.
- `file_type_config.svg`: a gear.
- `file_type_light_config.svg`: a gear, drawn for light themes.
- `file_type_csv.svg`: a page with a table of rows.
- `file_type_db.svg`: a stack of disks, the generic database symbol.
- `file_type_light_db.svg`: the same stack, drawn for light themes.
- `file_type_diff.svg`: a red minus above a green plus.
- `file_type_dotenv.svg`: the text ".ENV" on a yellow square.
- `file_type_font.svg`: the letters "Aa".
- `file_type_light_font.svg`: the letters "Aa", drawn for light themes.
- `file_type_image.svg`: a picture with a mountain and a sun.
- `file_type_ini.svg`: a gear.
- `file_type_light_ini.svg`: a gear, drawn for light themes.
- `file_type_json.svg`: yellow curly braces.
- `file_type_light_json.svg`: curly braces, drawn for light themes.
- `file_type_key.svg`: a golden key on a page with a ribbon seal.
- `file_type_license.svg`: two golden keys.
- `file_type_log.svg`: a green page of text lines.
- `file_type_markdown.svg`: the letters "M" and a down arrow, the generic Markdown mark.
- `file_type_patch.svg`: an adhesive bandage.
- `file_type_readme.svg`: an open book.
- `file_type_shell.svg`: a terminal window with a prompt.
- `file_type_sql.svg`: a stack of yellow disks, the generic database symbol.
- `file_type_text.svg`: a page with lines of text.
- `file_type_toml.svg`: the letter "T" in square brackets.
- `file_type_light_toml.svg`: the same, drawn for light themes.
- `file_type_video.svg`: a film strip with a play triangle.
- `file_type_xml.svg`: orange angle brackets and a slash.
- `file_type_yaml.svg`: the word "YAML" in yellow letters, the name of the format.
- `file_type_light_yaml.svg`: the same, drawn for light themes.
- `file_type_zip.svg`: a zipper on a tan page.

### Folders

- `default_folder.svg`: a plain closed folder.
- `default_folder_opened.svg`: a plain open folder.
- `folder_type_asset.svg`: a red folder with a safe box that has a gear on it.
- `folder_type_asset_opened.svg`: the same folder, open.
- `folder_type_dist.svg`: a tan folder with a white paper bag.
- `folder_type_dist_opened.svg`: the same folder, open.
- `folder_type_config.svg`: a blue folder with a gear.
- `folder_type_config_opened.svg`: the same folder, open.
- `folder_type_docs.svg`: a red folder with a page marked "DOC".
- `folder_type_docs_opened.svg`: the same folder, open.
- `folder_type_images.svg`: a green folder with a landscape picture.
- `folder_type_images_opened.svg`: the same folder, open.
- `folder_type_library.svg`: an orange folder with colored books.
- `folder_type_library_opened.svg`: the same folder, open.
- `folder_type_script.svg`: a gray folder with a scroll and angle brackets.
- `folder_type_script_opened.svg`: the same folder, open.
- `folder_type_src.svg`: a green folder with angle brackets and a slash.
- `folder_type_src_opened.svg`: the same folder, open.
- `folder_type_test.svg`: a red folder with a pink test tube.
- `folder_type_test_opened.svg`: the same folder, open.
