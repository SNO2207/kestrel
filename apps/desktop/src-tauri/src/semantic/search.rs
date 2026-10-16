//! Resolving descriptions in searches: `like:"a dog on a sofa"`, a query that starts with `~`,
//! and `like:"C:\Photos\dog.jpg"` ("Find similar"), for the engine, as tags are resolved.
//!
//! In two steps, so that the index is held only for the second: [`prepare`] runs the model (the
//! description through the text tower, or the picture's stored vectors) and scans the vector
//! store, and looks the description's words up in the text read from photos; then
//! [`Prepared::resolve`], with the index held, turns files into entries and scores each:
//!
//!   score = how alike it looks (cosine) + span x (0.6 x share of the words in its text
//!                                                + 0.4 x share of the words in its name)
//!
//! where `span` is the width of the model's range for the slider, so a photo whose text and name
//! hold every word moves from one end of the range to the other. A photo found only by its text
//! (not described yet) counts as looking as alike as the range's loose end. The owner's slider
//! sets the threshold within the range (`catalog::Cutoffs`); results come best first.

use std::collections::HashMap;

use kestrel_db::semantic::{self as data, TextHit};
use kestrel_embed::catalog::ModelSpec;
use kestrel_embed::{Embedder, Rgb, video};
use kestrel_index::{EntryId, Index, fold, walk};
use kestrel_query::{Clock, ClauseKind, ParseError, Query, likes, parse, resolve_likes};
use kestrel_vector::FileHit;

use super::stage::{MediaKind, is_placeholder, kind_of, on_disk, volume_code};
use super::{Loaded, Semantic};

/// Rows the store returns at most for one description.
const HITS: usize = 5000;
/// Photos looked up by their text at most.
const TEXT_HITS: usize = 2000;
const TEXT_WEIGHT: f32 = 0.6;
const NAME_WEIGHT: f32 = 0.4;
/// Words that say nothing about what a photo shows.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "of", "on", "in", "at", "with", "and", "or", "to", "for", "from", "by", "is", "are", "my", "some", "photo",
    "photos", "picture", "pictures", "image", "images", "video", "videos",
];

/// A file the description found, and how.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Candidate {
    /// Cosine similarity, when the file has vectors.
    sight: Option<f32>,
    /// For a video, the moment that matched best.
    at_ms: Option<u32>,
    /// Share of the description's words in the file's text.
    text: f32,
}

/// One description, ready to resolve.
#[derive(Debug)]
struct Like {
    description: String,
    threshold: f32,
    span: f32,
    /// The range's loose end: how alike a file found only by its text counts as looking.
    loose: f32,
    words: Vec<String>,
    candidates: HashMap<(u32, u64), Candidate>,
}

/// Everything a search's descriptions need from the model, gathered before the index is held.
#[derive(Debug, Default)]
pub struct Prepared {
    likes: Vec<Like>,
    moments: HashMap<String, u32>,
}

/// The words of a description worth looking for in text and names: lower case, without the
/// words that say nothing.
fn words_of(description: &str) -> Vec<String> {
    let mut words: Vec<String> = description
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '.' || c == '-'))
        .map(|word| word.trim_matches(|c| c == '.' || c == '-').to_owned())
        .filter(|word| (word.chars().count() >= 3 || word.chars().any(|c| c.is_ascii_digit())) && !STOP_WORDS.contains(&word.as_str()))
        .collect();
    words.dedup();
    words
}

/// The share of `words` found in `text` (folded).
fn share(words: &[String], text: &str) -> f32 {
    if words.is_empty() {
        return 0.0;
    }
    let text = fold(text);
    words.iter().filter(|word| text.contains(word.as_str())).count() as f32 / words.len() as f32
}

/// Whether a description is a picture's path ("Find similar") rather than words.
fn is_picture(description: &str) -> bool {
    let bytes = description.as_bytes();
    let rooted = (bytes.len() > 3 && bytes[1] == b':' && bytes[2] == b'\\') || description.starts_with(r"\\");
    rooted && description.rsplit_once('.').is_some_and(|(_, ext)| kind_of(ext).is_some())
}

impl Like {
    /// A description's candidates: the store's hits (best row of each file) and, for words, the
    /// files whose text holds some of them. `range` is the model's (fewer, more) thresholds and
    /// `cutoff` the slider, 0 for fewer to 1 for more.
    fn new(description: &str, range: (f32, f32), cutoff: f32, hits: &[FileHit], texts: &[TextHit]) -> Like {
        let (fewer, more) = range;
        let span = (fewer - more).max(1e-6);
        let words = if is_picture(description) { Vec::new() } else { words_of(description) };
        let mut candidates: HashMap<(u32, u64), Candidate> = HashMap::new();
        for hit in hits {
            candidates.insert(hit.key.file(), Candidate { sight: Some(hit.score), at_ms: hit.key.at_ms, text: 0.0 });
        }
        for text in texts {
            let found = share(&words, &text.body);
            if found > 0.0 {
                candidates.entry((text.file.volume, text.file.file_id)).or_default().text = found;
            }
        }
        Like { description: description.to_owned(), threshold: fewer - span * cutoff, span, loose: more, words, candidates }
    }

    /// The loosest score a stored vector can have and still reach the threshold with every
    /// boost: the store need return nothing below it.
    fn floor(range: (f32, f32)) -> f32 {
        range.1 - (range.0 - range.1)
    }
}

/// The vector of a picture for "Find similar": its stored vectors (a video's frames averaged),
/// or, for one not described yet, described now. A file only in the cloud is not downloaded.
fn picture_vector(loaded: &Loaded, path: &str) -> Result<Vec<f32>, String> {
    // Attributes first, without opening the file: a placeholder is never opened at all.
    let (attrs, _, _) = on_disk(path).ok_or_else(|| format!("Can't find {path}"))?;
    if is_placeholder(attrs) {
        return Err(format!("{path} is only in the cloud; Kestrel doesn't download it to compare"));
    }
    if let (Some(volume), Some(file_id)) = (volume_code(path), walk::file_id_of(path)) {
        let store = loaded.store.read();
        let rows = store.rows_of(volume, file_id);
        if !rows.is_empty() {
            let mut mean = vec![0f32; store.dim()];
            for row in rows {
                for (sum, v) in mean.iter_mut().zip(store.vector(row).unwrap_or_default()) {
                    *sum += v;
                }
            }
            kestrel_embed::l2_normalize(&mut mean);
            return Ok(mean);
        }
    }
    let side = loaded.side;
    let ext = path.rsplit_once('.').map_or("", |(_, ext)| ext);
    let pictures: Vec<Rgb> = match kind_of(ext) {
        Some(MediaKind::Video) => {
            let mut frames = Vec::new();
            let plan = video::Plan { every_ms: 5000, max_frames: 4, scene_changes: false };
            video::sample(std::path::Path::new(path), &plan, &mut |_, frame| {
                frames.push(kestrel_embed::resize::resize(&frame, side, side));
                true
            })
            .map_err(|e| e.0)?;
            frames
        }
        _ => vec![kestrel_embed::resize::resize(&kestrel_embed::image::decode(std::path::Path::new(path)).map_err(|e| e.0)?, side, side)],
    };
    let vectors = loaded.embedder.embed_images(&pictures).map_err(|e| e.0)?;
    let mut mean = vec![0f32; loaded.embedder.dim()];
    for vector in &vectors {
        for (sum, v) in mean.iter_mut().zip(vector) {
            *sum += v;
        }
    }
    kestrel_embed::l2_normalize(&mut mean);
    Ok(mean)
}

/// Every description in the query, negated ones included (they exclude what looks like them).
fn descriptions(query: &Query) -> Vec<String> {
    let mut out: Vec<String> = query
        .clauses
        .iter()
        .filter_map(|clause| match &clause.kind {
            ClauseKind::Like(description) => Some(description.clone()),
            _ => None,
        })
        .collect();
    out.dedup();
    out
}

/// Runs the model for every description in `text`; nothing at all for a query without one.
pub fn prepare(semantic: &Semantic, text: &str) -> Result<Prepared, String> {
    let Ok(query) = parse(text, Clock::local()) else { return Ok(Prepared::default()) };
    let wanted = descriptions(&query);
    if wanted.is_empty() {
        return Ok(Prepared::default());
    }
    let loaded = semantic.ready()?;
    let cutoff = semantic.cutoff();
    let mut likes = Vec::new();
    for description in wanted {
        likes.push(prepare_one(semantic, &loaded, &loaded.spec, &description, cutoff)?);
    }
    Ok(Prepared { likes, moments: HashMap::new() })
}

fn prepare_one(semantic: &Semantic, loaded: &Loaded, spec: &ModelSpec, description: &str, cutoff: f32) -> Result<Like, String> {
    let picture = is_picture(description);
    let (range, vector) = if picture {
        (spec.cutoffs.image, picture_vector(loaded, description)?)
    } else {
        let vectors = loaded.embedder.embed_texts(&[description]).map_err(|e| e.0)?;
        (spec.cutoffs.text, vectors.into_iter().next().unwrap_or_default())
    };
    let hits = {
        let store = loaded.store.read();
        let rows = store.search(&vector, HITS, Like::floor(range));
        store.best_per_file(&rows)
    };
    let texts = if picture { Vec::new() } else { data::search_text(&semantic.db, &words_of(description), TEXT_HITS).unwrap_or_default() };
    Ok(Like::new(description, range, cutoff, &hits, &texts))
}

impl Prepared {
    /// Resolves the query's descriptions to the entries that pass the threshold, and returns
    /// their scores for ranking (`None` when the query has no description).
    pub fn resolve(&mut self, query: &mut Query, index: &Index) -> Result<Option<HashMap<EntryId, f32>>, String> {
        if self.likes.is_empty() {
            return Ok(None);
        }
        let ranked = likes(query);
        let mut sums: HashMap<EntryId, (f32, u32)> = HashMap::new();
        let moments = &mut self.moments;
        let all = &self.likes;
        resolve_likes(query, |description| {
            let like = all
                .iter()
                .find(|like| like.description == description)
                .ok_or_else(|| ParseError(format!("\"{description}\" was not prepared")))?;
            let ranks = ranked.iter().any(|d| d == description);
            let mut ids = Vec::new();
            for (&(volume, file_id), candidate) in &like.candidates {
                for id in index.entries_with_file_id(file_id) {
                    if index.is_dir(id) {
                        continue;
                    }
                    let path = index.path(id);
                    if volume_code(&path) != Some(volume) {
                        continue;
                    }
                    let named = share(&like.words, index.name(id));
                    let score = candidate.sight.unwrap_or(like.loose) + like.span * (TEXT_WEIGHT * candidate.text + NAME_WEIGHT * named);
                    if score < like.threshold {
                        continue;
                    }
                    ids.push(id);
                    if ranks {
                        let sum = sums.entry(id).or_insert((0.0, 0));
                        sum.0 += score;
                        sum.1 += 1;
                        if let Some(at) = candidate.at_ms {
                            moments.insert(path.to_lowercase(), at);
                        }
                    }
                }
            }
            Ok(ids)
        })
        .map_err(|e| e.to_string())?;
        Ok(Some(sums.into_iter().map(|(id, (sum, count))| (id, sum / count.max(1) as f32)).collect()))
    }

    /// Where in each video the description matched, by lower-case path.
    pub fn take_moments(&mut self) -> HashMap<String, u32> {
        std::mem::take(&mut self.moments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, SortField, SortSpec};
    use crate::visibility::Visibility;
    use kestrel_db::semantic::FileState;
    use kestrel_index::IndexBuilder;
    use kestrel_vector::Key;

    const RANGE: (f32, f32) = (0.15, 0.03);

    #[test]
    fn words_leave_out_what_says_nothing() {
        assert_eq!(words_of("A photo of a Dog on the sofa"), ["dog", "sofa"]);
        assert_eq!(words_of("Costco receipt, total 42.17"), ["costco", "receipt", "total", "42.17"]);
        assert_eq!(words_of("TV"), Vec::<String>::new(), "two letters say too little");
        assert!(is_picture(r"C:\Photos\dog.JPG") && is_picture(r"\\nas\share\clip.mp4"));
        assert!(!is_picture("a dog") && !is_picture(r"C:\Photos\notes.txt") && !is_picture("dog.jpg"));
        assert_eq!(share(&words_of("red car"), "My Red CAR 2024.jpg"), 1.0);
        assert_eq!(share(&words_of("red car"), "red bike.jpg"), 0.5);
    }

    /// Four photos, a video and a scanned receipt, by name; what the model and the text index
    /// "said" is given here, and the ranking and cutoff are checked against the formula.
    #[test]
    fn hits_are_ranked_by_sight_text_and_name_and_cut_off() {
        let root = std::env::temp_dir().join(format!("kestrel-semantic-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let names = ["dog on sofa.jpg", "IMG_0001.jpg", "IMG_0002.jpg", "cat.jpg", "walk.mp4", "scan.png"];
        for name in names {
            std::fs::write(root.join(name), b"x").unwrap();
        }
        let id_of = |name: &str| walk::file_id_of(root.join(name).to_str().unwrap()).unwrap();
        let volume = volume_code(root.to_str().unwrap()).unwrap();
        let key = |name: &str, at_ms: Option<u32>| Key { volume, file_id: id_of(name), size: 1, mtime: 1, at_ms };
        let hit = |name: &str, score: f32, at: Option<u32>| FileHit { key: key(name, at), row: 0, score };
        // By sight: IMG_0001 0.12, "dog on sofa" 0.06 (its name holds both words), IMG_0002 0.05,
        // the video 0.10 at 4 s, the cat 0.02 (below the loose end).
        let hits = [
            hit("IMG_0001.jpg", 0.12, None),
            hit("walk.mp4", 0.10, Some(4000)),
            hit("dog on sofa.jpg", 0.06, None),
            hit("IMG_0002.jpg", 0.05, None),
            hit("cat.jpg", 0.02, None),
        ];
        // The scan was never described, but its text says "dog".
        let texts = [TextHit {
            file: FileState { volume, file_id: id_of("scan.png"), size: 1, mtime: 1 },
            body: "DOG LICENCE 2024".into(),
        }];
        let mut builder = IndexBuilder::new();
        walk::walk(root.to_str().unwrap(), &mut builder).unwrap();
        let engine = Engine::default();
        engine.install(builder.finish());

        let search = |cutoff: f32| -> (Vec<String>, HashMap<String, u32>) {
            let like = Like::new("dog on a sofa", RANGE, cutoff, &hits, &texts);
            let mut prepared = Prepared { likes: vec![like], moments: HashMap::new() };
            let by_name = SortSpec { key: SortField::Name, desc: false };
            let listing = engine
                .search_ranked("~dog on a sofa", Some(root.to_str().unwrap()), by_name, Visibility::ALL, |query, index| prepared.resolve(query, index))
                .unwrap();
            let rows = engine.rows(listing.set, 0, 100).unwrap().into_iter().map(|row| row.name).collect();
            (rows, prepared.take_moments())
        };
        // Middle of the slider: threshold 0.15 - 0.12 x 0.5 = 0.09.
        //   dog on sofa: 0.06 + 0.12 x 0.4 x 1 = 0.108; IMG_0001 0.12; the video 0.10;
        //   the scan: 0.03 + 0.12 x 0.6 x (1/2: "dog" of "dog", "sofa") = 0.066 (out);
        //   IMG_0002 0.05 (out), the cat 0.02 (out).
        let (rows, moments) = search(0.5);
        assert_eq!(rows, ["IMG_0001.jpg", "dog on sofa.jpg", "walk.mp4"]);
        let video = root.join("walk.mp4").to_str().unwrap().to_lowercase();
        assert_eq!(moments.get(&video), Some(&4000), "{moments:?}");
        // Fewer: threshold 0.15 - 0.12 x 0.1 = 0.138: nothing reaches it but... nothing.
        assert!(search(0.1).0.is_empty());
        // More: threshold 0.15 - 0.12 = 0.03: the scan (0.066), IMG_0002 (0.05) join; the cat
        // (0.02) still doesn't.
        assert_eq!(search(1.0).0, ["IMG_0001.jpg", "dog on sofa.jpg", "walk.mp4", "scan.png", "IMG_0002.jpg"]);

        // A plain search is not ranked and needs no model.
        let plain = engine.search_ranked("IMG", Some(root.to_str().unwrap()), SortSpec { key: SortField::Name, desc: true }, Visibility::ALL, |_, _| Ok(None)).unwrap();
        let names: Vec<String> = engine.rows(plain.set, 0, 10).unwrap().into_iter().map(|row| row.name).collect();
        assert_eq!(names, ["IMG_0002.jpg", "IMG_0001.jpg"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_negated_description_excludes_without_ranking() {
        let root = std::env::temp_dir().join(format!("kestrel-semantic-negated-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for name in ["a.jpg", "b.jpg"] {
            std::fs::write(root.join(name), b"x").unwrap();
        }
        let volume = volume_code(root.to_str().unwrap()).unwrap();
        let hit = |name: &str, score: f32| FileHit {
            key: Key { volume, file_id: walk::file_id_of(root.join(name).to_str().unwrap()).unwrap(), size: 1, mtime: 1, at_ms: None },
            row: 0,
            score,
        };
        let mut builder = IndexBuilder::new();
        walk::walk(root.to_str().unwrap(), &mut builder).unwrap();
        let engine = Engine::default();
        engine.install(builder.finish());
        let mut prepared = Prepared {
            likes: vec![Like::new("cat", RANGE, 0.5, &[hit("b.jpg", 0.2)], &[])],
            moments: HashMap::new(),
        };
        let listing = engine
            .search_ranked("jpg !like:cat", Some(root.to_str().unwrap()), SortSpec { key: SortField::Name, desc: false }, Visibility::ALL, |query, index| {
                prepared.resolve(query, index)
            })
            .unwrap();
        let names: Vec<String> = engine.rows(listing.set, 0, 10).unwrap().into_iter().map(|row| row.name).collect();
        assert_eq!(names, ["a.jpg"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
