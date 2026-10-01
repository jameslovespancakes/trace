//! Helpers of the trace-bridge unit tests.

use trace_core::model::FileId;

use crate::recognize::{annotation_files, Endpoint, Recognizer};
use crate::BridgeInput;

/// Endpoints of one file (a fresh recognizer; the detection shares one across files).
pub(crate) fn file_endpoints(input: &BridgeInput<'_>, file: FileId) -> Vec<Endpoint> {
    let annotations = annotation_files(input, None);
    let recognizer = Recognizer::new(input, &annotations);
    let language = match input.index.files.get(file.idx()) {
        Some(rec) => rec.language,
        None => return Vec::new(),
    };
    recognizer
        .file(file)
        .facts
        .into_iter()
        .map(|fact| Endpoint { file, language, fact })
        .collect()
}
