// backend.rs — GPU and output initialisation

use std::{
    os::unix::io::{FromRawFd, IntoRawFd},
    time::Instant,
};

use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::DrmCompositor, exporter::gbm::GbmFramebufferExporter, DrmDevice,
            DrmDeviceFd, DrmEvent, DrmNode,
        },
        egl::{EGLContext, EGLDisplay},
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer, Bind, ImportMemWl},
        session::Session,
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        drm::control::{connector, crtc, Device as DrmControlDevice, ModeTypeFlags},
        wayland_server::DisplayHandle,
    },
    utils::{DeviceFd, Size, Transform},
    wayland::output::OutputManagerState,
};

use crate::state::{BackendData, KittyCompositor, SurfaceData};

// Load the gl crate's function pointer table.
// Called once per EGL context creation in add_gpu().
use gl;

pub fn add_gpu(
    state: &mut KittyCompositor,
    dh: &DisplayHandle,
    node: DrmNode,
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let owned_fd = state.session.open(
        path,
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOCTTY,
    )?;
    let drm_fd = DrmDeviceFd::new(unsafe { DeviceFd::from_raw_fd(owned_fd.into_raw_fd()) });

    let (drm, drm_notifier) = DrmDevice::new(drm_fd.clone(), true)?;
    let gbm = GbmDevice::new(drm_fd.clone())?;
    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let ctx = EGLContext::new(&egl)?;
    let renderer = unsafe { GlesRenderer::new(ctx)? };

    if let Err(e) = egl.bind_wl_display(dh) {
        tracing::warn!("EGL bind_wl_display failed (hw-accel unavailable): {e}");
    }

    // ── Load GL function pointers ─────────────────────────────────────────
    // The `gl` crate requires explicit loading after the EGL context is
    // current. GlesRenderer::new() makes it current, so we load here.
    // Safe to call multiple times — just overwrites pointers with same values.
    gl::load_with(|s| {
        let sym = std::ffi::CString::new(s).unwrap();
        unsafe { smithay::backend::egl::ffi::egl::GetProcAddress(sym.as_ptr()) as *const _ }
    });

    // ── PixelUI init ──────────────────────────────────────────────────────
    // Must happen while this EGL context is current (i.e. right here).
    // The guard prevents double-init on multi-GPU systems — cell_size()
    // returns the default (8,16) until install_renderer() is called.
    if crate::pixelui::overlay_element::cell_size() == (8, 16) {
        tracing::info!(
            "init_pixel_ui: starting, font path = {:?}",
            state.config.font.path
        );

        // Explicitly make the EGL context current before any raw GL calls.
        // GlesRenderer::new() leaves the context current, but Smithay may
        // unbind it internally after construction. Force it current here.
        if let Err(e) = unsafe { renderer.egl_context().make_current() } {
            tracing::warn!("init_pixel_ui: could not make EGL context current: {e}");
        } else {
            init_pixel_ui(state);
        }

        tracing::info!(
            "init_pixel_ui: returned, installed={}",
            crate::pixelui::overlay_element::is_installed()
        );
    }

    state
        .handle
        .insert_source(drm_notifier, move |event, _, state| {
            if let DrmEvent::VBlank(crtc) = event {
                state.frame_finish(node, crtc);
            }
        })
        .unwrap();

    let mut backend = BackendData {
        surfaces: Default::default(),
        renderer,
        gbm,
        drm,
        drm_node: node,
    };

    let res_handles = backend.drm.resource_handles()?;
    let connectors: Vec<_> = res_handles
        .connectors()
        .iter()
        .filter_map(|&h| backend.drm.get_connector(h, false).ok())
        .filter(|c| c.state() == connector::State::Connected)
        .collect();

    for connector in connectors {
        let mode = connector
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or_else(|| connector.modes().first())
            .copied();
        let Some(mode) = mode else { continue };
        let crtc = res_handles
            .crtcs()
            .iter()
            .copied()
            .find(|&c| !backend.surfaces.contains_key(&c));
        let Some(crtc) = crtc else { continue };
        if let Err(e) = add_output(
            state,
            dh,
            &mut backend,
            node,
            connector.handle(),
            crtc,
            mode,
        ) {
            tracing::warn!("Failed to set up connector: {e}");
        }
    }

    state.backends.insert(node, backend);
    Ok(())
}

// ── PixelUI / UiRenderer initialisation ──────────────────────────────────────

fn init_pixel_ui(state: &KittyCompositor) {
    tracing::info!("init_pixel_ui entered");
    use crate::font::GlyphAtlas;
    use crate::pixelui::overlay_element;
    use crate::pixelui::UiRenderer;
    use crate::shaper::Shaper;

    let font_cfg = &state.config.font;
    tracing::info!("Reading font from {:?}", font_cfg.path);

    let regular_bytes: &'static [u8] = match std::fs::read(&font_cfg.path) {
        Ok(b) => Box::leak(b.into_boxed_slice()),
        Err(e) => {
            tracing::warn!(
                "PixelUI: could not read font {:?}: {e} — TWM chrome disabled",
                font_cfg.path
            );
            return;
        }
    };

    let bold_bytes: Option<&'static [u8]> =
        font_cfg
            .bold_path
            .as_ref()
            .and_then(|p| match std::fs::read(p) {
                Ok(b) => Some(Box::leak(b.into_boxed_slice()) as &'static [u8]),
                Err(e) => {
                    tracing::warn!("PixelUI: could not read bold font {p:?}: {e}");
                    None
                }
            });

    let italic_bytes: Option<&'static [u8]> =
        font_cfg
            .italic_path
            .as_ref()
            .and_then(|p| match std::fs::read(p) {
                Ok(b) => Some(Box::leak(b.into_boxed_slice()) as &'static [u8]),
                Err(e) => {
                    tracing::warn!("PixelUI: could not read italic font {p:?}: {e}");
                    None
                }
            });

    let atlas = match GlyphAtlas::new(
        regular_bytes,
        bold_bytes,
        italic_bytes,
        font_cfg.size,
        font_cfg.line_spacing.unwrap_or(1.1),
        font_cfg.dpi.unwrap_or(96),
    ) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("PixelUI: GlyphAtlas::new failed: {e} — TWM chrome disabled");
            return;
        }
    };
    tracing::info!("GlyphAtlas created ok");
    let shaper = Shaper::new(regular_bytes);
    tracing::info!("Shaper created ok");

    let current = unsafe { smithay::backend::egl::ffi::egl::GetCurrentContext() };
    tracing::info!("EGL current context before UiRenderer::new: {:?}", current);

    let ui_renderer = match UiRenderer::new(atlas, shaper, 0, 0) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("PixelUI: UiRenderer::new failed: {e} — TWM chrome disabled");
            return;
        }
    };

    overlay_element::install_renderer(ui_renderer);
    tracing::info!(
        "PixelUI installed — cell {}×{}px",
        overlay_element::cell_size().0,
        overlay_element::cell_size().1,
    );
}

// ── add_output ────────────────────────────────────────────────────────────────

pub fn add_output(
    state: &mut KittyCompositor,
    dh: &DisplayHandle,
    backend: &mut BackendData,
    node: DrmNode,
    connector: connector::Handle,
    crtc: crtc::Handle,
    drm_mode: smithay::reexports::drm::control::Mode,
) -> Result<(), Box<dyn std::error::Error>> {
    let wl_mode = Mode {
        size: (drm_mode.size().0 as i32, drm_mode.size().1 as i32).into(),
        refresh: drm_mode.vrefresh() as i32 * 1000,
    };

    let connector_hz = drm_mode.vrefresh() as u64;
    let frame_duration = state.config.frame_duration_for(connector_hz);

    let output = Output::new(
        format!("{node}-{crtc:?}"),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "KittyWM".into(),
            model: "DRM".into(),
        },
    );
    let _global = output.create_global::<KittyCompositor>(dh);
    output.change_current_state(
        Some(wl_mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(wl_mode);
    state.space.map_output(&output, (0, 0));

    // Now we know the real pixel size — update the overlay viewport so NDC
    // projection is correct for this output.
    let ow = drm_mode.size().0 as u32;
    let oh = drm_mode.size().1 as u32;
    crate::pixelui::overlay_element::set_viewport(ow, oh);

    let compositor = DrmCompositor::new(
        &output,
        backend.drm.create_surface(crtc, drm_mode, &[connector])?,
        None,
        GbmAllocator::new(
            backend.gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        ),
        GbmFramebufferExporter::new(backend.gbm.clone(), Some(node)),
        [Fourcc::Argb8888, Fourcc::Xrgb8888].iter().copied(),
        backend
            .renderer
            .egl_context()
            .dmabuf_render_formats()
            .clone(),
        Size::<u32, smithay::utils::Buffer>::from((64, 64)),
        None::<GbmDevice<DrmDeviceFd>>,
    )?;

    tracing::info!(
        "Output {node}-{crtc:?}: {}x{}@{}Hz → frame duration {:.2}ms (target_hz={:?})",
        drm_mode.size().0,
        drm_mode.size().1,
        connector_hz,
        frame_duration.as_secs_f64() * 1000.0,
        state.config.target_hz,
    );

    backend.surfaces.insert(
        crtc,
        SurfaceData {
            output: output.clone(),
            compositor,
            damage_tracker: OutputDamageTracker::from_output(&output),
            next_frame_time: Instant::now() + frame_duration,
            pending_frame: false,
            frame_duration,
        },
    );

    state
        .handle
        .insert_source(Timer::from_duration(frame_duration), move |_, _, state| {
            state.render_surface(node, crtc);
            TimeoutAction::ToDuration(frame_duration)
        })
        .ok();

    Ok(())
}
