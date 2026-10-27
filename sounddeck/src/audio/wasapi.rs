use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::Sender;
use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use super::decode::SAMPLE_RATE;
use super::engine::{AudioEvent, Ring, Slot};

const WAVE_FORMAT_EXTENSIBLE: u32 = 0xFFFE;
const WAVE_FORMAT_IEEE_FLOAT: u32 = 3;
const WAVE_FORMAT_PCM: u32 = 1;

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct DeviceList {
    pub devices: Vec<DeviceInfo>,
    pub default_id: Option<String>,
}

/// Enumerate WASAPI render endpoints on a dedicated COM thread.
pub fn list_devices() -> Result<DeviceList, String> {
    let handle = thread::Builder::new()
        .name("sounddeck-devices".into())
        .spawn(|| unsafe {
            let _com = ComGuard::new()?;
            enumerate_inner()
        })
        .map_err(|e| format!("Could not start audio device thread: {e}"))?;

    handle
        .join()
        .map_err(|_| "Audio device enumeration panicked".to_string())?
}

struct ComGuard {
    initialized: bool,
}

impl ComGuard {
    fn new() -> Result<Self, String> {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_ok() {
                Ok(Self { initialized: true })
            } else {
                Err(format!("COM initialization failed: {}", hr.message()))
            }
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

fn err(e: windows::core::Error) -> String {
    e.message().to_string()
}

unsafe fn enumerate_inner() -> Result<DeviceList, String> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(
        &MMDeviceEnumerator,
        None::<&windows::core::IUnknown>,
        CLSCTX_ALL,
    )
    .map_err(err)?;
    let collection = enumerator
        .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
        .map_err(err)?;
    let count = collection.GetCount().map_err(err)?;

    let mut devices = Vec::new();
    for index in 0..count {
        let device = match collection.Item(index) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let id = match device_id(&device) {
            Ok(id) => id,
            Err(_) => continue,
        };
        let name = device_name(&device).unwrap_or_else(|| format!("Output device {}", index + 1));
        devices.push(DeviceInfo { id, name });
    }

    let default_id = enumerator
        .GetDefaultAudioEndpoint(eRender, eConsole)
        .ok()
        .and_then(|d| device_id(&d).ok());

    Ok(DeviceList {
        devices,
        default_id,
    })
}

unsafe fn device_id(device: &IMMDevice) -> Result<String, String> {
    let raw = device.GetId().map_err(err)?;
    let id = raw.to_string().map_err(|e| e.to_string());
    CoTaskMemFree(Some(raw.0 as *const c_void));
    id
}

unsafe fn device_name(device: &IMMDevice) -> Option<String> {
    let store = device.OpenPropertyStore(STGM_READ).ok()?;
    let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
    let raw = PropVariantToStringAlloc(&value).ok()?;
    let name = raw.to_string().ok();
    CoTaskMemFree(Some(raw.0 as *const c_void));
    name
}

#[derive(Clone, Copy, Debug)]
enum SampleKind {
    F32,
    F64,
    I16,
    I24,
    I32,
}

impl SampleKind {
    fn label(self) -> &'static str {
        match self {
            Self::F32 => "float32",
            Self::F64 => "float64",
            Self::I16 => "int16",
            Self::I24 => "int24",
            Self::I32 => "int32",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Format {
    channels: usize,
    rate: u32,
    block_align: usize,
    bytes_per_sample: usize,
    kind: SampleKind,
}

unsafe fn parse_format(raw: *const WAVEFORMATEX) -> Result<Format, String> {
    if raw.is_null() {
        return Err("Audio device returned no format".into());
    }
    let base = &*raw;
    if base.nChannels == 0 || base.nSamplesPerSec == 0 || base.nBlockAlign == 0 {
        return Err("Audio device returned an invalid format".into());
    }

    let mut tag = base.wFormatTag as u32;
    if tag == WAVE_FORMAT_EXTENSIBLE {
        if base.cbSize < 22 {
            return Err("Invalid WAVEFORMATEXTENSIBLE header".into());
        }
        let ext = &*(raw as *const WAVEFORMATEXTENSIBLE);
        let subformat = std::ptr::read_unaligned(std::ptr::addr_of!(ext.SubFormat));
        let float_guid = GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);
        let pcm_guid = GUID::from_u128(0x0000_0001_0000_0010_8000_00aa_0038_9b71);
        if subformat == float_guid {
            tag = WAVE_FORMAT_IEEE_FLOAT;
        } else if subformat == pcm_guid {
            tag = WAVE_FORMAT_PCM;
        } else {
            return Err(format!("Unsupported device subformat {:?}", subformat));
        }
    }

    let bits = base.wBitsPerSample as u32;
    let kind = match (tag, bits) {
        (t, 32) if t == WAVE_FORMAT_IEEE_FLOAT => SampleKind::F32,
        (t, 64) if t == WAVE_FORMAT_IEEE_FLOAT => SampleKind::F64,
        (t, 16) if t == WAVE_FORMAT_PCM => SampleKind::I16,
        (t, 24) if t == WAVE_FORMAT_PCM => SampleKind::I24,
        (t, 32) if t == WAVE_FORMAT_PCM => SampleKind::I32,
        _ => {
            return Err(format!(
                "Unsupported device format: tag {tag}, {bits} bits/channel"
            ))
        }
    };

    let channels = base.nChannels as usize;
    let block_align = base.nBlockAlign as usize;
    let bytes_per_sample = block_align / channels;
    if bytes_per_sample == 0 {
        return Err("Invalid audio block alignment".into());
    }

    Ok(Format {
        channels,
        rate: base.nSamplesPerSec,
        block_align,
        bytes_per_sample,
        kind,
    })
}

/// Spawn the render client for one output device. The thread owns the WASAPI
/// stream and pulls canonical 48 kHz stereo samples out of `ring`.
pub fn spawn_render(
    device_id: String,
    device_name: String,
    slot: Slot,
    ring: Arc<Ring>,
    events: Sender<AudioEvent>,
    stop: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    ring.add_consumer();
    let consumer_ring = ring.clone();
    let result = thread::Builder::new()
        .name(format!("sounddeck-render-{}", slot.index()))
        .spawn(move || {
            crate::audio::raise_thread_priority();
            let _consumer = ConsumerGuard(consumer_ring.clone());
            match render_loop(
                &device_id,
                &device_name,
                slot,
                &consumer_ring,
                &events,
                &stop,
            ) {
                Ok(()) => {
                    if !stop.load(Ordering::SeqCst) {
                        let _ = events.send(AudioEvent::Stopped {
                            slot,
                            name: device_name,
                        });
                    }
                }
                Err(message) => {
                    let _ = events.send(AudioEvent::DeviceError {
                        slot,
                        name: device_name,
                        message,
                    });
                }
            }
        });
    if result.is_err() {
        ring.remove_consumer();
    }
    result.ok()
}

struct ConsumerGuard(Arc<Ring>);

impl Drop for ConsumerGuard {
    fn drop(&mut self) {
        self.0.remove_consumer();
    }
}

fn render_loop(
    device_id: &str,
    device_name: &str,
    slot: Slot,
    ring: &Ring,
    events: &Sender<AudioEvent>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let _com = ComGuard::new()?;

    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(
            &MMDeviceEnumerator,
            None::<&windows::core::IUnknown>,
            CLSCTX_ALL,
        )
        .map_err(err)?;
        let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
        let device = enumerator
            .GetDevice(PCWSTR(wide.as_ptr()))
            .map_err(|e| format!("Device unavailable: {e}"))?;

        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("Could not open audio device: {e}"))?;

        let mix_ptr = client.GetMixFormat().map_err(err)?;
        let format = match parse_format(mix_ptr) {
            Ok(f) => f,
            Err(e) => {
                CoTaskMemFree(Some(mix_ptr as *const c_void));
                return Err(e);
            }
        };

        let initialize = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            0,
            0,
            mix_ptr,
            None,
        );
        CoTaskMemFree(Some(mix_ptr as *const c_void));
        initialize.map_err(|e| format!("Could not initialize audio stream: {e}"))?;

        let buffer_size = client
            .GetBufferSize()
            .map_err(|e| format!("Could not query audio buffer: {e}"))?;
        let render: IAudioRenderClient = client
            .GetService()
            .map_err(|e| format!("Could not get render client: {e}"))?;

        let event = CreateEventW(None, BOOL(0), BOOL(0), PCWSTR::null())
            .map_err(|e| format!("Could not create audio event: {e}"))?;
        client
            .SetEventHandle(event)
            .map_err(|e| format!("Could not attach audio event: {e}"))?;
        client
            .Start()
            .map_err(|e| format!("Could not start audio stream: {e}"))?;

        let _ = events.send(AudioEvent::DeviceStarted {
            slot,
            name: device_name.to_string(),
            detail: format!(
                "{} Hz | {} ch | {}",
                format.rate,
                format.channels,
                format.kind.label()
            ),
        });

        let max_frames = buffer_size as usize;
        let mut stereo = vec![0.0f32; max_frames * 2];
        let mut source: VecDeque<f32> = VecDeque::with_capacity(32_768);
        let mut position = 0.0f64;
        let step = SAMPLE_RATE as f64 / format.rate as f64;

        let mut result = Ok(());
        let mut timeouts = 0u32;
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }

            let wait = WaitForSingleObject(event, 50);
            if wait == WAIT_TIMEOUT {
                // A live device signals the event continuously. Forty silent
                // timeouts (~2 s) mean the endpoint is gone, not slow.
                timeouts += 1;
                if timeouts > 40 {
                    result = Err("Audio device stopped responding".into());
                    break;
                }
                continue;
            }
            timeouts = 0;
            if wait != WAIT_OBJECT_0 {
                result = Err("Audio device stopped responding".into());
                break;
            }

            let padding = match client.GetCurrentPadding() {
                Ok(p) => p,
                Err(e) => {
                    result = Err(format!("Audio device error: {e}"));
                    break;
                }
            };
            let frames = buffer_size.saturating_sub(padding) as usize;
            if frames == 0 {
                continue;
            }

            let needed = ((position + frames as f64 * step).ceil() as usize + 2).max(4);
            while source.len() / 2 < needed {
                let want = (needed - source.len() / 2).min(9_600);
                let got = ring.pop_into(&mut source, want);
                crate::stats::ring_pop(slot.index(), got);
                if got == 0 {
                    // No sample data available for the rest of this cycle: pad
                    // with silence, but keep the queue's timeline intact.
                    let missing = needed - source.len() / 2;
                    source.extend(std::iter::repeat(0.0).take(missing * 2));
                    break;
                }
            }

            let available = source.len() / 2;
            stereo[..frames * 2].fill(0.0);
            if available == 0 {
                // Idle: the buffer is already silence, so skip the per-frame
                // resampling loop (it runs ~200x/second across the two outputs).
                position += step * frames as f64;
            } else {
                for frame in 0..frames {
                    let index = position as usize;
                    let frac = (position - index as f64) as f32;
                    let (left, right) = if index + 1 < available {
                        let a = index * 2;
                        let b = a + 2;
                        (
                            source[a] + (source[b] - source[a]) * frac,
                            source[a + 1] + (source[b + 1] - source[a + 1]) * frac,
                        )
                    } else if index < available {
                        (source[index * 2], source[index * 2 + 1])
                    } else {
                        (0.0, 0.0)
                    };
                    stereo[frame * 2] = left;
                    stereo[frame * 2 + 1] = right;
                    position += step;
                }
            }

            let consumed = (position as usize).saturating_sub(2);
            if consumed > 4096 {
                let drain = consumed.min(source.len() / 2);
                source.drain(..drain * 2);
                position -= drain as f64;
            }

            let data = match render.GetBuffer(frames as u32) {
                Ok(data) => data,
                Err(e) => {
                    result = Err(format!("Could not get audio buffer: {e}"));
                    break;
                }
            };
            write_output(&format, &stereo, frames, data);
            if let Err(e) = render.ReleaseBuffer(frames as u32, 0) {
                result = Err(format!("Could not release audio buffer: {e}"));
                break;
            }
        }

        let _ = client.Stop();
        let _ = CloseHandle(event);
        result
    }
}

unsafe fn write_output(format: &Format, stereo: &[f32], frames: usize, dst: *mut u8) {
    let channels = format.channels;
    let bytes = format.bytes_per_sample;
    for frame in 0..frames {
        let left = stereo[frame * 2].clamp(-1.0, 1.0);
        let right = stereo[frame * 2 + 1].clamp(-1.0, 1.0);
        let base = dst.add(frame * format.block_align);
        for channel in 0..channels {
            let value = if channels == 1 {
                (left + right) * 0.5
            } else if channel == 0 {
                left
            } else if channel == 1 {
                right
            } else {
                0.0
            };
            let target = base.add(channel * bytes);
            match format.kind {
                SampleKind::F32 => (target as *mut f32).write_unaligned(value),
                SampleKind::F64 => (target as *mut f64).write_unaligned(value as f64),
                SampleKind::I16 => {
                    (target as *mut i16).write_unaligned((value * i16::MAX as f32) as i16)
                }
                SampleKind::I32 => {
                    (target as *mut i32).write_unaligned((value as f64 * i32::MAX as f64) as i32)
                }
                SampleKind::I24 => {
                    let sample = (value as f64 * 8_388_607.0) as i32;
                    let bytes = sample.to_le_bytes();
                    target.write(bytes[0]);
                    target.add(1).write(bytes[1]);
                    target.add(2).write(bytes[2]);
                }
            }
        }
    }
}
