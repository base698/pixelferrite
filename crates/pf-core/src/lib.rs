//! Pixelferrite core: document model, compositing, tools and file formats.
//! This crate has no GUI dependencies so everything here is testable headless.

pub mod aiedit;
pub mod api;
pub mod blend;
pub mod buf;
pub mod composite;
pub mod document;
pub mod fill;
pub mod filter;
pub mod fx;
pub mod geom;
pub mod io;
pub mod ops;
pub mod paint;
pub mod segment;
pub mod selection;
pub mod text;
pub mod transform;

pub use blend::BlendMode;
pub use buf::{Buf, Mask, Pixmap};
pub use document::{DocState, Document, Layer, LayerId, Target};
pub use geom::IRect;
