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
