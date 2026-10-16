//! Writes small ONNX models by hand, protobuf and all, shaped like SigLIP 2's two towers: the
//! same input and output names, ranks and element types, so everything that loads, feeds and
//! reads the real models runs against them unchanged. Tests use them (no model is on the drive
//! and none may be downloaded), and development builds install them "as if downloaded" for the
//! end-to-end suite.
//!
//! The tiny towers work in a four-dimensional space anyone can compute by hand: the average red,
//! green and blue of the normalised picture, and how much fine detail it has (the mean absolute
//! Laplacian of its brightness, weighted by [`TEXTURE_WEIGHT`]); a description is the mean of
//! its tokens' rows of [`TINY_VOCAB`] over all 64 positions. So "red" finds red pictures first,
//! and "receipt" (white, detailed) finds a page of text.

use std::path::Path;

use crate::Rgb;

// ---------- protobuf ----------

/// A protobuf message being written.
#[derive(Default)]
struct Pb(Vec<u8>);

const VARINT: u8 = 0;
const LEN: u8 = 2;
const FIXED32: u8 = 5;

impl Pb {
    fn varint(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.0.push(value as u8 | 0x80);
            value >>= 7;
        }
        self.0.push(value as u8);
    }

    fn key(&mut self, field: u32, wire: u8) {
        self.varint(u64::from(field << 3 | u32::from(wire)));
    }

    fn int(&mut self, field: u32, value: i64) -> &mut Self {
        self.key(field, VARINT);
        self.varint(value as u64);
        self
    }

    fn bytes(&mut self, field: u32, bytes: &[u8]) -> &mut Self {
        self.key(field, LEN);
        self.varint(bytes.len() as u64);
        self.0.extend_from_slice(bytes);
        self
    }

    fn string(&mut self, field: u32, text: &str) -> &mut Self {
        self.bytes(field, text.as_bytes())
    }

    fn message(&mut self, field: u32, message: Pb) -> &mut Self {
        self.bytes(field, &message.0)
    }

    fn float(&mut self, field: u32, value: f32) -> &mut Self {
        self.key(field, FIXED32);
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
}

// ONNX's element types (TensorProto.DataType).
pub const FLOAT: i64 = 1;
pub const INT64: i64 = 7;
pub const FLOAT16: i64 = 10;

/// A dimension of a declared shape: fixed, or named and free (such as the batch).
#[derive(Clone, Copy)]
pub enum Dim {
    Fixed(i64),
    Named(&'static str),
}

/// An attribute of a node.
pub enum Attr {
    Int(&'static str, i64),
    Ints(&'static str, Vec<i64>),
    Float(&'static str, f32),
}

/// A node of the graph.
pub struct Node {
    pub op: &'static str,
    pub inputs: Vec<&'static str>,
    pub outputs: Vec<&'static str>,
    pub attrs: Vec<Attr>,
}

pub fn node(op: &'static str, inputs: &[&'static str], outputs: &[&'static str], attrs: Vec<Attr>) -> Node {
    Node { op, inputs: inputs.to_vec(), outputs: outputs.to_vec(), attrs }
}

/// A constant tensor of the graph.
pub enum Init {
    Float(&'static str, Vec<i64>, Vec<f32>),
    Int64(&'static str, Vec<i64>, Vec<i64>),
}

/// A whole model: one graph, at opset 17 (what Windows ML's ONNX Runtime 1.17 runs fully).
pub struct Graph {
    pub nodes: Vec<Node>,
    pub inits: Vec<Init>,
    pub inputs: Vec<(&'static str, i64, Vec<Dim>)>,
    pub outputs: Vec<(&'static str, i64, Vec<Dim>)>,
}

fn value_info(name: &str, elem: i64, dims: &[Dim]) -> Pb {
    let mut shape = Pb::default();
    for dim in dims {
        let mut d = Pb::default();
        match *dim {
            Dim::Fixed(value) => d.int(1, value),
            Dim::Named(name) => d.string(2, name),
        };
        shape.message(1, d);
    }
    let mut tensor = Pb::default();
    tensor.int(1, elem).message(2, shape);
    let mut ty = Pb::default();
    ty.message(1, tensor);
    let mut info = Pb::default();
    info.string(1, name).message(2, ty);
    info
}

impl Graph {
    /// The model's ONNX bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut graph = Pb::default();
        for (i, n) in self.nodes.iter().enumerate() {
            let mut p = Pb::default();
            for input in &n.inputs {
                p.string(1, input);
            }
            for output in &n.outputs {
                p.string(2, output);
            }
            p.string(3, &format!("{}_{i}", n.op)).string(4, n.op);
            for attr in &n.attrs {
                let mut a = Pb::default();
                match attr {
                    Attr::Int(name, value) => a.string(1, name).int(3, *value).int(20, 2),
                    Attr::Ints(name, values) => {
                        a.string(1, name);
                        for value in values {
                            a.int(8, *value);
                        }
                        a.int(20, 7)
                    }
                    Attr::Float(name, value) => a.string(1, name).float(2, *value).int(20, 1),
                };
                p.message(5, a);
            }
            graph.message(1, p);
        }
        graph.string(2, "kestrel-tiny");
        for init in &self.inits {
            let mut t = Pb::default();
            let (name, dims, elem, raw): (&str, &[i64], i64, Vec<u8>) = match init {
                Init::Float(name, dims, values) => (name, dims, FLOAT, values.iter().flat_map(|v| v.to_le_bytes()).collect()),
                Init::Int64(name, dims, values) => (name, dims, INT64, values.iter().flat_map(|v| v.to_le_bytes()).collect()),
            };
            for dim in dims {
                t.int(1, *dim);
            }
            t.int(2, elem).string(8, name).bytes(9, &raw);
            graph.message(5, t);
        }
        for (name, elem, dims) in &self.inputs {
            graph.message(11, value_info(name, *elem, dims));
        }
        for (name, elem, dims) in &self.outputs {
            graph.message(12, value_info(name, *elem, dims));
        }
        let mut opset = Pb::default();
        opset.string(1, "").int(2, 17);
        let mut model = Pb::default();
        model.int(1, 8).string(2, "kestrel").string(3, "1").message(7, graph).message(8, opset);
        model.0
    }
}

// ---------- the tiny SigLIP-shaped towers ----------

/// The length of the tiny towers' vectors.
pub const TINY_DIM: usize = 4;
/// How much the detail feature counts against the colours.
pub const TEXTURE_WEIGHT: f32 = 4.0;
/// The side of the tiny vision tower's square input.
pub const TINY_SIZE: u32 = 64;
/// Tokens a description is padded to, as for the real models.
pub const TINY_LENGTH: usize = 64;

/// The tiny tokenizer's words and the vector each stands for: red, green, blue, detail.
pub const TINY_VOCAB: &[(&str, [f32; 4])] = &[
    ("<pad>", [0.0; 4]),
    ("<eos>", [0.0; 4]),
    ("<unk>", [0.0; 4]),
    ("a", [0.0; 4]),
    ("the", [0.0; 4]),
    ("of", [0.0; 4]),
    ("photo", [0.0; 4]),
    ("picture", [0.0; 4]),
    ("red", [1.0, -1.0, -1.0, 0.0]),
    ("green", [-1.0, 1.0, -1.0, 0.0]),
    ("blue", [-1.0, -1.0, 1.0, 0.0]),
    ("yellow", [1.0, 1.0, -1.0, 0.0]),
    ("purple", [0.0, -1.0, 0.0, 0.0]),
    ("orange", [1.0, 0.0, -1.0, 0.0]),
    ("white", [1.0, 1.0, 1.0, 0.0]),
    ("black", [-1.0, -1.0, -1.0, 0.0]),
    ("square", [0.0, 0.0, 0.0, -0.5]),
    ("receipt", [1.0, 1.0, 1.0, 2.0]),
    ("stop", [1.0, -1.0, -1.0, 2.0]),
    ("sign", [0.0, 0.0, 0.0, 1.0]),
    ("sunset", [1.0, 0.0, -1.0, 0.0]),
    ("sea", [-1.0, 0.0, 1.0, 0.0]),
];

pub const PAD_ID: i64 = 0;
pub const EOS_ID: i64 = 1;

/// 3 x 3 Laplacian, the detail feature's kernel, shared out over the three channels.
const LAPLACIAN: [f32; 9] = [0.0, 1.0, 0.0, 1.0, -4.0, 1.0, 0.0, 1.0, 0.0];

fn eye(n: usize, last: f32) -> Vec<f32> {
    (0..n * n).map(|i| if i % (n + 1) == 0 { if i == n * n - 1 { last } else { 1.0 } } else { 0.0 }).collect()
}

/// The tiny vision tower for `size` x `size` pictures; with `half`, its input and outputs are
/// float16, as an fp16 export's may be.
pub fn tiny_vision(size: u32, half: bool) -> Vec<u8> {
    let s = i64::from(size);
    let elem = if half { FLOAT16 } else { FLOAT };
    let mut nodes = Vec::new();
    let pixels = if half {
        nodes.push(node("Cast", &["pixel_values"], &["pixels32"], vec![Attr::Int("to", FLOAT)]));
        "pixels32"
    } else {
        "pixel_values"
    };
    let edge_weights: Vec<f32> = (0..3).flat_map(|_| LAPLACIAN.iter().map(|w| w / 3.0)).collect();
    nodes.extend([
        node("Conv", &[pixels, "color_w"], &["color"], vec![Attr::Ints("kernel_shape", vec![1, 1])]),
        node("Conv", &[pixels, "edge_w"], &["edge"], vec![Attr::Ints("kernel_shape", vec![3, 3])]),
        node("Abs", &["edge"], &["edge_abs"], vec![]),
        node("GlobalAveragePool", &["color"], &["color_mean"], vec![]),
        node("GlobalAveragePool", &["edge_abs"], &["edge_mean"], vec![]),
        node("Concat", &["color_mean", "edge_mean"], &["features4d"], vec![Attr::Int("axis", 1)]),
        node("Flatten", &["features4d"], &["features"], vec![Attr::Int("axis", 1)]),
        node("Gemm", &["features", "proj_w", "proj_b"], &[if half { "pooled32" } else { "pooler_output" }], vec![Attr::Int("transB", 1)]),
        node("Reshape", &["color", "tokens_shape"], &["tokens"], vec![]),
        node("Transpose", &["tokens"], &[if half { "hidden32" } else { "last_hidden_state" }], vec![Attr::Ints("perm", vec![0, 2, 1])]),
    ]);
    if half {
        nodes.push(node("Cast", &["pooled32"], &["pooler_output"], vec![Attr::Int("to", FLOAT16)]));
        nodes.push(node("Cast", &["hidden32"], &["last_hidden_state"], vec![Attr::Int("to", FLOAT16)]));
    }
    Graph {
        nodes,
        inits: vec![
            Init::Float("color_w", vec![3, 3, 1, 1], eye(3, 1.0)),
            Init::Float("edge_w", vec![1, 3, 3, 3], edge_weights),
            Init::Float("proj_w", vec![TINY_DIM as i64, TINY_DIM as i64], eye(TINY_DIM, TEXTURE_WEIGHT)),
            Init::Float("proj_b", vec![TINY_DIM as i64], vec![0.0; TINY_DIM]),
            Init::Int64("tokens_shape", vec![3], vec![0, 3, -1]),
        ],
        inputs: vec![("pixel_values", elem, vec![Dim::Named("batch_size"), Dim::Fixed(3), Dim::Fixed(s), Dim::Fixed(s)])],
        outputs: vec![
            ("last_hidden_state", elem, vec![Dim::Named("batch_size"), Dim::Fixed(s * s), Dim::Fixed(3)]),
            ("pooler_output", elem, vec![Dim::Named("batch_size"), Dim::Fixed(TINY_DIM as i64)]),
        ],
    }
    .encode()
}

/// The tiny text tower: token ids in, the mean of their [`TINY_VOCAB`] rows out.
pub fn tiny_text(half: bool) -> Vec<u8> {
    let elem = if half { FLOAT16 } else { FLOAT };
    let table: Vec<f32> = TINY_VOCAB.iter().flat_map(|(_, row)| row.iter().copied()).collect();
    let mut nodes = vec![
        node("Gather", &["embeddings", "input_ids"], &[if half { "hidden32" } else { "last_hidden_state" }], vec![Attr::Int("axis", 0)]),
        node(
            "ReduceMean",
            &[if half { "hidden32" } else { "last_hidden_state" }],
            &["mean"],
            vec![Attr::Ints("axes", vec![1]), Attr::Int("keepdims", 0)],
        ),
        node("Gemm", &["mean", "proj_w", "proj_b"], &[if half { "pooled32" } else { "pooler_output" }], vec![Attr::Int("transB", 1)]),
    ];
    if half {
        nodes.push(node("Cast", &["pooled32"], &["pooler_output"], vec![Attr::Int("to", FLOAT16)]));
        nodes.push(node("Cast", &["hidden32"], &["last_hidden_state"], vec![Attr::Int("to", FLOAT16)]));
    }
    Graph {
        nodes,
        inits: vec![
            Init::Float("embeddings", vec![TINY_VOCAB.len() as i64, TINY_DIM as i64], table),
            Init::Float("proj_w", vec![TINY_DIM as i64, TINY_DIM as i64], eye(TINY_DIM, 1.0)),
            Init::Float("proj_b", vec![TINY_DIM as i64], vec![0.0; TINY_DIM]),
        ],
        inputs: vec![("input_ids", INT64, vec![Dim::Named("batch_size"), Dim::Named("sequence_length")])],
        outputs: vec![
            ("last_hidden_state", elem, vec![Dim::Named("batch_size"), Dim::Named("sequence_length"), Dim::Fixed(TINY_DIM as i64)]),
            ("pooler_output", elem, vec![Dim::Named("batch_size"), Dim::Fixed(TINY_DIM as i64)]),
        ],
    }
    .encode()
}

/// A `tokenizer.json` for [`TINY_VOCAB`]: words split on spaces, matched exactly (so
/// upper-case words are unknown unless lower-cased first, as for SigLIP 2's), `<eos>` appended,
/// no padding of its own.
pub fn tiny_tokenizer_json() -> String {
    let special = |id: usize, content: &str| {
        serde_json::json!({ "id": id, "content": content, "single_word": false, "lstrip": false, "rstrip": false, "normalized": false, "special": true })
    };
    let vocab: serde_json::Map<String, serde_json::Value> =
        TINY_VOCAB.iter().enumerate().map(|(id, (word, _))| ((*word).to_owned(), serde_json::json!(id))).collect();
    serde_json::json!({
        "version": "1.0",
        "truncation": null,
        "padding": null,
        "added_tokens": [special(0, "<pad>"), special(1, "<eos>"), special(2, "<unk>")],
        "normalizer": null,
        "pre_tokenizer": { "type": "WhitespaceSplit" },
        "post_processor": {
            "type": "TemplateProcessing",
            "single": [{ "Sequence": { "id": "A", "type_id": 0 } }, { "SpecialToken": { "id": "<eos>", "type_id": 0 } }],
            "pair": [
                { "Sequence": { "id": "A", "type_id": 0 } },
                { "Sequence": { "id": "B", "type_id": 1 } },
                { "SpecialToken": { "id": "<eos>", "type_id": 1 } }
            ],
            "special_tokens": { "<eos>": { "id": "<eos>", "ids": [1], "tokens": ["<eos>"] } }
        },
        "decoder": null,
        "model": { "type": "WordLevel", "vocab": vocab, "unk_token": "<unk>" }
    })
    .to_string()
}

/// Writes a tiny model where a downloaded one's files go: `onnx/vision_model_fp16.onnx`,
/// `onnx/text_model_fp16.onnx` and `tokenizer.json` under `dir`. Returns each file's bytes'
/// SHA-256 and size, keyed by path, for a catalogue entry that serves them.
pub fn write_tiny_model(dir: &Path, half: bool) -> std::io::Result<Vec<(String, u64, String)>> {
    use sha2::Digest;
    let files: [(&str, Vec<u8>); 3] = [
        ("onnx/vision_model_fp16.onnx", tiny_vision(TINY_SIZE, half)),
        ("onnx/text_model_fp16.onnx", tiny_text(half)),
        ("tokenizer.json", tiny_tokenizer_json().into_bytes()),
    ];
    let mut written = Vec::new();
    for (path, bytes) in files {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, &bytes)?;
        let hash = sha2::Sha256::digest(&bytes);
        written.push((path.to_owned(), bytes.len() as u64, hash.iter().map(|b| format!("{b:02x}")).collect()));
    }
    Ok(written)
}

/// What the tiny vision tower computes for a picture already resized to its input size, by
/// hand: the normalised channel means and the weighted detail, before L2 normalisation.
pub fn tiny_image_features(image: &Rgb) -> [f32; 4] {
    let (w, h) = (image.width as usize, image.height as usize);
    let norm = |v: u8| (f32::from(v) / 255.0 - 0.5) / 0.5;
    let mut means = [0f64; 3];
    for pixel in image.pixels.chunks_exact(3) {
        for c in 0..3 {
            means[c] += f64::from(norm(pixel[c]));
        }
    }
    let count = (w * h) as f64;
    let mut detail = 0f64;
    if w > 2 && h > 2 {
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let mut sum = 0f32;
                for ky in 0..3 {
                    for kx in 0..3 {
                        let at = ((y + ky - 1) * w + (x + kx - 1)) * 3;
                        let brightness: f32 = (0..3).map(|c| norm(image.pixels[at + c])).sum::<f32>() / 3.0;
                        sum += LAPLACIAN[ky * 3 + kx] * brightness;
                    }
                }
                detail += f64::from(sum.abs());
            }
        }
        detail /= ((w - 2) * (h - 2)) as f64;
    }
    [
        (means[0] / count) as f32,
        (means[1] / count) as f32,
        (means[2] / count) as f32,
        detail as f32 * TEXTURE_WEIGHT,
    ]
}

/// What the tiny text tower computes for token ids, by hand, before L2 normalisation.
pub fn tiny_text_features(ids: &[i64]) -> [f32; 4] {
    let mut sum = [0f32; 4];
    for &id in ids {
        let row = TINY_VOCAB.get(id as usize).map_or([0.0; 4], |(_, row)| *row);
        for d in 0..4 {
            sum[d] += row[d];
        }
    }
    sum.map(|v| v / ids.len().max(1) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_messages_encode_as_protobuf_does() {
        let mut pb = Pb::default();
        pb.int(1, 300);
        assert_eq!(pb.0, [0x08, 0xAC, 0x02]);
        let mut pb = Pb::default();
        pb.string(2, "hi").int(3, -1);
        // A negative int64 is ten bytes of varint.
        assert_eq!(&pb.0[..4], [0x12, 2, b'h', b'i']);
        assert_eq!(pb.0.len(), 4 + 1 + 10);
        let mut pb = Pb::default();
        pb.float(2, 1.0);
        assert_eq!(pb.0, [0x15, 0, 0, 0x80, 0x3F]);
    }
}
