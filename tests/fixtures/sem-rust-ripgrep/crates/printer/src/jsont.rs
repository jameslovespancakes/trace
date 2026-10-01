// ripgrep printer jsont.rs shape: several `impl<'a> serde::Serialize for X<'a>` blocks in one
// file (same-named `serialize` methods; `serde` is a registry dependency rust-analyzer cannot
// resolve offline) calling `Data::from_bytes` through an associated-function path, 4 times.
use std::borrow::Cow;
use std::path::Path;

pub(crate) struct Match<'a> {
    pub(crate) path: Option<&'a Path>,
    pub(crate) lines: &'a [u8],
}

impl<'a> serde::Serialize for Match<'a> {
    fn serialize<S: serde::Serializer>(
        &self,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        let mut state = s.serialize_struct("Match", 2)?;
        state.serialize_field("path", &self.path.map(Data::from_path))?;
        state.serialize_field("lines", &Data::from_bytes(self.lines))?;
        state.end()
    }
}

pub(crate) struct Context<'a> {
    pub(crate) lines: &'a [u8],
}

impl<'a> serde::Serialize for Context<'a> {
    fn serialize<S: serde::Serializer>(
        &self,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        let mut state = s.serialize_struct("Context", 1)?;
        state.serialize_field("lines", &Data::from_bytes(self.lines))?;
        state.end()
    }
}

pub(crate) struct SubMatch<'a> {
    pub(crate) m: &'a [u8],
    pub(crate) replacement: Option<&'a [u8]>,
}

impl<'a> serde::Serialize for SubMatch<'a> {
    fn serialize<S: serde::Serializer>(
        &self,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        let mut state = s.serialize_struct("SubMatch", 2)?;
        state.serialize_field("match", &Data::from_bytes(self.m))?;
        if let Some(r) = self.replacement {
            state.serialize_field("replacement", &Data::from_bytes(r))?;
        }
        state.end()
    }
}

/// Data represents things that look like strings, but may actually not be valid UTF-8.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum Data<'a> {
    Text { text: Cow<'a, str> },
    Bytes { bytes: &'a [u8] },
}

impl<'a> Data<'a> {
    fn from_bytes(bytes: &[u8]) -> Data<'_> {
        match std::str::from_utf8(bytes) {
            Ok(text) => Data::Text { text: Cow::Borrowed(text) },
            Err(_) => Data::Bytes { bytes },
        }
    }

    fn from_path(path: &Path) -> Data<'_> {
        Data::Text { text: Cow::Owned(path.display().to_string()) }
    }
}
