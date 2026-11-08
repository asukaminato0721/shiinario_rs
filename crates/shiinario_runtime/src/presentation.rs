//! Upload the engine's logical RGBA canvas; preserve its aspect ratio on screen.
use crate::{resources::Surface, viewport::ViewportTransform};
use anyhow::{Context, Result, ensure};
use std::sync::Arc;
use winit::{dpi::PhysicalSize, window::Window};

pub struct Presentation {
    /// Kept to recreate `surface` after `CurrentSurfaceTexture::Lost`.
    instance: wgpu::Instance,
    window: Arc<dyn Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    logical: [u32; 2],
    size: PhysicalSize<u32>,
}
impl Presentation {
    pub async fn new(window: Arc<dyn Window>, logical: [u32; 2]) -> Result<Self> {
        let size = window.surface_size();
        let descriptor =
            wgpu::InstanceDescriptor::new_with_display_handle(Box::new(window.clone()));
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            #[cfg(target_os = "windows")]
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::DX12),
            #[cfg(not(target_os = "windows"))]
            backends: wgpu::Backends::from_env().unwrap_or_default(),
            ..descriptor
        });
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .context("no compatible graphics adapter")?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Shiina Rio presentation"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await?;
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .context("no compatible window surface configuration")?;
        let caps = surface.get_capabilities(&adapter);
        config.format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .context("window surface has no sRGB format")?;
        config.present_mode = wgpu::PresentMode::Fifo;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        surface.configure(&device, &config);
        if let Some(error) = scope.pop().await {
            anyhow::bail!("configure presentation surface: {error}");
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("logical canvas"),
            size: wgpu::Extent3d {
                width: logical[0],
                height: logical[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let view = texture.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("canvas.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Ok(Self {
            instance,
            window,
            surface,
            device,
            queue,
            config,
            texture,
            bind_group,
            pipeline,
            logical,
            size,
        })
    }
    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        self.size = size;
        if size.width > 0 && size.height > 0 {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }
    pub fn transform(&self) -> Result<ViewportTransform> {
        ViewportTransform::fit(
            self.logical,
            [self.size.width.max(1), self.size.height.max(1)],
        )
    }
    /// False means presentation was deferred (minimized or temporary surface loss).
    pub fn draw(&mut self, canvas: &Surface) -> Result<bool> {
        if self.size.width == 0 || self.size.height == 0 {
            return Ok(false);
        }
        ensure!(
            [canvas.width, canvas.height] == self.logical,
            "presentation canvas dimensions changed"
        );
        ensure!(
            canvas.rgba.len() == canvas.width as usize * canvas.height as usize * 4,
            "invalid presentation pixels"
        );
        // A surface cannot be reconfigured while one of its frames is held, so a
        // suboptimal frame is presented first and the surface reconfigured after.
        let (frame, freshness) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, Freshness::Current),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, Freshness::Stale),
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self
                    .instance
                    .create_surface(self.window.clone())
                    .context("recreate lost presentation surface")?;
                self.surface.configure(&self.device, &self.config);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                anyhow::bail!("acquire presentation surface texture: validation error")
            }
        };
        self.queue.write_texture(
            self.texture.as_image_copy(),
            &canvas.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(canvas.width * 4),
                rows_per_image: Some(canvas.height),
            },
            self.texture.size(),
        );
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let rect = self.transform()?.to_window_rect([
            0,
            0,
            self.logical[0] as i32,
            self.logical[1] as i32,
        ])?;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_viewport(
                rect[0] as f32,
                rect[1] as f32,
                (rect[2] - rect[0]) as f32,
                (rect[3] - rect[1]) as f32,
                0.0,
                1.0,
            );
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        match freshness {
            Freshness::Current => {}
            Freshness::Stale => self.surface.configure(&self.device, &self.config),
        }
        Ok(true)
    }
}

/// Whether an acquired frame still matches the surface's current properties.
enum Freshness {
    Current,
    Stale,
}
