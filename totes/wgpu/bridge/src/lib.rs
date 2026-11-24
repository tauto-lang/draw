#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use wgpu as w;

type R<T> = Result<T, String>;
static NEXT: AtomicU64 = AtomicU64::new(1);

enum Resource {
    Buffer(w::Buffer),
    Texture(w::Texture),
    Pipeline(w::RenderPipeline),
    Window(w::Surface<'static>, w::SurfaceConfiguration, bool),
}

pub struct Device {
    gpu: w::Device,
    queue: w::Queue,
    uncaptured: Arc<Mutex<Option<String>>>,
    resources: HashMap<u64, Resource>,
}

impl Device {
    fn check_pending(&self) -> R<()> {
        let mut pending = self.uncaptured.lock().unwrap_or_else(|e| e.into_inner());
        match pending.take() {
            Some(message) => Err(message),
            None => Ok(()),
        }
    }

    fn keep(&mut self, value: Resource) -> R<u64> {
        let id = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| "resource identifiers exhausted")?;
        self.resources.insert(id, value);
        Ok(id)
    }
    fn texture(&self, id: u64) -> R<&w::Texture> {
        match self.resources.get(&id) {
            Some(Resource::Texture(t)) => Ok(t),
            _ => Err("unknown texture on this device".into()),
        }
    }
    fn format(&self, id: u64) -> R<w::TextureFormat> {
        match self.resources.get(&id) {
            Some(Resource::Window(_, cfg, _)) => Ok(cfg.format),
            _ => Ok(self.texture(id)?.format()),
        }
    }
}

fn checked<T>(gpu: &w::Device, operation: impl FnOnce() -> R<T>) -> R<T> {
    for filter in [
        w::ErrorFilter::OutOfMemory,
        w::ErrorFilter::Internal,
        w::ErrorFilter::Validation,
    ] {
        gpu.push_error_scope(filter);
    }
    let result = operation();
    let mut error = None;
    for _ in 0..3 {
        if let Some(e) = pollster::block_on(gpu.pop_error_scope()) {
            error.get_or_insert_with(|| e.to_string());
        }
    }
    match error {
        Some(e) => Err(e),
        None => result,
    }
}

fn connect(instance: &w::Instance, surface: Option<&w::Surface<'_>>) -> R<(Device, w::Adapter)> {
    let mut errors = Vec::new();
    for fallback in [false, true] {
        let adapter =
            match pollster::block_on(instance.request_adapter(&w::RequestAdapterOptions {
                compatible_surface: surface,
                force_fallback_adapter: fallback,
                ..Default::default()
            })) {
                Ok(a) => a,
                Err(e) => {
                    errors.push(format!("adapter (fallback={fallback}): {e}"));
                    continue;
                }
            };
        let descriptor = w::DeviceDescriptor {
            required_limits: w::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        };
        match pollster::block_on(adapter.request_device(&descriptor)) {
            Ok((gpu, queue)) => {
                let uncaptured = Arc::new(Mutex::new(None));
                let pending = Arc::clone(&uncaptured);
                gpu.on_uncaptured_error(Box::new(move |error| {
                    let mut slot = pending.lock().unwrap_or_else(|e| e.into_inner());
                    slot.get_or_insert_with(|| error.to_string());
                }));
                return Ok((
                    Device {
                        gpu,
                        queue,
                        uncaptured,
                        resources: HashMap::new(),
                    },
                    adapter,
                ));
            }
            Err(e) => errors.push(format!("request_device (fallback={fallback}): {e}")),
        }
    }
    Err(format!(
        "利用できる GPU デバイスがありません: {}",
        errors.join("\n")
    ))
}

pub fn open() -> R<Device> {
    Ok(connect(&w::Instance::default(), None)?.0)
}

/// Call on the window-system thread (the main thread on macOS).
/// The surface owns its window handle provider for the entire presentation lifetime.
pub fn open_window(
    window: impl Into<w::SurfaceTarget<'static>>,
    width: u32,
    height: u32,
) -> R<(Device, u64)> {
    let instance = w::Instance::default();
    let surface = instance.create_surface(window).map_err(|e| e.to_string())?;
    let (mut d, adapter) = connect(&instance, Some(&surface))?;
    let mut cfg = surface
        .get_default_config(&adapter, width, height)
        .ok_or("unsupported surface")?;
    cfg.format = linear_format(&surface.get_capabilities(&adapter).formats)?;
    configure(&d.gpu, &surface, &cfg)?;
    let id = d.keep(Resource::Window(surface, cfg, true))?;
    Ok((d, id))
}
fn linear_format(formats: &[w::TextureFormat]) -> R<w::TextureFormat> {
    formats
        .iter()
        .copied()
        .find(|f| {
            matches!(
                f,
                w::TextureFormat::Bgra8Unorm | w::TextureFormat::Rgba8Unorm
            )
        })
        .ok_or_else(|| "surface has no linear RGBA8 or BGRA8 format".into())
}

pub fn window_target(d: &mut Device) -> R<u64> {
    d.check_pending()?;
    d.resources
        .iter()
        .find_map(|(&id, resource)| matches!(resource, Resource::Window(..)).then_some(id))
        .ok_or_else(|| "device has no window target".into())
}

fn configure(gpu: &w::Device, surface: &w::Surface<'_>, cfg: &w::SurfaceConfiguration) -> R<()> {
    if cfg.width == 0 || cfg.height == 0 {
        return Err("surface extent must be positive".into());
    }
    checked(gpu, || {
        surface.configure(gpu, cfg);
        Ok(())
    })
}
pub fn resize(d: &mut Device, id: u64, width: u32, height: u32) -> R<()> {
    d.check_pending()?;
    let Some(Resource::Window(surface, cfg, configured)) = d.resources.get_mut(&id) else {
        return Err("unknown window".into());
    };
    let next = w::SurfaceConfiguration {
        width,
        height,
        ..cfg.clone()
    };
    *configured = false;
    configure(&d.gpu, surface, &next)?;
    *configured = true;
    *cfg = next;
    Ok(())
}
pub fn release(d: &mut Device, id: u64) -> R<()> {
    d.check_pending()?;
    d.resources
        .remove(&id)
        .map(drop)
        .ok_or_else(|| "unknown resource on this device".into())
}

pub fn make_buffer(d: &mut Device, bytes: u64, uniform: bool) -> R<u64> {
    d.check_pending()?;
    let usage = if uniform {
        w::BufferUsages::UNIFORM
    } else {
        w::BufferUsages::STORAGE
    };
    let b = checked(&d.gpu, || {
        Ok(d.gpu.create_buffer(&w::BufferDescriptor {
            label: None,
            size: bytes,
            usage: usage | w::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }))
    })?;
    d.keep(Resource::Buffer(b))
}
pub fn upload_buffer(d: &mut Device, id: u64, offset: u64, data: &[u8]) -> R<()> {
    d.check_pending()?;
    let Some(Resource::Buffer(b)) = d.resources.get(&id) else {
        return Err("unknown buffer".into());
    };
    checked(&d.gpu, || {
        d.queue.write_buffer(b, offset, data);
        Ok(())
    })
}
pub fn make_texture(d: &mut Device, width: u32, height: u32, mask: bool) -> R<u64> {
    d.check_pending()?;
    let (format, extra) = if mask {
        (w::TextureFormat::R8Uint, w::TextureUsages::empty())
    } else {
        (
            w::TextureFormat::Rgba8Unorm,
            w::TextureUsages::RENDER_ATTACHMENT | w::TextureUsages::COPY_SRC,
        )
    };
    let t = checked(&d.gpu, || {
        Ok(d.gpu.create_texture(&w::TextureDescriptor {
            label: None,
            size: extent(width, height),
            mip_level_count: 1,
            sample_count: 1,
            dimension: w::TextureDimension::D2,
            format,
            view_formats: &[],
            usage: extra | w::TextureUsages::COPY_DST | w::TextureUsages::TEXTURE_BINDING,
        }))
    })?;
    d.keep(Resource::Texture(t))
}
fn extent(width: u32, height: u32) -> w::Extent3d {
    w::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}
pub fn upload_texture(
    d: &mut Device,
    id: u64,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    data: &[u8],
) -> R<()> {
    d.check_pending()?;
    let t = d.texture(id)?;
    let bpp = if t.format() == w::TextureFormat::R8Uint {
        1
    } else {
        4
    };
    let pitch = width.checked_mul(bpp).ok_or("row too large")?;
    if u64::from(pitch) * u64::from(height) > data.len() as u64 {
        return Err("texture upload data is shorter than the requested rectangle".into());
    }
    checked(&d.gpu, || {
        d.queue.write_texture(
            w::TexelCopyTextureInfo {
                origin: w::Origin3d { x, y, z: 0 },
                ..t.as_image_copy()
            },
            data,
            w::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch),
                rows_per_image: None,
            },
            extent(width, height),
        );
        Ok(())
    })
}

pub fn make_pipeline(d: &mut Device, target: u64, source: &str, blend: bool) -> R<u64> {
    d.check_pending()?;
    let format = d.format(target)?;
    let module = checked(&d.gpu, || {
        Ok(d.gpu.create_shader_module(w::ShaderModuleDescriptor {
            label: None,
            source: w::ShaderSource::Wgsl(source.into()),
        }))
    })?;
    let pipe = checked(&d.gpu, || {
        Ok(d.gpu.create_render_pipeline(&w::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: w::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(w::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(w::ColorTargetState {
                    format,
                    blend: blend.then_some(w::BlendState::ALPHA_BLENDING),
                    write_mask: w::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        }))
    })?;
    d.keep(Resource::Pipeline(pipe))
}

pub fn draw(
    d: &mut Device,
    target: u64,
    pipe: u64,
    ids: &[u64],
    clear: Option<u32>,
    vertices: u32,
    instances: u32,
) -> R<u32> {
    d.check_pending()?;
    let Some(Resource::Pipeline(pipeline)) = d.resources.get(&pipe) else {
        return Err("unknown pipeline".into());
    };
    let frame = match d.resources.get(&target) {
        Some(Resource::Window(_, _, false)) => return Ok(1),
        Some(Resource::Window(s, _, true)) => {
            if clear.is_none() {
                return Err("a newly acquired window frame requires a clear color".into());
            }
            match checked(&d.gpu, || Ok(s.get_current_texture()))? {
                Ok(frame) => Some(frame),
                Err(w::SurfaceError::Outdated) => return Ok(1),
                Err(w::SurfaceError::Lost) => return Ok(2),
                Err(w::SurfaceError::Timeout) => return Ok(3),
                Err(e) => return Err(e.to_string()),
            }
        }
        _ => None,
    };
    let texture = match &frame {
        Some(f) => &f.texture,
        None => d.texture(target)?,
    };
    let group = if ids.is_empty() {
        None
    } else {
        let layout = checked(&d.gpu, || Ok(pipeline.get_bind_group_layout(0)))?;
        let views: Vec<_> = checked(&d.gpu, || {
            Ok(ids
                .iter()
                .map(|id| match d.resources.get(id) {
                    Some(Resource::Texture(t)) => Some(t.create_view(&Default::default())),
                    _ => None,
                })
                .collect())
        })?;
        let entries = ids
            .iter()
            .zip(&views)
            .enumerate()
            .map(|(i, (id, view))| {
                let resource = match (view, d.resources.get(id)) {
                    (Some(v), _) => w::BindingResource::TextureView(v),
                    (_, Some(Resource::Buffer(b))) => b.as_entire_binding(),
                    _ => return Err("unknown shader binding".to_string()),
                };
                Ok(w::BindGroupEntry {
                    binding: u32::try_from(i).map_err(|e| e.to_string())?,
                    resource,
                })
            })
            .collect::<R<Vec<_>>>()?;
        Some(checked(&d.gpu, || {
            Ok(d.gpu.create_bind_group(&w::BindGroupDescriptor {
                label: None,
                layout: &layout,
                entries: &entries,
            }))
        })?)
    };
    checked(&d.gpu, || {
        let view = texture.create_view(&Default::default());
        let mut encoder = d.gpu.create_command_encoder(&Default::default());
        let load = clear.map_or(w::LoadOp::Load, |rgba| {
            let [r, g, b, a] = rgba.to_be_bytes().map(|x| f64::from(x) / 255.0);
            w::LoadOp::Clear(w::Color { r, g, b, a })
        });
        {
            let mut pass = encoder.begin_render_pass(&w::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(w::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: w::Operations {
                        load,
                        store: w::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipeline);
            if let Some(g) = &group {
                pass.set_bind_group(0, g, &[]);
            }
            pass.draw(0..vertices, 0..instances);
        }
        d.queue.submit([encoder.finish()]);
        Ok(())
    })?;
    if let Some(f) = frame {
        checked(&d.gpu, || {
            f.present();
            Ok(())
        })?;
    }
    Ok(0)
}

pub fn read_rgba(d: &mut Device, id: u64, out: &mut Vec<u8>) -> R<()> {
    d.check_pending()?;
    let t = d.texture(id)?;
    if t.format() != w::TextureFormat::Rgba8Unorm {
        return Err("readback requires RGBA8".into());
    }
    let row = t.width().checked_mul(4).ok_or("row too large")?;
    let pitch = row
        .checked_next_multiple_of(w::COPY_BYTES_PER_ROW_ALIGNMENT)
        .ok_or("row too large")?;
    let size = u64::from(pitch) * u64::from(t.height());
    let buffer = checked(&d.gpu, || {
        Ok(d.gpu.create_buffer(&w::BufferDescriptor {
            label: None,
            size,
            usage: w::BufferUsages::COPY_DST | w::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }))
    })?;
    checked(&d.gpu, || {
        let mut enc = d.gpu.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            t.as_image_copy(),
            w::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: w::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(pitch),
                    rows_per_image: None,
                },
            },
            t.size(),
        );
        d.queue.submit([enc.finish()]);
        Ok(())
    })?;
    let slice = buffer.slice(..);
    let (tx, rx) = mpsc::channel();
    checked(&d.gpu, || {
        slice.map_async(w::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        Ok(())
    })?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        checked(&d.gpu, || {
            d.gpu.poll(w::PollType::Poll).map_err(|e| e.to_string())
        })?;
        match rx.recv_timeout(std::time::Duration::from_millis(2)) {
            Ok(result) => {
                result.map_err(|e| e.to_string())?;
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("readback callback disconnected".into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) if std::time::Instant::now() >= deadline => {
                return Err("GPU readback timed out after 10 seconds".into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    let pixels = checked(&d.gpu, || {
        let mapped = slice.get_mapped_range();
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(
                usize::try_from(u64::from(row) * u64::from(t.height()))
                    .map_err(|_| "image too large")?,
            )
            .map_err(|e| e.to_string())?;
        for line in mapped.chunks_exact(pitch as usize) {
            pixels.extend_from_slice(&line[..row as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(pixels)
    })?;
    *out = pixels;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_format_keeps_linear_output() {
        use w::TextureFormat::*;
        assert_eq!(linear_format(&[Bgra8UnormSrgb, Bgra8Unorm]), Ok(Bgra8Unorm));
        assert!(linear_format(&[Rgba8UnormSrgb]).is_err());
    }

    #[test]
    fn uncaptured_error_reaches_next_call() -> R<()> {
        let mut d = open()?;
        let invalid = d.gpu.create_buffer(&w::BufferDescriptor {
            label: Some("uncaptured-probe"),
            size: 4,
            usage: w::BufferUsages::empty(),
            mapped_at_creation: false,
        });
        drop(invalid);
        let result = make_buffer(&mut d, 4, true);
        assert!(result.is_err_and(|e| e.contains("uncaptured-probe")));
        make_buffer(&mut d, 4, true)?;
        Ok(())
    }
}
