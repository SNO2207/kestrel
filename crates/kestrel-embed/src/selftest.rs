//! A check that a model sees what it should, run right after a model is installed and whenever
//! the owner asks. It draws its own pictures (a plain red square, a plain blue square, a page
//! of receipt text, a stop sign's word on red) and asks the model to match them with
//! descriptions both ways: each picture's best description must be its own, and each
//! description's best picture too.
//!
//! Mistakes in what the models are fed are silent otherwise: red and blue swapped (BGR for RGB)
//! pairs the squares with the wrong colours; text not lower-cased or not padded to 64 tokens,
//! or pictures not normalised, blur every score until the pairs no longer stand out. It also
//! measures how many pictures a second the model embeds, and where it runs.

use std::time::Instant;

use serde::Serialize;

use crate::{DeviceInfo, Embedder, Result, Rgb, dot};

/// The side the self-test draws at; the model resizes as it would any photo.
const SIDE: u32 = 384;

/// One picture, the description that should be its best match, and its name on the page.
pub struct Sample {
    pub label: &'static str,
    pub prompt: &'static str,
    pub image: Rgb,
}

/// The descriptions, in the order of [`samples`].
pub const PROMPTS: [&str; 4] = ["a red square", "a blue square", "a receipt", "a stop sign"];

/// The pictures the self-test draws.
pub fn samples() -> Result<Vec<Sample>> {
    Ok(vec![
        Sample { label: "Red square", prompt: PROMPTS[0], image: Rgb::filled(SIDE, SIDE, [220, 20, 30]) },
        Sample { label: "Blue square", prompt: PROMPTS[1], image: Rgb::filled(SIDE, SIDE, [25, 40, 215]) },
        Sample {
            label: "Receipt text",
            prompt: PROMPTS[2],
            image: crate::draw::words(SIDE, SIDE, [250, 250, 245], [20, 20, 20], &["STORE #102", "RECEIPT", "MILK 3.49", "BREAD 2.99", "TOTAL 42.17"], 40)?,
        },
        Sample {
            label: "STOP on red",
            prompt: PROMPTS[3],
            image: crate::draw::words(SIDE, SIDE, [200, 16, 32], [255, 255, 255], &["STOP"], 120)?,
        },
    ])
}

/// How one picture or one description fared.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// What was matched: a picture's name, or a description in quotes.
    pub subject: String,
    pub expected: String,
    pub best: String,
    pub passed: bool,
    /// The cosine of the expected pair, and of the best other one.
    pub expected_score: f32,
    pub runner_up: f32,
}

/// What the self-test found.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub passed: bool,
    pub checks: Vec<Check>,
    pub images_per_second: f32,
    pub device: DeviceInfo,
    pub took_ms: u64,
    /// Set when the model couldn't run at all.
    pub error: Option<String>,
}

fn best(scores: &[f32]) -> usize {
    scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |(i, _)| i)
}

fn runner_up(scores: &[f32], except: usize) -> f32 {
    scores.iter().enumerate().filter(|(i, _)| *i != except).map(|(_, &s)| s).fold(f32::NEG_INFINITY, f32::max)
}

/// Runs the self-test, then embeds `throughput_images` pictures to measure the speed.
pub fn run(embedder: &dyn Embedder, throughput_images: usize) -> Report {
    let started = Instant::now();
    let device = embedder.device();
    match checks(embedder) {
        Ok(checks) => {
            let images_per_second = throughput(embedder, throughput_images).unwrap_or(0.0);
            Report {
                passed: checks.iter().all(|check| check.passed),
                checks,
                images_per_second,
                device,
                took_ms: started.elapsed().as_millis() as u64,
                error: None,
            }
        }
        Err(error) => Report {
            passed: false,
            checks: Vec::new(),
            images_per_second: 0.0,
            device,
            took_ms: started.elapsed().as_millis() as u64,
            error: Some(error.0),
        },
    }
}

fn checks(embedder: &dyn Embedder) -> Result<Vec<Check>> {
    let samples = samples()?;
    let images: Vec<Rgb> = samples.iter().map(|sample| sample.image.clone()).collect();
    let pictures = embedder.embed_images(&images)?;
    let texts = embedder.embed_texts(&PROMPTS)?;
    // scores[i][j]: picture i against description j.
    let scores: Vec<Vec<f32>> = pictures.iter().map(|p| texts.iter().map(|t| dot(p, t)).collect()).collect();
    let mut out = Vec::new();
    for (i, sample) in samples.iter().enumerate() {
        let found = best(&scores[i]);
        out.push(Check {
            subject: sample.label.to_owned(),
            expected: format!("\u{201c}{}\u{201d}", PROMPTS[i]),
            best: format!("\u{201c}{}\u{201d}", PROMPTS[found]),
            passed: found == i,
            expected_score: scores[i][i],
            runner_up: runner_up(&scores[i], i),
        });
    }
    for (j, prompt) in PROMPTS.iter().enumerate() {
        let column: Vec<f32> = scores.iter().map(|row| row[j]).collect();
        let found = best(&column);
        out.push(Check {
            subject: format!("\u{201c}{prompt}\u{201d}"),
            expected: samples[j].label.to_owned(),
            best: samples[found].label.to_owned(),
            passed: found == j,
            expected_score: column[j],
            runner_up: runner_up(&column, j),
        });
    }
    Ok(out)
}

/// Pictures a second, preparation included, over `count` photo-sized pictures.
fn throughput(embedder: &dyn Embedder, count: usize) -> Result<f32> {
    if count == 0 {
        return Ok(0.0);
    }
    // Varied pictures of a typical phone photo's shape, so resizing costs what it would.
    let images: Vec<Rgb> = (0..count)
        .map(|i| {
            let mut image = Rgb::filled(1024, 768, [(i * 37 % 255) as u8, (i * 91 % 255) as u8, (i * 53 % 255) as u8]);
            for x in 0..1024 {
                image.set(x, (i as u32 * 13) % 768, [255, 255, 255]);
            }
            image
        })
        .collect();
    // One warm-up batch: the first run compiles the GPU's shaders.
    embedder.embed_images(&images[..images.len().min(4)])?;
    let started = Instant::now();
    embedder.embed_images(&images)?;
    Ok(count as f32 / started.elapsed().as_secs_f32().max(1e-6))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l2_normalize;

    /// An embedder that understands colours and the words it is given, the way a working model
    /// would, unless told to make one of the mistakes the self-test is there to catch.
    struct Fake {
        swap_red_blue: bool,
        ignore_case: bool,
        fail: bool,
    }

    impl Fake {
        fn good() -> Fake {
            Fake { swap_red_blue: false, ignore_case: true, fail: false }
        }
    }

    /// Dimensions: red, blue, paper-white, text detail.
    impl Embedder for Fake {
        fn dim(&self) -> usize {
            4
        }

        fn embed_images(&self, images: &[Rgb]) -> Result<Vec<Vec<f32>>> {
            if self.fail {
                return Err(crate::Error("no device".into()));
            }
            Ok(images
                .iter()
                .map(|image| {
                    let n = (image.width * image.height) as f32;
                    let mean = |c: usize| image.pixels.iter().skip(c).step_by(3).map(|&v| f32::from(v)).sum::<f32>() / n / 255.0;
                    let (mut r, g, mut b) = (mean(0), mean(1), mean(2));
                    if self.swap_red_blue {
                        std::mem::swap(&mut r, &mut b);
                    }
                    let edges = image.pixels.chunks_exact(3).collect::<Vec<_>>().windows(2).filter(|w| w[0][1].abs_diff(w[1][1]) > 100).count();
                    let mut v = vec![r - g, b - g, g, edges as f32 / n * 60.0];
                    l2_normalize(&mut v);
                    v
                })
                .collect())
        }

        fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| {
                    let text = if self.ignore_case { text.to_lowercase() } else { text.to_string() };
                    let has = |word: &str| text.split(' ').any(|w| w == word);
                    let mut v = vec![0.0f32; 4];
                    if has("red") || has("stop") {
                        v[0] += 1.0;
                    }
                    if has("blue") {
                        v[1] += 1.0;
                    }
                    if has("receipt") {
                        v[2] += 1.0;
                        v[3] += 0.5;
                    }
                    if has("stop") {
                        v[3] += 1.0;
                    }
                    if has("square") {
                        v[3] -= 0.3;
                    }
                    l2_normalize(&mut v);
                    v
                })
                .collect())
        }

        fn device(&self) -> DeviceInfo {
            DeviceInfo { gpu: false, name: "Fake".into(), note: None }
        }
    }

    #[test]
    fn a_model_that_sees_correctly_passes() {
        let report = run(&Fake::good(), 8);
        assert!(report.passed, "{report:#?}");
        assert_eq!(report.checks.len(), 8, "four pictures and four descriptions");
        assert!(report.images_per_second > 0.0);
        assert!(report.checks.iter().all(|check| check.expected_score > check.runner_up));
    }

    #[test]
    fn swapped_colour_channels_are_caught() {
        let report = run(&Fake { swap_red_blue: true, ..Fake::good() }, 0);
        assert!(!report.passed);
        let red = report.checks.iter().find(|check| check.subject == "Red square").unwrap();
        assert!(!red.passed);
        assert_eq!(red.best, "\u{201c}a blue square\u{201d}");
    }

    /// A model given words it doesn't know (as upper-case words are to one trained on lower
    /// case) can't tell the descriptions apart, and fails.
    #[test]
    fn descriptions_the_model_cannot_read_are_caught() {
        struct Shouting(Fake);
        impl Embedder for Shouting {
            fn dim(&self) -> usize {
                4
            }
            fn embed_images(&self, images: &[Rgb]) -> Result<Vec<Vec<f32>>> {
                self.0.embed_images(images)
            }
            fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
                let upper: Vec<String> = texts.iter().map(|t| t.to_uppercase()).collect();
                self.0.embed_texts(&upper.iter().map(String::as_str).collect::<Vec<_>>())
            }
            fn device(&self) -> DeviceInfo {
                self.0.device()
            }
        }
        let report = run(&Shouting(Fake { ignore_case: false, ..Fake::good() }), 0);
        assert!(!report.passed);
    }

    #[test]
    fn a_model_that_cannot_run_is_a_failure_with_the_reason() {
        let report = run(&Fake { fail: true, ..Fake::good() }, 4);
        assert!(!report.passed);
        assert_eq!(report.error.as_deref(), Some("no device"));
        assert!(report.checks.is_empty());
    }

    /// The real pipeline on the tiny model (Windows ML, the GPU, preprocessing, the tokenizer):
    /// its colour-and-detail space is made to tell these four pictures apart.
    #[test]
    fn the_tiny_model_passes_on_the_gpu() {
        let dir = std::env::temp_dir().join(format!("kestrel-embed-selftest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::onnx::write_tiny_model(&dir, true).unwrap();
        let model = crate::Siglip::open(&dir, &crate::catalog::fast(), crate::winml::Want::Gpu).unwrap();
        let report = run(&model, 32);
        println!("{report:#?}");
        assert!(report.passed, "{report:#?}");
        assert!(report.device.gpu);
        assert!(report.images_per_second > 1.0);
        drop(model);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
