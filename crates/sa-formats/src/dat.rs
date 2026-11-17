//! `gta.dat` / `default.dat`: which IDE, IPL, IMG and COL files the game loads.

use crate::strip_comment;

#[derive(Debug, Default, Clone)]
pub struct DatFile {
    pub ide: Vec<String>,
    pub ipl: Vec<String>,
    pub img: Vec<String>,
    pub colfile: Vec<String>,
}

impl DatFile {
    pub fn parse(text: &str, into: &mut DatFile) {
        for line in text.lines() {
            let l = strip_comment(line);
            let mut it = l.splitn(2, char::is_whitespace);
            let (Some(kind), Some(rest)) = (it.next(), it.next()) else { continue };
            let rest = rest.trim().to_string();
            match kind.to_ascii_uppercase().as_str() {
                "IDE" => into.ide.push(rest),
                "IPL" => into.ipl.push(rest),
                "IMG" | "CDIMAGE" => into.img.push(rest),
                // COLFILE <zone> <path>
                "COLFILE" => {
                    if let Some(p) = rest.split_whitespace().nth(1) {
                        into.colfile.push(p.to_string())
                    }
                }
                _ => {}
            }
        }
    }
}
