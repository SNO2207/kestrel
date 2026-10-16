//! Runs an ONNX model through Windows ML (`Windows.AI.MachineLearning`): the ONNX Runtime with
//! DirectML that is part of Windows, documented and serviced with it, so nothing has to be
//! downloaded to run a model. The high-performance GPU is used when it can be; otherwise the
//! CPU, and the session says why.
//!
//! A session on a model that failed to load once crashed the process with an access
//! violation, so [`Session::load`] checks the model loaded and declares its inputs and outputs
//! before it makes a session.

use std::path::Path;

use parking_lot::Mutex;
use windows::AI::MachineLearning::{
    ILearningModelFeatureDescriptor, LearningModel, LearningModelBinding, LearningModelDevice, LearningModelDeviceKind,
    LearningModelFeatureKind, LearningModelSession, LearningModelSessionOptions, TensorFeatureDescriptor, TensorFloat,
    TensorFloat16Bit, TensorInt64Bit, TensorKind,
};
use windows_core::{HSTRING, IInspectable, Interface};

use crate::{DeviceInfo, Error, Result};

/// The element type of a tensor input or output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elem {
    F32,
    F16,
    I64,
    Other(i32),
}

impl Elem {
    fn of(kind: TensorKind) -> Elem {
        match kind {
            TensorKind::Float => Elem::F32,
            TensorKind::Float16 => Elem::F16,
            TensorKind::Int64 => Elem::I64,
            other => Elem::Other(other.0),
        }
    }
}

/// An input or output the model declares. A free dimension (the batch, say) is -1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feature {
    pub name: String,
    pub elem: Elem,
    pub shape: Vec<i64>,
}

/// A tensor to feed the model. Floats go in as f32 whatever the model wants: Windows ML
/// narrows them to float16 when the input is declared so.
pub enum Input<'a> {
    Floats { shape: Vec<i64>, data: &'a [f32] },
    Ints { shape: Vec<i64>, data: &'a [i64] },
}

/// Which device to ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// The high-performance GPU, through DirectML, falling back to the CPU.
    Gpu,
    Cpu,
}

/// A loaded model, ready to run.
pub struct Session {
    session: LearningModelSession,
    pub inputs: Vec<Feature>,
    pub outputs: Vec<Feature>,
    device: DeviceInfo,
    /// Evaluations run one at a time; the GPU is busy anyway.
    lock: Mutex<()>,
}

fn describe(features: windows_collections::IVectorView<ILearningModelFeatureDescriptor>) -> Result<Vec<Feature>> {
    let mut out = Vec::new();
    for feature in features {
        let name = feature.Name()?.to_string_lossy();
        if feature.Kind()? != LearningModelFeatureKind::Tensor {
            out.push(Feature { name, elem: Elem::Other(-1), shape: Vec::new() });
            continue;
        }
        let tensor: TensorFeatureDescriptor = feature.cast()?;
        let shape = tensor.Shape()?.into_iter().collect();
        out.push(Feature { name, elem: Elem::of(tensor.TensorKind()?), shape });
    }
    Ok(out)
}

/// The name Windows gives the graphics adapter a Windows ML device runs on.
fn adapter_name(device: &LearningModelDevice) -> Option<String> {
    use windows::Win32::Foundation::LUID;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory4};
    let id = device.AdapterId().ok()?;
    let luid = LUID { LowPart: id.LowPart, HighPart: id.HighPart };
    // SAFETY: plain DXGI calls with owned interfaces; the description is read from a struct
    // the call fills.
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory1().ok()?;
        let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(luid).ok()?;
        let desc = adapter.GetDesc1().ok()?;
        let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        Some(String::from_utf16_lossy(&desc.Description[..end]))
    }
}

fn create_session(model: &LearningModel, kind: LearningModelDeviceKind) -> Result<(LearningModelSession, LearningModelDevice)> {
    let device = LearningModelDevice::Create(kind)?;
    let options = LearningModelSessionOptions::new()?;
    // The model's own copy of its weights (over a gigabyte for the large text tower) is freed
    // once the session holds what it needs.
    let _ = options.SetCloseModelOnSessionCreation(true);
    let session = LearningModelSession::CreateFromModelOnDeviceWithSessionOptions(model, &device, &options)?;
    Ok((session, device))
}

impl Session {
    /// Loads the model at `path` and makes a session on the device wanted.
    pub fn load(path: &Path, want: Want) -> Result<Session> {
        crate::com();
        if !path.is_file() {
            return Err(Error(format!("{} is missing", path.display())));
        }
        let model = LearningModel::LoadFromFilePath(&HSTRING::from(path))
            .map_err(|error| Error(format!("Windows ML couldn't load {}: {}", path.display(), Error::from(error))))?;
        let inputs = describe(model.InputFeatures()?)?;
        let outputs = describe(model.OutputFeatures()?)?;
        if inputs.is_empty() || outputs.is_empty() {
            return Err(Error(format!("{} declares no inputs or outputs", path.display())));
        }
        let (session, device) = match want {
            Want::Gpu => match create_session(&model, LearningModelDeviceKind::DirectXHighPerformance) {
                Ok((session, device)) => {
                    let name = adapter_name(&device).unwrap_or_else(|| "Graphics card".into());
                    (session, DeviceInfo { gpu: true, name, note: None })
                }
                Err(gpu_error) => {
                    let model = LearningModel::LoadFromFilePath(&HSTRING::from(path))?;
                    let (session, _) = create_session(&model, LearningModelDeviceKind::Cpu)?;
                    let note = format!("Running on the processor: the graphics card couldn't run the model ({gpu_error}).");
                    (session, DeviceInfo { gpu: false, name: "CPU".into(), note: Some(note) })
                }
            },
            Want::Cpu => {
                let (session, _) = create_session(&model, LearningModelDeviceKind::Cpu)?;
                (session, DeviceInfo { gpu: false, name: "CPU".into(), note: None })
            }
        };
        Ok(Session { session, inputs, outputs, device, lock: Mutex::new(()) })
    }

    pub fn device(&self) -> &DeviceInfo {
        &self.device
    }

    pub fn input(&self, name: &str) -> Option<&Feature> {
        self.inputs.iter().find(|feature| feature.name == name)
    }

    pub fn output(&self, name: &str) -> Option<&Feature> {
        self.outputs.iter().find(|feature| feature.name == name)
    }

    /// Runs the model on `inputs` and reads `output` back as f32s, with its shape.
    pub fn run(&self, inputs: &[(&str, Input<'_>)], output: &str) -> Result<(Vec<f32>, Vec<i64>)> {
        let _running = self.lock.lock();
        let binding = LearningModelBinding::CreateFromSession(&self.session)?;
        for (name, input) in inputs {
            let feature = self.input(name).ok_or_else(|| Error(format!("the model has no input {name}")))?;
            let value: IInspectable = match (input, feature.elem) {
                (Input::Floats { shape, data }, Elem::F32) => TensorFloat::CreateFromShapeArrayAndDataArray(shape, data)?.cast()?,
                (Input::Floats { shape, data }, Elem::F16) => TensorFloat16Bit::CreateFromShapeArrayAndDataArray(shape, data)?.cast()?,
                (Input::Ints { shape, data }, Elem::I64) => TensorInt64Bit::CreateFromShapeArrayAndDataArray(shape, data)?.cast()?,
                (_, elem) => return Err(Error(format!("the model's input {name} is {elem:?}, which Kestrel doesn't feed"))),
            };
            binding.Bind(&HSTRING::from(*name), &value)?;
        }
        let result = self.session.Evaluate(&binding, &HSTRING::new())?;
        if !result.Succeeded()? {
            return Err(Error(format!("the model failed to run (0x{:08X})", result.ErrorStatus()?)));
        }
        let value = result.Outputs()?.Lookup(&HSTRING::from(output))?;
        if let Ok(tensor) = value.cast::<TensorFloat>() {
            let shape = tensor.Shape()?.into_iter().collect();
            let view = tensor.GetAsVectorView()?;
            let mut data = vec![0f32; view.Size()? as usize];
            view.GetMany(0, &mut data)?;
            return Ok((data, shape));
        }
        if let Ok(tensor) = value.cast::<TensorFloat16Bit>() {
            let shape = tensor.Shape()?.into_iter().collect();
            let view = tensor.GetAsVectorView()?;
            let mut data = vec![0f32; view.Size()? as usize];
            view.GetMany(0, &mut data)?;
            return Ok((data, shape));
        }
        Err(Error(format!("the model's output {output} is not floating point")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onnx;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kestrel-embed-winml-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Two 4 x 4 pictures, normalised, NCHW: one with a bright cross on dark, one flat.
    fn pixels() -> Vec<f32> {
        let mut data = Vec::new();
        for image in 0..2 {
            for channel in 0..3 {
                for y in 0..4 {
                    for x in 0..4 {
                        let v = if image == 0 && (x == 1 || y == 2) { 0.8 } else { -0.5 + 0.25 * channel as f32 };
                        data.push(v);
                    }
                }
            }
        }
        data
    }

    /// The tiny vision tower's output for `pixels()`, worked out by hand.
    fn expected_image(data: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        for image in data.chunks_exact(48) {
            let mean = |c: usize| image[c * 16..c * 16 + 16].iter().sum::<f32>() / 16.0;
            let bright = |x: usize, y: usize| (0..3).map(|c| image[c * 16 + y * 4 + x]).sum::<f32>() / 3.0;
            let mut detail = 0.0;
            for y in 1..3 {
                for x in 1..3 {
                    let lap = bright(x - 1, y) + bright(x + 1, y) + bright(x, y - 1) + bright(x, y + 1) - 4.0 * bright(x, y);
                    detail += lap.abs();
                }
            }
            out.extend([mean(0), mean(1), mean(2), detail / 4.0 * onnx::TEXTURE_WEIGHT]);
        }
        out
    }

    fn check_tiny_towers(want: Want, half: bool) {
        let dir = scratch(&format!("{want:?}-{half}"));
        let vision_path = dir.join("vision.onnx");
        let text_path = dir.join("text.onnx");
        std::fs::write(&vision_path, onnx::tiny_vision(4, half)).unwrap();
        std::fs::write(&text_path, onnx::tiny_text(half)).unwrap();
        let tolerance = if half { 2e-2 } else { 1e-5 };

        let vision = Session::load(&vision_path, want).unwrap();
        println!("{want:?} half={half}: {:?}", vision.device());
        assert_eq!(vision.device().gpu, want == Want::Gpu, "{:?}", vision.device());
        let input = vision.input("pixel_values").unwrap();
        assert_eq!(input.shape, [-1, 3, 4, 4], "the batch is free");
        assert_eq!(input.elem, if half { Elem::F16 } else { Elem::F32 });
        assert!(vision.output("last_hidden_state").is_some());
        let data = pixels();
        let (out, shape) =
            vision.run(&[("pixel_values", Input::Floats { shape: vec![2, 3, 4, 4], data: &data })], "pooler_output").unwrap();
        assert_eq!(shape, [2, 4]);
        for (got, want) in out.iter().zip(expected_image(&data)) {
            assert!((got - want).abs() <= tolerance * (1.0 + want.abs()), "{out:?} vs {:?}", expected_image(&data));
        }

        let text = Session::load(&text_path, want).unwrap();
        let ids: Vec<i64> = [3, 8, 16, 1].into_iter().chain([0; 4]).chain([9, 10, 1].into_iter().chain([0; 5])).collect();
        let (out, shape) = text.run(&[("input_ids", Input::Ints { shape: vec![2, 8], data: &ids })], "pooler_output").unwrap();
        assert_eq!(shape, [2, 4]);
        let expected: Vec<f32> = ids.chunks_exact(8).flat_map(onnx::tiny_text_features).collect();
        for (got, want) in out.iter().zip(&expected) {
            assert!((got - want).abs() <= tolerance * (1.0 + want.abs()), "{out:?} vs {expected:?}");
        }
        // "red square": the mean of red and square over 8 positions, by hand.
        assert!((expected[0] - 1.0 / 8.0).abs() < 1e-6 && (expected[3] + 0.5 / 8.0).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tiny_towers_run_on_the_gpu_as_computed_by_hand() {
        check_tiny_towers(Want::Gpu, false);
        check_tiny_towers(Want::Gpu, true);
    }

    #[test]
    fn tiny_towers_run_on_the_cpu_as_computed_by_hand() {
        check_tiny_towers(Want::Cpu, false);
        check_tiny_towers(Want::Cpu, true);
    }

    /// A file that is not a model is refused before any session is made: a session on a model
    /// that failed to load crashed the process once.
    #[test]
    fn a_broken_model_is_an_error_not_a_crash() {
        let dir = scratch("broken");
        let path = dir.join("broken.onnx");
        std::fs::write(&path, b"this is not a model").unwrap();
        let error = Session::load(&path, Want::Gpu).err().expect("refused");
        assert!(error.0.contains("couldn't load"), "{error}");
        assert!(Session::load(&dir.join("missing.onnx"), Want::Cpu).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
