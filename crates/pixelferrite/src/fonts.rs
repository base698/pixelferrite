//! Fonts for the text tool: the built-in face plus whatever is installed.

use std::collections::HashMap;
use std::sync::Arc;

use pf_core::text::FontRef;

/// Font file bytes and the face's index within the file.
pub type Face = (Arc<Vec<u8>>, u32);

#[derive(Default)]
pub struct Fonts {
    /// Loaded on first use: scanning system fonts takes a moment.
    db: Option<fontdb::Database>,
    families: Vec<String>,
    cache: HashMap<(String, bool, bool), Face>,
    builtin: Option<Face>,
}

impl Fonts {
    fn db(&mut self) -> &fontdb::Database {
        if self.db.is_none() {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            let mut names: Vec<String> = db.faces().filter_map(|f| f.families.first().map(|n| n.0.clone())).collect();
            // Hidden system faces start with a dot on macOS.
            names.retain(|n| !n.starts_with('.'));
            names.sort_by_key(|n| n.to_lowercase());
            names.dedup();
            self.families = names;
            self.db = Some(db);
        }
        self.db.as_ref().unwrap()
    }

    /// Installed font families, sorted by name.
    pub fn families(&mut self) -> &[String] {
        self.db();
        &self.families
    }

    fn builtin(&mut self) -> Face {
        self.builtin.get_or_insert_with(|| (Arc::new(epaint_default_fonts::UBUNTU_LIGHT.to_vec()), 0)).clone()
    }

    /// The face for a family and style. Falls back to the built-in font when
    /// the family is empty, missing, or in a format we can't read.
    pub fn face(&mut self, family: &str, bold: bool, italic: bool) -> Face {
        if family.is_empty() {
            return self.builtin();
        }
        let key = (family.to_owned(), bold, italic);
        if let Some(f) = self.cache.get(&key) {
            return f.clone();
        }
        let db = self.db();
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight: if bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL },
            stretch: fontdb::Stretch::Normal,
            style: if italic { fontdb::Style::Italic } else { fontdb::Style::Normal },
        };
        let found = db
            .query(&query)
            .and_then(|id| db.with_face_data(id, |data, index| (Arc::new(data.to_vec()), index)))
            .filter(|(data, index)| FontRef::try_from_slice_and_index(data, *index).is_ok());
        let face = found.unwrap_or_else(|| self.builtin());
        self.cache.insert(key, face.clone());
        face
    }
}

impl pf_core::api::Host for Fonts {
    fn font(&mut self, family: &str, bold: bool, italic: bool) -> Face {
        self.face(family, bold, italic)
    }

    fn font_families(&mut self) -> Vec<String> {
        self.families().to_vec()
    }
}
