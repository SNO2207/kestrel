//! The models Kestrel offers, pinned to exact files: where they come from (a repository at a
//! fixed revision), how big each file is and its SHA-256, and how the model wants its input.

use std::borrow::Cow;

use serde::Serialize;

/// Where model files come from: `{BASE}/{repo}/resolve/{revision}/{path}`.
pub const HUGGING_FACE: &str = "https://huggingface.co";

/// One file of a model, as published.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFile {
    /// Its path in the repository, which is also its path under the model's folder.
    pub path: &'static str,
    pub size: u64,
    /// SHA-256, lower-case hex.
    pub sha256: Cow<'static, str>,
    /// What it is: `vision`, `text` or `tokenizer`.
    pub role: &'static str,
}

/// How the vision tower wants its pictures.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSpec {
    /// The square side the picture is resized to.
    pub size: u32,
    /// Pixel values are multiplied by this (1/255), then normalised.
    pub rescale: f32,
    pub mean: [f32; 3],
    pub std: [f32; 3],
}

/// Thresholds for "how alike is alike enough" at the two ends of the owner's slider. Cosines
/// between a description and a picture are small for SigLIP (a good match is around 0.1 to
/// 0.2), between two pictures large; so each kind of query has its own range.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cutoffs {
    /// Description to picture: the slider's "fewer" end and its "more" end.
    pub text: (f32, f32),
    /// Picture to picture ("Find similar").
    pub image: (f32, f32),
}

/// A model Kestrel can download and run.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    /// Kestrel's own name for it, which is also its folder: `fast` or `best`.
    pub id: &'static str,
    /// What the AI search page calls it.
    pub title: &'static str,
    /// What it is, in a sentence.
    pub summary: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub licence: &'static str,
    pub author: &'static str,
    pub converted_by: &'static str,
    pub files: Vec<ModelFile>,
    pub image: ImageSpec,
    /// The length of its vectors.
    pub dim: usize,
    /// Tokens every description is padded (or cut) to.
    pub text_length: usize,
    pub cutoffs: Cutoffs,
}

impl ModelSpec {
    /// Every file's size, added up.
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    /// Where a file is published.
    pub fn url(&self, base: &str, file: &ModelFile) -> String {
        format!("{}/{}/resolve/{}/{}", base.trim_end_matches('/'), self.repo, self.revision, file.path)
    }

    /// The repository's page, at the pinned revision.
    pub fn page(&self) -> String {
        format!("{HUGGING_FACE}/{}/tree/{}", self.repo, self.revision)
    }

    pub fn file(&self, role: &str) -> Option<&ModelFile> {
        self.files.iter().find(|file| file.role == role)
    }

    /// The tag its vectors are stored under: vectors of another model are never compared.
    pub fn vector_tag(&self) -> String {
        format!("{}@{}", self.id, &self.revision[..self.revision.len().min(16)])
    }
}

/// The normalisation both SigLIP 2 models use: x / 255, then (x - 0.5) / 0.5.
const SIGLIP_NORMALISATION: (f32, [f32; 3], [f32; 3]) = (1.0 / 255.0, [0.5; 3], [0.5; 3]);

/// The tokenizer both models share (Gemma's, as SigLIP 2 uses it).
const TOKENIZER: ModelFile = ModelFile {
    path: "tokenizer.json",
    size: 34_363_039,
    sha256: Cow::Borrowed("cb9140fae3ac5122c972d37adf83e1248471a38147ad76f8215c8872c6fd8322"),
    role: "tokenizer",
};

/// SigLIP 2 base, 256 px: quicker and smaller.
pub fn fast() -> ModelSpec {
    let (rescale, mean, std) = SIGLIP_NORMALISATION;
    ModelSpec {
        id: "fast",
        title: "Fast",
        summary: "SigLIP 2 base at 256 × 256 pixels: Google's image and text model in its base size. Quicker and smaller, a little less exact.",
        repo: "onnx-community/siglip2-base-patch16-256-ONNX",
        revision: "d1114256522a37ffa257a0a58017348ab0058db2",
        licence: "Apache 2.0",
        author: "Google",
        converted_by: "onnx-community",
        files: vec![
            ModelFile {
                path: "onnx/vision_model_fp16.onnx",
                size: 186_131_676,
                sha256: Cow::Borrowed("fe9ad8020a6d3d98d394c9be8f07064066135fc2f87ec11692de0b677c0ac4db"),
                role: "vision",
            },
            ModelFile {
                path: "onnx/text_model_fp16.onnx",
                size: 564_862_230,
                sha256: Cow::Borrowed("80954edffdc689599e5d5bc6a1738380bc9e8139a18e5c8892485f248b6b4890"),
                role: "text",
            },
            TOKENIZER,
        ],
        image: ImageSpec { size: 256, rescale, mean, std },
        dim: 768,
        text_length: 64,
        cutoffs: Cutoffs { text: (0.15, 0.03), image: (0.85, 0.55) },
    }
}

/// SigLIP 2 so400m, 384 px: the most exact.
pub fn best() -> ModelSpec {
    let (rescale, mean, std) = SIGLIP_NORMALISATION;
    ModelSpec {
        id: "best",
        title: "Best",
        summary: "SigLIP 2 so400m at 384 × 384 pixels: Google's image and text model in its largest shape-optimised size. The most exact, three times the size.",
        repo: "onnx-community/siglip2-so400m-patch16-384-ONNX",
        revision: "29f74e673c298fd07482d8e20138179282001eb7",
        licence: "Apache 2.0",
        author: "Google",
        converted_by: "onnx-community",
        files: vec![
            ModelFile {
                path: "onnx/vision_model_fp16.onnx",
                size: 857_096_382,
                sha256: Cow::Borrowed("7a8059233d8076973f8f754b8bf588674baa3ac4982bd605bd1f25986e85096a"),
                role: "vision",
            },
            ModelFile {
                path: "onnx/text_model_fp16.onnx",
                size: 1_416_135_143,
                sha256: Cow::Borrowed("6aed8a7f6fbfbbc1464e57ac7dc3582557bd9da8d7abe761163e2eaf7ad66994"),
                role: "text",
            },
            TOKENIZER,
        ],
        image: ImageSpec { size: 384, rescale, mean, std },
        dim: 1152,
        text_length: 64,
        cutoffs: Cutoffs { text: (0.16, 0.04), image: (0.85, 0.55) },
    }
}

/// Both models, fast first.
pub fn models() -> Vec<ModelSpec> {
    vec![fast(), best()]
}

pub fn model(id: &str) -> Option<ModelSpec> {
    models().into_iter().find(|model| model.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_matches_the_published_files() {
        let fast = fast();
        assert_eq!(fast.total_size(), 785_356_945);
        assert_eq!(
            fast.url(HUGGING_FACE, &fast.files[0]),
            "https://huggingface.co/onnx-community/siglip2-base-patch16-256-ONNX/resolve/d1114256522a37ffa257a0a58017348ab0058db2/onnx/vision_model_fp16.onnx"
        );
        let best = best();
        assert_eq!(best.total_size(), 2_307_594_564);
        assert_eq!(best.file("tokenizer"), fast.file("tokenizer"), "one tokenizer for both");
        for model in models() {
            for file in &model.files {
                assert_eq!(file.sha256.len(), 64);
                assert!(file.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
            }
            assert!(model.vector_tag().len() <= 40, "{}", model.vector_tag());
            assert_ne!(model.vector_tag(), if model.id == "fast" { best.vector_tag() } else { fast.vector_tag() });
        }
    }
}
