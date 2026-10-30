//! `ptyZZZ assets <dir>`: write the browser side of a terminal, the
//! `<pty-term>` element, its styles, its font and the Datastar Rocket bundle
//! it imports, so a server can serve them next to this binary. Embedded at
//! build time, so the files always match the protocol this binary speaks.

use std::path::Path;

/// Paths relative to `static/`, kept as the same tree on disk: pty-term.js
/// imports `./vendor/...` and pty-term.css loads `fonts/...` relative to
/// themselves.
const FILES: &[(&str, &[u8])] = &[
    ("pty-term.js", include_bytes!("../static/pty-term.js")),
    ("pty-term.css", include_bytes!("../static/pty-term.css")),
    (
        "vendor/datastar-rocket@1.0.4.js",
        include_bytes!("../static/vendor/datastar-rocket@1.0.4.js"),
    ),
    (
        "fonts/AdwaitaMonoNerdFontMono-Regular.woff2",
        include_bytes!("../static/fonts/AdwaitaMonoNerdFontMono-Regular.woff2"),
    ),
    (
        "fonts/AdwaitaMonoNerdFontMono-Bold.woff2",
        include_bytes!("../static/fonts/AdwaitaMonoNerdFontMono-Bold.woff2"),
    ),
];

/// Write every file under `dir`, creating directories as needed and
/// replacing what is there. Prints each path written.
pub fn write(dir: &Path) -> std::io::Result<()> {
    for (rel, bytes) in FILES {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
        println!("{}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_whole_tree_byte_for_byte() {
        let dir = std::env::temp_dir().join(format!("ptyzzz-assets-{}", std::process::id()));
        write(&dir).unwrap();
        for (rel, bytes) in FILES {
            assert_eq!(&std::fs::read(dir.join(rel)).unwrap(), bytes, "{rel}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_element_imports_rocket_relative_to_itself() {
        let js = std::str::from_utf8(FILES[0].1).unwrap();
        assert!(js.contains("from \"./vendor/datastar-rocket@1.0.4.js\""));
    }
}
