/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use compositing::CompositorChan;
use layout::layout_task::LayoutTask;

use geom::size::Size2D;
use gfx::render_task::{PaintPermissionGranted, PaintPermissionRevoked};
use gfx::render_task::{RenderChan, RenderTask};
use script::layout_interface::LayoutChan;
use script::script_task::LoadMsg;
use script::script_task::{AttachLayoutMsg, NewLayoutInfo, ScriptTask, ScriptChan};
use script::script_task;
use servo_msg::constellation_msg::{ConstellationChan, Failure, PipelineId, SubpageId};
use servo_net::image_cache_task::ImageCacheTask;
use servo_net::resource_task::ResourceTask;
use servo_util::opts::Opts;
use servo_util::time::ProfilerChan;
use std::rc::Rc;
use url::Url;

/// A uniquely-identifiable pipeline of script task, layout task, and render task.
pub struct Pipeline {
    pub id: PipelineId,
    pub subpage_id: Option<SubpageId>,
    pub script_chan: ScriptChan,
    pub layout_chan: LayoutChan,
    pub render_chan: RenderChan,
    pub layout_shutdown_port: Receiver<()>,
    pub render_shutdown_port: Receiver<()>,
    /// The most recently loaded url
    pub url: Url,
}

/// The subset of the pipeline that is needed for layer composition.
#[deriving(Clone)]
pub struct CompositionPipeline {
    pub id: PipelineId,
    pub script_chan: ScriptChan,
    pub render_chan: RenderChan,
}

impl Pipeline {
    /// Starts a render task, layout task, and script task. Returns the channels wrapped in a
    /// struct.
    pub fn with_script(id: PipelineId,
                       subpage_id: SubpageId,
                       constellation_chan: ConstellationChan,
                       compositor_chan: CompositorChan,
                       image_cache_task: ImageCacheTask,
                       profiler_chan: ProfilerChan,
                       opts: Opts,
                       script_pipeline: Rc<Pipeline>,
                       url: Url)
                       -> Pipeline {
        let (layout_port, layout_chan) = LayoutChan::new();
        let (render_port, render_chan) = RenderChan::new();
        let (render_shutdown_chan, render_shutdown_port) = channel();
        let (layout_shutdown_chan, layout_shutdown_port) = channel();

        let failure = Failure {
            pipeline_id: id,
            subpage_id: Some(subpage_id),
        };

        RenderTask::create(id,
                           render_port,
                           compositor_chan.clone(),
                           constellation_chan.clone(),
                           failure.clone(),
                           opts.clone(),
                           profiler_chan.clone(),
                           render_shutdown_chan);

        LayoutTask::create(id,
                           layout_port,
                           layout_chan.clone(),
                           constellation_chan,
                           failure,
                           script_pipeline.script_chan.clone(),
                           render_chan.clone(),
                           image_cache_task.clone(),
                           opts.clone(),
                           profiler_chan,
                           layout_shutdown_chan);

        let new_layout_info = NewLayoutInfo {
            old_pipeline_id: script_pipeline.id.clone(),
            new_pipeline_id: id,
            subpage_id: subpage_id,
            layout_chan: layout_chan.clone(),
        };

        let ScriptChan(ref chan) = script_pipeline.script_chan;
        chan.send(AttachLayoutMsg(new_layout_info));

        Pipeline::new(id,
                      Some(subpage_id),
                      script_pipeline.script_chan.clone(),
                      layout_chan,
                      render_chan,
                      layout_shutdown_port,
                      render_shutdown_port,
                      url)
    }

    pub fn create(id: PipelineId,
                  subpage_id: Option<SubpageId>,
                  constellation_chan: ConstellationChan,
                  compositor_chan: CompositorChan,
                  image_cache_task: ImageCacheTask,
                  resource_task: ResourceTask,
                  profiler_chan: ProfilerChan,
                  window_size: Size2D<uint>,
                  opts: Opts,
                  url: Url)
                  -> Pipeline {
        let (script_port, script_chan) = ScriptChan::new();
        let (layout_port, layout_chan) = LayoutChan::new();
        let (render_port, render_chan) = RenderChan::new();
        let (render_shutdown_chan, render_shutdown_port) = channel();
        let (layout_shutdown_chan, layout_shutdown_port) = channel();
        let pipeline = Pipeline::new(id,
                                     subpage_id,
                                     script_chan.clone(),
                                     layout_chan.clone(),
                                     render_chan.clone(),
                                     layout_shutdown_port,
                                     render_shutdown_port,
                                     url);

        let failure = Failure {
            pipeline_id: id,
            subpage_id: subpage_id,
        };

        ScriptTask::create(id,
                           ~compositor_chan.clone(),
                           layout_chan.clone(),
                           script_port,
                           script_chan.clone(),
                           constellation_chan.clone(),
                           failure.clone(),
                           resource_task,
                           image_cache_task.clone(),
                           window_size);

        RenderTask::create(id,
                           render_port,
                           compositor_chan.clone(),
                           constellation_chan.clone(),
                           failure.clone(),
                           opts.clone(),
                           profiler_chan.clone(),
                           render_shutdown_chan);

        LayoutTask::create(id,
                           layout_port,
                           layout_chan.clone(),
                           constellation_chan,
                           failure,
                           script_chan.clone(),
                           render_chan.clone(),
                           image_cache_task,
                           opts.clone(),
                           profiler_chan,
                           layout_shutdown_chan);

        pipeline
    }

    pub fn new(id: PipelineId,
               subpage_id: Option<SubpageId>,
               script_chan: ScriptChan,
               layout_chan: LayoutChan,
               render_chan: RenderChan,
               layout_shutdown_port: Receiver<()>,
               render_shutdown_port: Receiver<()>,
               url: Url)
               -> Pipeline {
        Pipeline {
            id: id,
            subpage_id: subpage_id,
            script_chan: script_chan,
            layout_chan: layout_chan,
            render_chan: render_chan,
            layout_shutdown_port: layout_shutdown_port,
            render_shutdown_port: render_shutdown_port,
            url: url,
        }
    }

    pub fn load(&self) {
        let ScriptChan(ref chan) = self.script_chan;
        chan.send(LoadMsg(self.id, self.url.clone()));
    }

    pub fn grant_paint_permission(&self) {
        self.render_chan.chan.try_send(PaintPermissionGranted);
    }

    pub fn revoke_paint_permission(&self) {
        debug!("pipeline revoking render channel paint permission");
        self.render_chan.chan.try_send(PaintPermissionRevoked);
    }

    pub fn exit(&self) {
        debug!("pipeline {:?} exiting", self.id);

        // Script task handles shutting down layout, and layout handles shutting down the renderer.
        // For now, if the script task has failed, we give up on clean shutdown.
        let ScriptChan(ref chan) = self.script_chan;
        if chan.try_send(script_task::ExitPipelineMsg(self.id)) {
            // Wait until all slave tasks have terminated and run destructors
            // NOTE: We don't wait for script task as we don't always own it
            self.render_shutdown_port.recv_opt();
            self.layout_shutdown_port.recv_opt();
        }
    }

    pub fn to_sendable(&self) -> CompositionPipeline {
        CompositionPipeline {
            id: self.id.clone(),
            script_chan: self.script_chan.clone(),
            render_chan: self.render_chan.clone(),
        }
    }
}

