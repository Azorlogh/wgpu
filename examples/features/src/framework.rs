use std::sync::Arc;

use wgpu::{InstanceDescriptor, Surface, SurfaceConfiguration};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{KeyEvent, WindowEvent},
    event_loop::{EventLoop, EventLoopProxy},
    keyboard::{Key, NamedKey},
    window::Window,
};

pub trait Example: 'static + Sized {
    const SRGB: bool = true;

    fn optional_features() -> wgpu::Features {
        wgpu::Features::empty()
    }

    fn required_features() -> wgpu::Features {
        wgpu::Features::empty()
    }

    fn required_downlevel_capabilities() -> wgpu::DownlevelCapabilities {
        wgpu::DownlevelCapabilities {
            flags: wgpu::DownlevelFlags::empty(),
            shader_model: wgpu::ShaderModel::Sm5,
            ..wgpu::DownlevelCapabilities::default()
        }
    }

    fn required_limits() -> wgpu::Limits {
        wgpu::Limits::downlevel_webgl2_defaults() // These downlevel limits will allow the code to run on all possible hardware
    }

    fn init(
        config: &wgpu::SurfaceConfiguration,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Self;

    fn resize(
        &mut self,
        config: &wgpu::SurfaceConfiguration,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    );

    fn update(&mut self, event: WindowEvent);

    fn render(&mut self, view: &wgpu::TextureView, device: &wgpu::Device, queue: &wgpu::Queue);
}

// Initialize logging in platform dependant ways.
fn init_logger() {
    cfg_if::cfg_if! {
        if #[cfg(target_arch = "wasm32")] {
            // As we don't have an environment to pull logging level from, we use the query string.
            let query_string = web_sys::window().unwrap().location().search().unwrap();
            let query_level: Option<log::LevelFilter> = parse_url_query_string(&query_string, "RUST_LOG")
                .and_then(|x| x.parse().ok());

            // We keep wgpu at Error level, as it's very noisy.
            let base_level = query_level.unwrap_or(log::LevelFilter::Info);
            let wgpu_level = query_level.unwrap_or(log::LevelFilter::Error);

            // On web, we use fern, as console_log doesn't have filtering on a per-module level.
            fern::Dispatch::new()
                .level(base_level)
                .level_for("wgpu_core", wgpu_level)
                .level_for("wgpu_hal", wgpu_level)
                .level_for("naga", wgpu_level)
                .chain(fern::Output::call(console_log::log))
                .apply()
                .unwrap();
            std::panic::set_hook(Box::new(console_error_panic_hook::hook));
        } else {
            // parse_default_env will read the RUST_LOG environment variable and apply it on top
            // of these default filters.
            env_logger::builder()
                .filter_level(log::LevelFilter::Info)
                // We keep wgpu at Error level, as it's very noisy.
                .filter_module("wgpu_core", log::LevelFilter::Info)
                .filter_module("wgpu_hal", log::LevelFilter::Error)
                .filter_module("naga", log::LevelFilter::Error)
                .parse_default_env()
                .init();
        }
    }
}

struct FrameCounter {
    // Instant of the last time we printed the frame time.
    last_printed_instant: web_time::Instant,
    // Number of frames since the last time we printed the frame time.
    frame_count: u32,
}

impl FrameCounter {
    fn new() -> Self {
        Self {
            last_printed_instant: web_time::Instant::now(),
            frame_count: 0,
        }
    }

    fn update(&mut self) {
        self.frame_count += 1;
        let new_instant = web_time::Instant::now();
        let elapsed_secs = (new_instant - self.last_printed_instant).as_secs_f32();
        if elapsed_secs > 1.0 {
            let elapsed_ms = elapsed_secs * 1000.0;
            let frame_time = elapsed_ms / self.frame_count as f32;
            let fps = self.frame_count as f32 / elapsed_secs;
            log::info!("Frame time {:.2}ms ({:.1} FPS)", frame_time, fps);

            self.last_printed_instant = new_instant;
            self.frame_count = 0;
        }
    }
}

enum FrameworkEvent {
    InitializedWgpu {
        surface: Surface<'static>,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    },
}

pub struct App<E: Example> {
    instance: wgpu::Instance,
    title: String,
    example: Option<E>,
    window: Option<Arc<Window>>,
    state: AppState,
    event_loop_proxy: EventLoopProxy<FrameworkEvent>,
    frame_counter: FrameCounter,
}

impl<E: Example> App<E> {
    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        match &mut self.state {
            AppState::Running {
                device,
                surface,
                surface_cfg,
                queue,
                ..
            } => {
                log::info!("Surface resize {size:?}");

                surface_cfg.width = size.width.max(1);
                surface_cfg.height = size.height.max(1);
                surface.configure(&device, surface_cfg);

                self.example
                    .as_mut()
                    .unwrap()
                    .resize(surface_cfg, &device, &queue);
                self.window.as_ref().unwrap().request_redraw();
            }
            _ => {}
        }
    }
}

fn acquire_frame(
    surface: &Surface,
    surface_cfg: &SurfaceConfiguration,
    device: &wgpu::Device,
) -> wgpu::SurfaceTexture {
    match surface.get_current_texture() {
        Ok(frame) => frame,
        // If we timed out, just try again
        Err(wgpu::SurfaceError::Timeout) => surface
            .get_current_texture()
            .expect("Failed to acquire next surface texture!"),
        Err(
            // If the surface is outdated, or was lost, reconfigure it.
            wgpu::SurfaceError::Outdated
            | wgpu::SurfaceError::Lost
            | wgpu::SurfaceError::Other
            // If OutOfMemory happens, reconfiguring may not help, but we might as well try
            | wgpu::SurfaceError::OutOfMemory,
        ) => {
            surface.configure(&device, surface_cfg);
            surface
                .get_current_texture()
                .expect("Failed to acquire next surface texture!")
        }
    }
}

#[derive(Default)]
enum AppState {
    #[default]
    Init,
    WgpuInitialized {
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    },
    Running {
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface: Surface<'static>,
        surface_cfg: SurfaceConfiguration,
    },
}

async fn init_wgpu<E: Example>(
    instance: wgpu::Instance,
    window: Arc<Window>,
    event_loop_proxy: EventLoopProxy<FrameworkEvent>,
) {
    let surface = instance.create_surface(window).unwrap();
    let adapter = get_adapter_with_capabilities_or_from_env(
        &instance,
        &E::required_features(),
        &E::required_downlevel_capabilities(),
        Some(&surface),
    )
    .await;
    // Make sure we use the texture resolution limits from the adapter, so we can support images the size of the surface.
    let needed_limits = E::required_limits().using_resolution(adapter.limits());

    let trace_dir = std::env::var("WGPU_TRACE");
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                required_features: (E::optional_features() & adapter.features())
                    | E::required_features(),
                required_limits: needed_limits,
                memory_hints: wgpu::MemoryHints::MemoryUsage,
            },
            trace_dir.ok().as_ref().map(std::path::Path::new),
        )
        .await
        .expect("Unable to find a suitable GPU adapter!");
    event_loop_proxy
        .send_event(FrameworkEvent::InitializedWgpu {
            surface,
            adapter,
            device,
            queue,
        })
        .ok();
}

fn configure_surface(
    surface: &Surface<'static>,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    window: Arc<Window>,
    srgb: bool,
) -> wgpu::SurfaceConfiguration {
    // Window size is only actually valid after we enter the event loop.
    let window_size = window.inner_size();
    let width = window_size.width.max(1);
    let height = window_size.height.max(1);

    log::info!("Surface resume {window_size:?}");

    // Get the default configuration,
    let mut config = surface
        .get_default_config(adapter, width, height)
        .expect("Surface isn't supported by the adapter.");
    if srgb {
        // Not all platforms (WebGPU) support sRGB swapchains, so we need to use view formats
        let view_format = config.format.add_srgb_suffix();
        config.view_formats.push(view_format);
    } else {
        // All platforms support non-sRGB swapchains, so we can just use the format directly.
        let format = config.format.remove_srgb_suffix();
        config.format = format;
        config.view_formats.push(format);
    };

    surface.configure(device, &config);
    config
}

impl<E: Example> ApplicationHandler<FrameworkEvent> for App<E> {
    fn user_event(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        event: FrameworkEvent,
    ) {
        match event {
            FrameworkEvent::InitializedWgpu {
                mut surface,
                adapter,
                device,
                queue,
            } => {
                let surface_cfg = configure_surface(
                    &mut surface,
                    &adapter,
                    &device,
                    self.window.clone().unwrap(),
                    E::SRGB,
                );
                self.example = Some(E::init(&surface_cfg, &adapter, &device, &queue));
                self.state = AppState::Running {
                    adapter,
                    device,
                    queue,
                    surface,
                    surface_cfg,
                };
            }
        }
    }

    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let mut attributes = Window::default_attributes();
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsCast;
            use winit::platform::web::WindowAttributesExtWebSys;
            let canvas = web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .get_element_by_id("canvas")
                .unwrap()
                .dyn_into::<web_sys::HtmlCanvasElement>()
                .unwrap();
            attributes = attributes.with_canvas(Some(canvas));
        }
        attributes = attributes.with_title(&self.title);

        let window = Arc::new(event_loop.create_window(attributes).unwrap());

        let instance = self.instance.clone();
        let event_loop_proxy = self.event_loop_proxy.clone();
        self.state = match std::mem::take(&mut self.state) {
            AppState::Init => {
                let wgpu_init = init_wgpu::<E>(instance, window.clone(), event_loop_proxy);
                cfg_if::cfg_if! {
                    if #[cfg(target_arch = "wasm32")] {
                        wasm_bindgen_futures::spawn_local(wgpu_init);
                    } else {
                        pollster::block_on(wgpu_init);
                    }
                }
                AppState::Init
            }
            AppState::WgpuInitialized {
                adapter,
                device,
                queue,
            }
            | AppState::Running {
                adapter,
                device,
                queue,
                ..
            } => {
                let surface = instance.create_surface(window.clone()).unwrap();
                let surface_cfg =
                    configure_surface(&surface, &adapter, &device, window.clone(), E::SRGB);
                AppState::Running {
                    adapter,
                    device,
                    queue,
                    surface,
                    surface_cfg,
                }
            }
        };

        self.window = Some(window);
    }

    fn suspended(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        self.state = match std::mem::take(&mut self.state) {
            AppState::Running {
                adapter,
                device,
                queue,
                ..
            }
            | AppState::WgpuInitialized {
                adapter,
                device,
                queue,
            } => AppState::WgpuInitialized {
                adapter,
                device,
                queue,
            },
            s => s,
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        ..
                    },
                ..
            }
            | WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                self.resize(size);
            }
            #[cfg(not(target_arch = "wasm32"))]
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Character(s),
                        ..
                    },
                ..
            } if s == "r" => {
                println!("{:#?}", self.instance.generate_report());
            }
            WindowEvent::RedrawRequested => {
                if let AppState::Running {
                    surface,
                    surface_cfg,
                    device,
                    queue,
                    ..
                } = &mut self.state
                {
                    // On MacOS, currently redraw requested comes in _before_ Init does.
                    // If this happens, just drop the requested redraw on the floor.
                    //
                    // See https://github.com/rust-windowing/winit/issues/3235 for some discussion
                    if self.example.is_none() {
                        return;
                    }

                    self.frame_counter.update();

                    let frame = acquire_frame(surface, &surface_cfg, &device);
                    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
                        format: Some(surface_cfg.view_formats[0]),
                        ..wgpu::TextureViewDescriptor::default()
                    });

                    self.example
                        .as_mut()
                        .unwrap()
                        .render(&view, &device, &queue);

                    self.window.as_ref().unwrap().pre_present_notify();
                    frame.present();

                    self.window.as_ref().unwrap().request_redraw();
                }
            }
            _ => {
                if let Some(example) = &mut self.example {
                    example.update(event);
                }
            }
        }
    }
}

async fn start<E: Example>(title: &str) {
    init_logger();

    log::debug!(
        "Enabled backends: {:?}",
        wgpu::Instance::enabled_backend_features()
    );

    let event_loop = EventLoop::<FrameworkEvent>::with_user_event()
        .build()
        .unwrap();
    let frame_counter = FrameCounter::new();

    let mut app = App::<E> {
        instance: wgpu::Instance::new(&InstanceDescriptor::from_env_or_default()),
        title: title.to_owned(),
        example: None,
        state: AppState::Init,
        window: None,
        frame_counter,
        event_loop_proxy: event_loop.create_proxy(),
    };
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn_app(app);
    }
    #[cfg(not(target_arch = "wasm32"))]
    event_loop.run_app(&mut app).unwrap();
}

pub fn run<E: Example>(title: &'static str) {
    cfg_if::cfg_if! {
        if #[cfg(target_arch = "wasm32")] {
            wasm_bindgen_futures::spawn_local(async move { start::<E>(title).await })
        } else {
            pollster::block_on(start::<E>(title));
        }
    }
}

#[cfg(target_arch = "wasm32")]
/// Parse the query string as returned by `web_sys::window()?.location().search()?` and get a
/// specific key out of it.
pub fn parse_url_query_string<'a>(query: &'a str, search_key: &str) -> Option<&'a str> {
    let query_string = query.strip_prefix('?')?;

    for pair in query_string.split('&') {
        let mut pair = pair.split('=');
        let key = pair.next()?;
        let value = pair.next()?;

        if key == search_key {
            return Some(value);
        }
    }

    None
}

#[cfg(test)]
pub use wgpu_test::image::ComparisonType;

use crate::utils::get_adapter_with_capabilities_or_from_env;

#[cfg(test)]
#[derive(Clone)]
pub struct ExampleTestParams<E> {
    pub name: &'static str,
    // Path to the reference image, relative to the root of the repo.
    pub image_path: &'static str,
    pub width: u32,
    pub height: u32,
    pub optional_features: wgpu::Features,
    pub base_test_parameters: wgpu_test::TestParameters,
    /// Comparisons against FLIP statistics that determine if the test passes or fails.
    pub comparisons: &'static [ComparisonType],
    pub _phantom: std::marker::PhantomData<E>,
}

#[cfg(test)]
impl<E: Example + wgpu::WasmNotSendSync> From<ExampleTestParams<E>>
    for wgpu_test::GpuTestConfiguration
{
    fn from(params: ExampleTestParams<E>) -> Self {
        wgpu_test::GpuTestConfiguration::new()
            .name(params.name)
            .parameters({
                assert_eq!(params.width % 64, 0, "width needs to be aligned 64");

                let features = E::required_features() | params.optional_features;

                params
                    .base_test_parameters
                    .clone()
                    .features(features)
                    .limits(E::required_limits())
            })
            .run_async(move |ctx| async move {
                let format = if E::SRGB {
                    wgpu::TextureFormat::Rgba8UnormSrgb
                } else {
                    wgpu::TextureFormat::Rgba8Unorm
                };
                let dst_texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("destination"),
                    size: wgpu::Extent3d {
                        width: params.width,
                        height: params.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });

                let dst_view = dst_texture.create_view(&wgpu::TextureViewDescriptor::default());

                let dst_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("image map buffer"),
                    size: params.width as u64 * params.height as u64 * 4,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });

                let mut example = E::init(
                    &wgpu::SurfaceConfiguration {
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        format,
                        width: params.width,
                        height: params.height,
                        desired_maximum_frame_latency: 2,
                        present_mode: wgpu::PresentMode::Fifo,
                        alpha_mode: wgpu::CompositeAlphaMode::Auto,
                        view_formats: vec![format],
                    },
                    &ctx.adapter,
                    &ctx.device,
                    &ctx.queue,
                );

                example.render(&dst_view, &ctx.device, &ctx.queue);

                let mut cmd_buf = ctx
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());

                cmd_buf.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: &dst_texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &dst_buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(params.width * 4),
                            rows_per_image: None,
                        },
                    },
                    wgpu::Extent3d {
                        width: params.width,
                        height: params.height,
                        depth_or_array_layers: 1,
                    },
                );

                ctx.queue.submit(Some(cmd_buf.finish()));

                let dst_buffer_slice = dst_buffer.slice(..);
                dst_buffer_slice.map_async(wgpu::MapMode::Read, |_| ());
                ctx.async_poll(wgpu::PollType::wait()).await.unwrap();
                let bytes = dst_buffer_slice.get_mapped_range().to_vec();

                wgpu_test::image::compare_image_output(
                    dbg!(env!("CARGO_MANIFEST_DIR").to_string() + "/../../" + params.image_path),
                    &ctx.adapter_info,
                    params.width,
                    params.height,
                    &bytes,
                    params.comparisons,
                )
                .await;
            })
    }
}
