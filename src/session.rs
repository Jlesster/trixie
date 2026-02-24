// session.rs — VT pause/resume handling

use smithay::backend::session::{libseat::LibSeatSession, Event as SessionEvent, Session};
use smithay::reexports::calloop::LoopHandle;

use crate::state::KittyCompositor;

pub fn register_session_handler(
    handle: &LoopHandle<'static, KittyCompositor>,
    notifier: smithay::backend::session::libseat::LibSeatSessionNotifier,
) {
    handle
        .insert_source(notifier, |event, _, state| match event {
            SessionEvent::PauseSession => on_pause(state),
            SessionEvent::ActivateSession => on_resume(state),
        })
        .unwrap();
}

// ── pause ─────────────────────────────────────────────────────────────────────

fn on_pause(state: &mut KittyCompositor) {
    tracing::info!("Session paused — suspending input and DRM");

    // Suspend input first so no events arrive while DRM is paused.
    state.libinput.suspend();

    for (node, backend) in &mut state.backends {
        tracing::debug!("Pausing DRM node {node}");
        backend.drm.pause();

        // Mark every surface as not having a pending frame so the render
        // timer doesn't try to submit to a paused device on the next tick.
        for sd in backend.surfaces.values_mut() {
            sd.pending_frame = false;
        }
    }
}

// ── resume ────────────────────────────────────────────────────────────────────

fn on_resume(state: &mut KittyCompositor) {
    tracing::info!("Session resumed — reactivating DRM and input");

    // 1. Reactivate every DRM device first, collecting which nodes succeeded.
    //    We must do this before re-enabling input so libinput events don't
    //    race an unready DRM surface.
    let mut ready_nodes = Vec::new();
    for (node, backend) in &mut state.backends {
        match backend.drm.activate(false) {
            Ok(()) => {
                tracing::debug!("DRM node {node} reactivated");
                ready_nodes.push(*node);
            }
            Err(e) => {
                tracing::error!("Failed to reactivate DRM node {node}: {e}");
            }
        }
    }

    // 2. Phase-lock the next_frame_time for every surface on ready nodes so
    //    the per-output timers don't fire a burst of catch-up frames.
    //    Without this the timers can fire immediately because next_frame_time
    //    is in the past after a long VT switch.
    let now = std::time::Instant::now();
    for node in &ready_nodes {
        if let Some(backend) = state.backends.get_mut(node) {
            for sd in backend.surfaces.values_mut() {
                // Push next_frame_time forward so the first frame fires one
                // full interval from now, giving the display pipeline time
                // to settle after reactivation.
                sd.next_frame_time = now + sd.frame_duration;
            }
        }
    }

    // 3. Re-enable input only after DRM is ready.
    if let Err(e) = state
        .libinput
        .udev_assign_seat(state.session.seat().as_str())
    {
        tracing::error!("Failed to reassign libinput seat on resume: {e:?}");
    }

    // 4. Queue a single render pass to redraw all ready surfaces.
    //    insert_idle runs after the current event-loop tick completes, so
    //    it will see the updated next_frame_time values above.
    if !ready_nodes.is_empty() {
        state.handle.insert_idle(|state| state.render_all());
    }

    tracing::info!(
        "Session resume complete ({} node(s) ready)",
        ready_nodes.len()
    );
}
