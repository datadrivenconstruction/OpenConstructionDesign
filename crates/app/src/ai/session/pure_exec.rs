// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A `Core` on the caller's thread: tool calls without a worker, a provider or a window. The
//! contract tests run the same call here, over the wire and in the dialog and compare results.

use super::{open_core, CallCx, CancelToken, Conversation, Core, SessionCfg, SourceCopy, ToolOutcome, ViewCtx, WorkerInit};
use crate::ai::plan::CompiledPlan;
use crate::ai::AiError;
use ddc_ai::Mode;
use serde_json::Value as J;
use std::path::Path;
use std::sync::{mpsc, Arc, OnceLock};

pub struct PureRun {
    core: Core,
    head: u64,
    // the core sends to these until it is dropped; the conversation is not used
    _conv: Conversation,
    _from: mpsc::Receiver<super::FromWorker>,
    _to: mpsc::Sender<super::ToWorker>,
}

impl PureRun {
    /// A copy of the project file `base` at change head `head` (`Model::change_head`).
    pub fn open(cfg: SessionCfg, base: &Path, head: u64, view: ViewCtx) -> Result<PureRun, AiError> {
        let (to, rx) = mpsc::channel();
        let (tx, from) = mpsc::channel();
        let init = WorkerInit {
            cfg,
            source: SourceCopy::File(base.to_path_buf()),
            head,
            view,
            rx,
            tx,
            cancel: CancelToken::new(),
            stop: CancelToken::new(),
            wake: Arc::new(OnceLock::new()),
        };
        let (core, conv) = open_core(init)?;
        Ok(PureRun {
            core,
            head,
            _conv: conv,
            _from: from,
            _to: to,
        })
    }

    /// One catalog call as the chat makes it: a write becomes an item on the scratch copy.
    pub fn call(&mut self, name: &str, args: &J) -> ToolOutcome {
        let cancel = CancelToken::new();
        let cx = CallCx {
            mode: Mode::Propose,
            cancel: &cancel,
            actor: "pure",
            lang: self.core.lang,
        };
        self.core.call_tool(name, args, &cx, false)
    }

    /// The steps of every item so far, as the card's Apply would run them.
    pub fn compiled(&self) -> CompiledPlan {
        self.core.plan.compiled(self.head)
    }

    pub fn items(&self) -> usize {
        self.core.plan.items.len()
    }
}
