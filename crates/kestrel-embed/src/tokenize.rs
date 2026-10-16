//! Descriptions as the text tower takes them, which SigLIP 2 is strict about: lower-cased
//! (it was trained on lower-case text), tokenised by the model's own `tokenizer.json` with its
//! special tokens, and padded with the pad token to exactly 64 tokens (or cut to 64). The text
//! tower pools the last position, so a description padded to another length, or not at all,
//! means something else to it, and the results quietly get worse.

use std::path::Path;

use tokenizers::{
    PaddingDirection, PaddingParams, PaddingStrategy, Tokenizer, TruncationDirection, TruncationParams, TruncationStrategy,
};

use crate::{Error, Result};

/// The model's tokenizer, set to lower-case and to pad and cut to a fixed length.
pub struct Tokens {
    tokenizer: Tokenizer,
    length: usize,
    pad_id: u32,
}

fn failed(error: impl std::fmt::Display) -> Error {
    Error(format!("the tokenizer failed: {error}"))
}

impl Tokens {
    /// Reads the tokenizer at `path`.
    pub fn open(path: &Path, length: usize) -> Result<Tokens> {
        let tokenizer = Tokenizer::from_file(path).map_err(|error| Error(format!("can't read {}: {error}", path.display())))?;
        Tokens::with(tokenizer, length)
    }

    /// A tokenizer from the bytes of a `tokenizer.json`.
    pub fn from_json(json: &[u8], length: usize) -> Result<Tokens> {
        Tokens::with(Tokenizer::from_bytes(json).map_err(failed)?, length)
    }

    fn with(mut tokenizer: Tokenizer, length: usize) -> Result<Tokens> {
        // The pad token the file declares for padding, else the one named `<pad>` (Gemma's,
        // id 0); a tokenizer with neither can't make what the model expects.
        let (pad_id, pad_token) = match tokenizer.get_padding() {
            Some(padding) => (padding.pad_id, padding.pad_token.clone()),
            None => {
                let id = tokenizer.token_to_id("<pad>").ok_or_else(|| Error("the tokenizer has no pad token".into()))?;
                (id, "<pad>".to_owned())
            }
        };
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::Fixed(length),
            direction: PaddingDirection::Right,
            pad_to_multiple_of: None,
            pad_id,
            pad_type_id: 0,
            pad_token,
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                direction: TruncationDirection::Right,
                max_length: length,
                strategy: TruncationStrategy::LongestFirst,
                stride: 0,
            }))
            .map_err(failed)?;
        Ok(Tokens { tokenizer, length, pad_id })
    }

    pub fn length(&self) -> usize {
        self.length
    }

    pub fn pad_id(&self) -> u32 {
        self.pad_id
    }

    /// The ids of `text`, lower-cased, with the special tokens the tokenizer adds, padded or
    /// cut to exactly the length.
    pub fn ids(&self, text: &str) -> Result<Vec<i64>> {
        let lower = text.to_lowercase();
        let encoding = self.tokenizer.encode(lower.as_str(), true).map_err(failed)?;
        let mut ids: Vec<i64> = encoding.get_ids().iter().map(|&id| i64::from(id)).collect();
        // The tokenizer pads and cuts already; this only makes the length a certainty.
        ids.resize(self.length, i64::from(self.pad_id));
        Ok(ids)
    }

    /// Several descriptions as one `[n, length]` batch, row after row.
    pub fn batch(&self, texts: &[&str]) -> Result<Vec<i64>> {
        let mut out = Vec::with_capacity(texts.len() * self.length);
        for text in texts {
            out.extend(self.ids(text)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onnx::{EOS_ID, PAD_ID, TINY_VOCAB, tiny_tokenizer_json};

    fn id(word: &str) -> i64 {
        TINY_VOCAB.iter().position(|(w, _)| *w == word).unwrap() as i64
    }

    fn tokens() -> Tokens {
        Tokens::from_json(tiny_tokenizer_json().as_bytes(), 64).unwrap()
    }

    #[test]
    fn descriptions_are_lower_cased_and_padded_to_exactly_64() {
        let tokens = tokens();
        let ids = tokens.ids("A Red SQUARE").unwrap();
        assert_eq!(ids.len(), 64);
        assert_eq!(&ids[..4], [id("a"), id("red"), id("square"), EOS_ID]);
        assert!(ids[4..].iter().all(|&id| id == PAD_ID), "{ids:?}");
        assert_eq!(ids, tokens.ids("a red square").unwrap(), "case makes no difference");
        assert_eq!(tokens.pad_id() as i64, PAD_ID);
    }

    /// The fixture's vocabulary is lower case only, as Gemma's is mostly lower case for the
    /// words SigLIP 2 was trained on: without lower-casing, "Red" would be unknown.
    #[test]
    fn without_lower_casing_the_words_would_be_unknown() {
        let raw = Tokenizer::from_bytes(tiny_tokenizer_json().as_bytes()).unwrap();
        let encoding = raw.encode("Red", true).unwrap();
        assert_eq!(encoding.get_ids(), [id("<unk>") as u32, EOS_ID as u32]);
    }

    #[test]
    fn long_descriptions_are_cut_to_64_keeping_the_end_token() {
        let tokens = tokens();
        let long = vec!["blue"; 100].join(" ");
        let ids = tokens.ids(&long).unwrap();
        assert_eq!(ids.len(), 64);
        assert_eq!(ids[63], EOS_ID, "the special token survives the cut");
        assert!(ids[..63].iter().all(|&i| i == id("blue")));
        let batch = tokens.batch(&["red", "Blue sea"]).unwrap();
        assert_eq!(batch.len(), 128);
        assert_eq!(&batch[64..67], [id("blue"), id("sea"), EOS_ID]);
    }

    #[test]
    fn a_tokenizer_without_a_pad_token_is_refused() {
        let json = tiny_tokenizer_json().replace("<pad>", "<nothing>");
        assert!(Tokens::from_json(json.as_bytes(), 64).is_err());
    }
}
