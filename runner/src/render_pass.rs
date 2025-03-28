use crate::{
    bind_group_buffer::{BindGroupBufferType, BufferDescriptor, SSBO},
    context::GraphicsContext,
    controller::Controller,
    ui::{Ui, UiState},
};
use egui_winit::winit::window::Window;
use shared::push_constants::shader::*;
use wgpu::util::DeviceExt;

struct Pipelines {
    render: wgpu::RenderPipeline,
    compute: wgpu::ComputePipeline,
}

struct PipelineLayouts {
    render: wgpu::PipelineLayout,
    compute: wgpu::PipelineLayout,
}

struct BindGroupData {
    #[cfg(feature = "emulate_constants")]
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

pub struct RenderPass {
    pipelines: Pipelines,
    #[cfg(not(target_arch = "wasm32"))]
    pipeline_layouts: PipelineLayouts,
    ui_renderer: egui_wgpu::Renderer,
    bind_group_data: Vec<BindGroupData>,
}

impl RenderPass {
    pub fn new(
        ctx: &GraphicsContext,
        #[cfg(not(target_arch = "wasm32"))] shader_path: &std::path::Path,
        buffer_data: &[BufferDescriptor],
    ) -> Self {
        let bind_group_layouts = create_bind_group_layouts(ctx, buffer_data);
        let pipeline_layouts = create_pipeline_layouts(ctx, &bind_group_layouts);
        let pipelines = create_pipelines(
            &ctx.device,
            &pipeline_layouts,
            ctx.config.format,
            #[cfg(not(target_arch = "wasm32"))]
            shader_path,
        );
        let bind_group_data = maybe_create_bind_groups(ctx, buffer_data, &bind_group_layouts);

        let ui_renderer = egui_wgpu::Renderer::new(&ctx.device, ctx.config.format, None, 1, false);

        Self {
            pipelines,
            #[cfg(not(target_arch = "wasm32"))]
            pipeline_layouts,
            ui_renderer,
            bind_group_data,
        }
    }

    pub fn compute(&mut self, ctx: &GraphicsContext, controller: &Controller) {
        let workspace = {
            use glam::*;
            const COMPUTE_THREADS: UVec2 = uvec2(16, 16);
            let dim = controller.compute_dimensions();
            (dim.as_vec2() / COMPUTE_THREADS.as_vec2())
                .ceil()
                .as_uvec2()
                .extend(1)
        };
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });

            cpass.set_pipeline(&self.pipelines.compute);
            #[cfg(not(feature = "emulate_constants"))]
            cpass.set_push_constants(0, controller.compute_constants());
            #[cfg(feature = "emulate_constants")]
            ctx.queue.write_buffer(
                &self.bind_group_data.last().unwrap().buffer,
                0,
                controller.compute_constants(),
            );
            for (i, bind_group_data) in self.bind_group_data.iter().enumerate() {
                cpass.set_bind_group(i as u32, &bind_group_data.bind_group, &[]);
            }
            cpass.dispatch_workgroups(workspace.x, workspace.y, workspace.z);
        }
        ctx.queue.submit(Some(encoder.finish()));
    }

    pub fn render(
        &mut self,
        ctx: &GraphicsContext,
        window: &Window,
        ui: &mut Ui,
        ui_state: &mut UiState,
        controller: &mut Controller,
    ) -> Result<(), wgpu::SurfaceError> {
        let output = match ctx.surface.get_current_texture() {
            Ok(surface_texture) => surface_texture,
            Err(err) => {
                eprintln!("get_current_texture error: {err:?}");
                return match err {
                    wgpu::SurfaceError::Lost => {
                        ctx.surface.configure(&ctx.device, &ctx.config);
                        Ok(())
                    }
                    _ => Err(err),
                };
            }
        };
        let output_view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        if controller.size.x > 0 && controller.size.y > 0 {
            self.render_shader(ctx, &output_view, controller);
        }
        self.render_ui(ctx, &output_view, window, ui, ui_state, controller);

        output.present();

        Ok(())
    }

    fn render_shader(
        &mut self,
        ctx: &GraphicsContext,
        output_view: &wgpu::TextureView,
        controller: &Controller,
    ) {
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Shader Encoder"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Shader Render Pass"),
                occlusion_query_set: None,
                timestamp_writes: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::GREEN),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
            });
            {
                let scale_factor = controller.scale_factor;
                let size = controller.size.as_vec2();
                rpass.set_viewport(
                    0.0,
                    (shared::UI_MENU_HEIGHT as f64 * scale_factor) as f32,
                    size.x,
                    size.y,
                    0.0,
                    1.0,
                );
            }

            rpass.set_pipeline(&self.pipelines.render);
            #[cfg(not(feature = "emulate_constants"))]
            rpass.set_push_constants(
                wgpu::ShaderStages::FRAGMENT,
                0,
                controller.fragment_constants(),
            );
            #[cfg(feature = "emulate_constants")]
            ctx.queue.write_buffer(
                &self.bind_group_data[self.bind_group_data.len() - 2].buffer,
                0,
                controller.fragment_constants(),
            );
            for (i, bind_group_data) in self.bind_group_data.iter().enumerate() {
                rpass.set_bind_group(i as u32, &bind_group_data.bind_group, &[]);
            }
            rpass.draw(0..3, 0..1);
        }

        ctx.queue.submit(Some(encoder.finish()));
    }

    fn render_ui(
        &mut self,
        ctx: &GraphicsContext,
        output_view: &wgpu::TextureView,
        window: &Window,
        ui: &mut Ui,
        ui_state: &mut UiState,
        controller: &mut Controller,
    ) {
        let (clipped_primitives, textures_delta) = ui.prepare(window, ui_state, controller);

        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [ctx.config.width, ctx.config.height],
            pixels_per_point: ui.pixels_per_point(),
        };

        for (id, delta) in &textures_delta.set {
            self.ui_renderer
                .update_texture(&ctx.device, &ctx.queue, *id, delta);
        }

        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("UI Encoder"),
            });

        self.ui_renderer.update_buffers(
            &ctx.device,
            &ctx.queue,
            &mut encoder,
            &clipped_primitives,
            &screen_descriptor,
        );

        {
            let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("UI Render Pass"),
                occlusion_query_set: None,
                timestamp_writes: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
            });

            for id in &textures_delta.free {
                self.ui_renderer.free_texture(id);
            }

            self.ui_renderer.render(
                &mut rpass.forget_lifetime(),
                &clipped_primitives,
                &screen_descriptor,
            );
        }

        ctx.queue.submit(Some(encoder.finish()));
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_module(&mut self, ctx: &GraphicsContext, shader_path: &std::path::Path) {
        self.pipelines = create_pipelines(
            &ctx.device,
            &self.pipeline_layouts,
            ctx.config.format,
            shader_path,
        );
    }
}

fn maybe_create_bind_groups(
    ctx: &GraphicsContext,
    buffer_descriptors: &[BufferDescriptor],
    bind_group_layouts: &[wgpu::BindGroupLayout],
) -> Vec<BindGroupData> {
    let bind_group_data = buffer_descriptors
        .iter()
        .zip(bind_group_layouts)
        .enumerate()
        .map(|(i, (descriptor, layout))| {
            let buffer = ctx
                .device
                .create_buffer_init(&match &descriptor.buffer_type {
                    BindGroupBufferType::SSBO(ssbo) => wgpu::util::BufferInitDescriptor {
                        label: Some("Bind Group Buffer"),
                        contents: ssbo.data,
                        usage: wgpu::BufferUsages::STORAGE,
                    },
                    BindGroupBufferType::Uniform(uniform) => wgpu::util::BufferInitDescriptor {
                        label: Some("Bind Group Buffer"),
                        contents: uniform.data,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    },
                });
            BindGroupData {
                bind_group: ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                    label: Some(&format!("bind_group {}", i)),
                }),
                #[cfg(feature = "emulate_constants")]
                buffer,
            }
        });
    #[cfg(feature = "emulate_constants")]
    let bind_group_data = {
        let usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        bind_group_data.chain([
            {
                let buffer = ctx
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: None,
                        contents: &[0; std::mem::size_of::<FragmentConstants>()],
                        usage,
                    });
                BindGroupData {
                    bind_group: ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        layout: &bind_group_layouts[bind_group_layouts.len() - 2],
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buffer.as_entire_binding(),
                        }],
                        label: Some("emulated fragment constants bind group"),
                    }),
                    buffer,
                }
            },
            {
                let buffer = ctx
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: None,
                        contents: &[0; std::mem::size_of::<ComputeConstants>()],
                        usage,
                    });
                BindGroupData {
                    bind_group: ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        layout: &bind_group_layouts[bind_group_layouts.len() - 1],
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buffer.as_entire_binding(),
                        }],
                        label: Some("emulated compute constants bind group"),
                    }),
                    buffer,
                }
            },
        ])
    };
    bind_group_data.collect()
}

fn create_pipelines(
    device: &wgpu::Device,
    pipeline_layouts: &PipelineLayouts,
    surface_format: wgpu::TextureFormat,
    #[cfg(not(target_arch = "wasm32"))] shader_path: &std::path::Path,
) -> Pipelines {
    cfg_if::cfg_if! {
        if #[cfg(target_arch = "wasm32")] {
            let data = include_bytes!(env!("shader.spv"));
        } else {
            let data = &std::fs::read(shader_path).unwrap();
        }
    }
    let spirv = wgpu::util::make_spirv_raw(data);
    let module = &device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::SpirV(std::borrow::Cow::Borrowed(&spirv)),
    });
    let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&pipeline_layouts.render),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("main_vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("main_fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview: None,
        cache: None,
    });
    let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: Some(&pipeline_layouts.compute),
        module,
        entry_point: Some("main_cs"),
        compilation_options: Default::default(),
        cache: None,
    });
    Pipelines {
        render: render_pipeline,
        compute: compute_pipeline,
    }
}

fn create_bind_group_layouts(
    ctx: &GraphicsContext,
    buffer_descriptors: &[BufferDescriptor],
) -> Vec<wgpu::BindGroupLayout> {
    let layouts = buffer_descriptors
        .iter()
        .enumerate()
        .map(|(i, descriptor)| {
            ctx.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: descriptor.shader_stages,
                        ty: wgpu::BindingType::Buffer {
                            ty: (match descriptor.buffer_type {
                                BindGroupBufferType::Uniform(_) => wgpu::BufferBindingType::Uniform,
                                BindGroupBufferType::SSBO(SSBO { read_only, .. }) => {
                                    wgpu::BufferBindingType::Storage { read_only }
                                }
                            }),
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                    label: Some(&format!("bind_group_layout {}", i)),
                })
        });
    #[cfg(feature = "emulate_constants")]
    let layouts = {
        layouts.chain([
            ctx.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                    label: Some("emulated fragment constants layout"),
                }),
            ctx.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                    label: Some("emulated compute constants layout"),
                }),
        ])
    };
    layouts.collect()
}

fn create_pipeline_layouts(
    ctx: &GraphicsContext,
    bind_group_layouts: &[wgpu::BindGroupLayout],
) -> PipelineLayouts {
    let bind_group_layouts = &bind_group_layouts.iter().collect::<Vec<_>>();
    let create = |push_constant_ranges| {
        ctx.device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts,
                push_constant_ranges,
            })
    };
    PipelineLayouts {
        render: create(&[
            #[cfg(not(feature = "emulate_constants"))]
            wgpu::PushConstantRange {
                stages: wgpu::ShaderStages::FRAGMENT,
                range: 0..std::mem::size_of::<FragmentConstants>() as u32,
            },
        ]),
        compute: create(&[
            #[cfg(not(feature = "emulate_constants"))]
            wgpu::PushConstantRange {
                stages: wgpu::ShaderStages::COMPUTE,
                range: 0..std::mem::size_of::<ComputeConstants>() as u32,
            },
        ]),
    }
}
